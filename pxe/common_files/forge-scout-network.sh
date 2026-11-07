#!/usr/bin/env sh
#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
set -u
set -f

cmdline_file=${SCOUT_CMDLINE_FILE:-/proc/cmdline}
sys_class_net=${SCOUT_SYS_CLASS_NET:-/sys/class/net}
ip_command=${SCOUT_IP_COMMAND:-ip}
networkctl_command=${SCOUT_NETWORKCTL_COMMAND:-networkctl}
systemctl_command=${SCOUT_SYSTEMCTL_COMMAND:-systemctl}
networkd_runtime_dir=${SCOUT_NETWORKD_RUNTIME_DIR:-/run/systemd/network}
networkd_config_dir=${SCOUT_NETWORKD_CONFIG_DIR:-/etc/systemd/network}
wait_online_command=${SCOUT_WAIT_ONLINE_COMMAND:-/usr/lib/systemd/systemd-networkd-wait-online}
networkd_dhcp_file=${SCOUT_NETWORKD_DHCP_FILE:-/etc/systemd/network/dhcp.network}
network_wait_seconds=${SCOUT_NETWORK_WAIT_SECONDS:-120}
network_poll_interval=${SCOUT_NETWORK_POLL_INTERVAL:-1}

mode=${1:-configure}
nextroot=${2:-}
preferred_network_file="$networkd_runtime_dir/00-forge-scout-preferred.network"
runtime_network_file="$networkd_runtime_dir/01-forge-scout-nonpreferred.network"
legacy_runtime_file="$networkd_runtime_dir/00-forge-scout-nonpreferred.network"
legacy_network_file="$networkd_config_dir/00-forge-scout-nonpreferred.network"
temporary_network_file=
prepare_cleanup_armed=false

uptime_seconds() {
	uptime_value=
	if read -r uptime_value _ < /proc/uptime; then
		printf '%s\n' "${uptime_value%%.*}"
	else
		printf '0\n'
	fi
}

sleep_before_deadline() {
	deadline_seconds=$1
	current_seconds=$(uptime_seconds)
	remaining_seconds=$((deadline_seconds - current_seconds))

	if [ "$remaining_seconds" -le 0 ]; then
		return 1
	fi

	sleep_seconds=$network_poll_interval
	if [ "$sleep_seconds" -gt "$remaining_seconds" ]; then
		sleep_seconds=$remaining_seconds
	fi
	sleep "$sleep_seconds"
}

script_result=failed
script_exit_reason=unexpected_exit
script_start_seconds=$(uptime_seconds)

report_exit() {
	exit_status=$?
	trap - HUP INT TERM
	[ -z "$temporary_network_file" ] || rm -f "$temporary_network_file"
	if [ "$exit_status" -ne 0 ] && [ "$prepare_cleanup_armed" = true ]; then
		remove_policy || exit_status=1
	fi
	current_seconds=$(uptime_seconds)
	duration_seconds=unknown

	if [ "$current_seconds" -ge "$script_start_seconds" ]; then
		duration_seconds=$((current_seconds - script_start_seconds))
	fi

	printf '%s\n' \
		"Scout network configuration finished: result=$script_result reason=$script_exit_reason exit_status=$exit_status duration_seconds=$duration_seconds"

	trap - 0
	exit "$exit_status"
}

finish() {
	script_result=$1
	script_exit_reason=$2
	exit "$3"
}

trap report_exit 0
trap 'finish failed interrupted 1' HUP INT TERM

remove_runtime_policy() {
	rm -f "$preferred_network_file" "$runtime_network_file" "$legacy_runtime_file"
}

remove_policy() {
	remove_runtime_policy && rm -f "$legacy_network_file"
}

restore_broad_networking() {
	if [ "$mode" = wait-online ]; then
		# Root systemctl uses PID 1's private socket before the system bus starts.
		remove_policy && "$systemctl_command" reload systemd-networkd.service
		return
	fi
	set +f
	# Remember the links using our configuration before removing it. Matching
	# networkd's selection also includes alternative interface names.
	set --
	recovery_failed=false
	for net_path in "$sys_class_net"/*
	do
		[ -d "$net_path" ] || continue
		interface=${net_path##*/}
		if ! network_status=$("$networkctl_command" --json=short status "$interface"); then
			[ -e "$net_path" ] || continue
			echo "Failed to inspect interface during Scout network recovery: interface=$interface" >&2
			recovery_failed=true
			continue
		fi
		case "$network_status" in
			*"\"NetworkFile\":\"$preferred_network_file\""*|*"\"NetworkFile\":\"$runtime_network_file\""*|*"\"NetworkFile\":\"$legacy_runtime_file\""*|*"\"NetworkFile\":\"$legacy_network_file\""*)
				set -- "$@" "$interface"
				;;
		esac
	done
	if [ "$#" -eq 0 ] && [ ! -e "$preferred_network_file" ] &&
		[ ! -e "$runtime_network_file" ] && [ ! -e "$legacy_runtime_file" ] &&
		[ ! -e "$legacy_network_file" ]; then
		[ "$recovery_failed" = false ]
		return
	fi
	if ! remove_policy || ! "$networkctl_command" reload; then
		echo "Failed to restore Scout's broad network configuration" >&2
		return 1
	fi
	for interface
	do
		if ! "$networkctl_command" reconfigure "$interface"; then
			echo "Failed to restore DHCP during Scout network recovery: interface=$interface" >&2
			recovery_failed=true
		fi
	done
	[ "$recovery_failed" = false ]
}

use_broad_networking() {
	fallback_result=$1
	fallback_reason=$2
	fallback_status=$3
	if ! restore_broad_networking; then
		finish failed network_recovery_failed 1
	fi
	if [ "$mode" = wait-online ]; then
		# Recovery needs one routable link, even if the selected NIC stays down.
		if "$wait_online_command" --any --operational-state=routable --timeout="$network_wait_seconds"; then
			finish fallback "$fallback_reason" 0
		fi
		finish failed fallback_wait_online_failed 1
	fi
	finish "$fallback_result" "$fallback_reason" "$fallback_status"
}

write_network_file() {
	network_file=$1
	if ! mkdir -p "${network_file%/*}" ||
		! temporary_network_file=$(mktemp "${network_file%/*}/.forge-scout-network.XXXXXX"); then
		return 1
	fi
	if ! cat >"$temporary_network_file" || ! chmod 0644 "$temporary_network_file"; then
		return 1
	fi
	# A repeated readiness check must not restart DHCP on the selected link.
	if cmp -s "$temporary_network_file" "$network_file"; then
		rm -f "$temporary_network_file" || return 1
	else
		mv -f "$temporary_network_file" "$network_file" || return 1
	fi
	temporary_network_file=
}

install_policy() {
	# networkd uses the first matching file. Install the MAC allow rule before
	# the data-NIC exclusion so an interrupted write cannot exclude every NIC.
	write_network_file "$preferred_network_file" <<EOF || return 1
[Match]
MACAddress=$preferred_mac

[Network]
DHCP=yes

[DHCP]
ClientIdentifier=mac
EOF
	write_network_file "$runtime_network_file" <<'EOF'
[Match]
Name=enx* enp* enP*

[Link]
RequiredForOnline=no

[Network]
DHCP=no
IPv6AcceptRA=no
ConfigureWithoutCarrier=yes
EOF
}

case "$mode:$#" in
	prepare:1|configure:0|wait-online:1) ;;
	handoff:2)
		if [ ! -d "$nextroot" ]; then
			finish failed nextroot_missing 1
		fi
		;;
	*) finish failed invalid_arguments 1 ;;
esac

validate_positive_integer() {
	value=$1
	name=$2

	case "$value" in
		''|*[!0-9]*|0|0*)
			echo "Invalid Scout network timing value: variable=$name value=$value reason=positive_integer_required" >&2
			finish failed invalid_timing_value 1
			;;
	esac
}

validate_positive_integer "$network_wait_seconds" SCOUT_NETWORK_WAIT_SECONDS
validate_positive_integer "$network_poll_interval" SCOUT_NETWORK_POLL_INTERVAL

# The loader's /run survives a soft reboot. Remove only Scout-owned policy;
# a name-based rule copied into /etc by an older loader must not override it.
if [ "$mode" = prepare ]; then
	prepare_cleanup_armed=true
	remove_policy || finish failed inherited_policy_remove_failed 1
fi

read_selection() {
	selection_reason=
	if ! cmdline=$(cat "$cmdline_file"); then
		echo "Skipping Scout network configuration: reason=cmdline_unreadable" >&2
		selection_reason=cmdline_unreadable
		return 1
	fi

	preferred_mac=
	mac_parameter_count=0
	for parameter in $cmdline
	do
		case "$parameter" in
			mac=*)
				mac_parameter_count=$((mac_parameter_count + 1))
				preferred_mac=${parameter#mac=}
				;;
		esac
	done
	if [ "$mac_parameter_count" -ne 1 ]; then
		echo "Skipping Scout network configuration: reason=mac_parameter_count count=$mac_parameter_count" >&2
		selection_reason=mac_parameter_count
		return 1
	fi

	case "$preferred_mac" in
		[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]) ;;
		*)
			echo "Skipping Scout network configuration: reason=invalid_mac_parameter" >&2
			selection_reason=invalid_mac_parameter
			return 1
			;;
	esac
	preferred_mac=$(printf '%s\n' "$preferred_mac" | tr '[:upper:]' '[:lower:]')
}

read_selection
set +f

probe_preferred_interface() {
	preferred_interface=
	preferred_interface_count=0
	for net_path in "$sys_class_net"/*
	do
		[ -e "$net_path" ] || continue
		[ -r "$net_path/address" ] || continue

		interface_mac=$(cat "$net_path/address") || continue
		interface_mac=$(printf '%s\n' "$interface_mac" | tr '[:upper:]' '[:lower:]')
		if [ "$interface_mac" = "$preferred_mac" ]; then
			preferred_interface=${net_path##*/}
			preferred_interface_count=$((preferred_interface_count + 1))
		fi
	done

	# More than one interface owning the same MAC is a configuration fault, not
	# a timing issue. Waiting cannot resolve it, so give up immediately.
	if [ "$preferred_interface_count" -gt 1 ]; then
		echo "Skipping Scout network configuration: reason=preferred_interface_count count=$preferred_interface_count" >&2
		not_ready=preferred_interface_count
		return 1
	fi

	not_ready=
	if [ "$preferred_interface_count" -ne 1 ]; then
		not_ready=no_interface
	else
		carrier_file="$sys_class_net/$preferred_interface/carrier"
		# Reading `carrier` fails with EINVAL while the link is
		# administratively down, so treat an unreadable value as no carrier yet.
		if [ ! -r "$carrier_file" ] || [ "$(cat "$carrier_file" 2>/dev/null)" != 1 ]; then
			not_ready=no_carrier
		# Tentative or DAD-failed IPv6 addresses do not establish readiness.
		elif ! global_addresses=$("$ip_command" -o address show dev "$preferred_interface" scope global -tentative -dadfailed); then
			not_ready=address_inspection_failed
		elif [ -z "$global_addresses" ]; then
			not_ready=no_global_address
		fi
	fi

	[ -z "$not_ready" ]
}

if [ "$mode" = handoff ]; then
	legacy_target="$nextroot/etc/systemd/network/00-forge-scout-nonpreferred.network"
	if [ -f "$nextroot/etc/systemd/system/systemd-network-generator.service.d/50-forge-scout-network.conf" ]; then
		# The new root regenerates policy from mac= before starting networking.
		rm -f "$legacy_target" || finish failed handoff_policy_remove_failed 1
		finish success handoff_preparation_supported 0
	fi

	if [ -n "$selection_reason" ] || ! probe_preferred_interface; then
		if [ "${not_ready:-}" = address_inspection_failed ]; then
			finish failed address_inspection_failed 1
		fi
		# An old root cannot regenerate the MAC rule. Let it bootstrap broadly
		# if the selected interface is not usable at the handoff.
		rm -f "$legacy_target" || finish failed handoff_policy_remove_failed 1
		remove_runtime_policy || finish failed handoff_runtime_remove_failed 1
		finish fallback handoff_without_usable_selection 0
	fi

	# Old Scout images need a self-contained rule, not a blanket exclusion
	# whose allow rule lives only in the loader's /run.
	write_network_file "$legacy_target" <<EOF || finish failed handoff_policy_write_failed 1
[Match]
Name=enx* enp* enP*
Property=!INTERFACE=$preferred_interface

[Link]
RequiredForOnline=no

[Network]
DHCP=no
IPv6AcceptRA=no
ConfigureWithoutCarrier=yes
EOF
	remove_runtime_policy || finish failed handoff_runtime_remove_failed 1
	finish success handoff_legacy_policy_installed 0
fi

if [ -n "$selection_reason" ]; then
	if [ "$mode" = prepare ]; then
		finish skipped "$selection_reason" 0
	fi
	use_broad_networking skipped "$selection_reason" 0
fi

if [ "$mode" = prepare ]; then
	install_policy || finish failed network_configuration_install_failed 1
	finish success prepared 0
fi

probe_start_seconds=$(uptime_seconds)
probe_deadline_seconds=$((probe_start_seconds + network_wait_seconds))
while ! probe_preferred_interface
do
	if [ "$not_ready" = preferred_interface_count ]; then
		use_broad_networking skipped preferred_interface_count 0
	fi
	if ! sleep_before_deadline "$probe_deadline_seconds"; then
		echo "Scout network configuration timed out: interface=${preferred_interface:-<none>} mac=$preferred_mac reason=$not_ready waited=${network_wait_seconds}s" >&2
		if [ "$not_ready" = address_inspection_failed ]; then
			finish failed address_inspection_failed 1
		fi
		use_broad_networking failed preferred_interface_timeout 1
	fi
done

if [ "$mode" = wait-online ]; then
	# Native wait observes networkd without requiring the system bus. Detailed
	# policy and competing-address checks run in the later configure call.
	"$wait_online_command" --timeout="$network_wait_seconds" || finish failed selected_wait_online_failed 1
	if ! probe_preferred_interface; then
		echo "Selected Scout interface lost readiness: reason=$not_ready" >&2
		if [ "$not_ready" = address_inspection_failed ]; then
			finish failed address_inspection_failed 1
		fi
		use_broad_networking failed preferred_interface_lost_readiness 1
	fi
	finish success completed 0
fi

# Build the target set from the configuration networkd selected for each link.
# Name= also matches alternative interface names, so matching only the current
# interface name here could miss links that the wildcard configuration affects.
set --
for net_path in "$sys_class_net"/*
do
	[ -d "$net_path" ] || continue

	interface=${net_path##*/}
	[ "$interface" = "$preferred_interface" ] && continue

	if ! network_status=$(
		"$networkctl_command" --json=short status "$interface"
	); then
		# A link can disappear while interfaces are being enumerated.
		[ -e "$net_path" ] || continue

		echo "Failed to inspect Scout network configuration: interface=$interface reason=network_status_inspection_failed" >&2
		finish failed network_status_inspection_failed 1
	fi

	case "$network_status" in
		*"\"NetworkFile\":\"$networkd_dhcp_file\""*|*"\"NetworkFile\":\"$runtime_network_file\""*|*"\"NetworkFile\":\"$legacy_runtime_file\""*|*"\"NetworkFile\":\"$legacy_network_file\""*)
			set -- "$@" "$interface"
			;;
	esac
done

if ! install_policy || ! rm -f "$legacy_runtime_file" "$legacy_network_file"; then
	finish failed network_configuration_install_failed 1
fi

if ! "$networkctl_command" reload; then
	echo "Failed to reload networkd configuration" >&2
	finish failed networkd_reload_failed 1
fi

echo "Selected preferred network interface: interface=$preferred_interface mac=$preferred_mac"
failed=false
failure_reason=unknown_failure

# Reload can restart DHCP when switching from broad bootstrap to the MAC rule.
# Verify the selected link again alongside withdrawal on the other links.
set -- "$preferred_interface" "$@"
reconfiguration_start_seconds=$(uptime_seconds)
reconfiguration_deadline_seconds=$((reconfiguration_start_seconds + network_wait_seconds))
wait_complete=false
wait_failed=false
while [ "$#" -gt 0 ]
do
	all_interfaces_complete=true
	incomplete_interfaces=
	incomplete_report=
	for interface
	do
		# A removed nonpreferred link is safe; if it reappears, networkd will
		# apply the wildcard configuration.
		if [ "$interface" != "$preferred_interface" ] && [ ! -e "$sys_class_net/$interface" ]; then
			continue
		fi

		expected_network_file=$runtime_network_file
		[ "$interface" != "$preferred_interface" ] || expected_network_file=$preferred_network_file
		incomplete_reason=
		incomplete_detail=
		if [ "$interface" = "$preferred_interface" ] && [ ! -e "$sys_class_net/$interface" ]; then
			incomplete_reason=no_interface
		elif ! network_status=$(
			"$networkctl_command" --json=short status "$interface"
		); then
			incomplete_reason=network_status_inspection_failed
		else
			case "$network_status" in
				*"\"NetworkFile\":\"$expected_network_file\""*)
					;;
				*)
					incomplete_reason=unexpected_network_file
					incomplete_detail="network_status=$network_status"
					;;
			esac

			if [ -z "$incomplete_reason" ]; then
				case "$network_status" in
					*'"AdministrativeState":"failed"'*)
						echo "Network interface reconfiguration failed: interface=$interface network_status=$network_status" >&2
						failed=true
						failure_reason=interface_reconfiguration_failed
						wait_failed=true
						break
						;;
					*'"AdministrativeState":"configured"'*)
						if [ "$interface" = "$preferred_interface" ]; then
							if [ "$(cat "$sys_class_net/$interface/carrier" 2>/dev/null)" != 1 ]; then
								incomplete_reason=no_carrier
							elif ! global_addresses=$("$ip_command" -o address show dev "$interface" scope global -tentative -dadfailed); then
								incomplete_reason=address_inspection_failed
							elif [ -n "$global_addresses" ]; then
								continue
							else
								incomplete_reason=no_global_address
							fi
						elif ! global_addresses=$(
							"$ip_command" -o address show dev "$interface" scope global
						); then
							incomplete_reason=address_inspection_failed
						elif ! ipv4_routes=$(
							"$ip_command" -o -4 route show dev "$interface" scope global
						); then
							incomplete_reason=ipv4_route_inspection_failed
						elif ! ipv6_routes=$(
							"$ip_command" -o -6 route show dev "$interface" scope global
						); then
							incomplete_reason=ipv6_route_inspection_failed
						elif ! non_link_local_ipv6_routes=$(
							printf '%s\n' "$ipv6_routes" |
								sed '/^[fF][eE][89aAbB][[:xdigit:]]:/d'
						); then
							incomplete_reason=ipv6_route_filter_failed
						elif [ -n "$global_addresses" ]; then
							incomplete_reason=global_addresses_remain
							incomplete_detail="global_addresses=$global_addresses"
						elif [ -n "$ipv4_routes" ]; then
							incomplete_reason=global_ipv4_routes_remain
							incomplete_detail="ipv4_routes=$ipv4_routes"
						elif [ -n "$non_link_local_ipv6_routes" ]; then
							incomplete_reason=global_ipv6_routes_remain
							incomplete_detail="ipv6_routes=$non_link_local_ipv6_routes"
						else
							continue
						fi
						;;
					*)
						incomplete_reason=administrative_state_not_configured
						incomplete_detail="network_status=$network_status"
						;;
				esac
			fi
		fi

		all_interfaces_complete=false
		incomplete_interfaces="${incomplete_interfaces}${incomplete_interfaces:+ }$interface"
		incomplete_message="Network interface still incomplete: interface=$interface reason=$incomplete_reason${incomplete_detail:+ $incomplete_detail}"
		if [ -n "$incomplete_report" ]; then
			incomplete_report="$incomplete_report
$incomplete_message"
		else
			incomplete_report=$incomplete_message
		fi
	done

	if [ "$wait_failed" = true ]; then
		break
	fi
	if [ "$all_interfaces_complete" = true ]; then
		wait_complete=true
		break
	fi

	if ! sleep_before_deadline "$reconfiguration_deadline_seconds"; then
		[ -z "$incomplete_report" ] || printf '%s\n' "$incomplete_report" >&2
		if ! probe_preferred_interface && [ "$not_ready" != address_inspection_failed ]; then
			echo "Selected Scout interface unavailable after reconfiguration: reason=$not_ready" >&2
			use_broad_networking failed preferred_interface_timeout 1
		fi
		break
	fi
done

if [ "$#" -gt 0 ] && [ "$wait_complete" = false ] && [ "$wait_failed" = false ]; then
	echo "Timed out waiting for network interface reconfiguration: interfaces=${incomplete_interfaces:-unknown} waited=${network_wait_seconds}s" >&2
	failed=true
	failure_reason=interface_reconfiguration_timeout
fi

if [ "$failed" = true ]; then
	finish failed "$failure_reason" 1
fi

finish success completed 0
