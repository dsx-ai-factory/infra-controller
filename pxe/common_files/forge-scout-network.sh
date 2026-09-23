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
networkd_runtime_dir=${SCOUT_NETWORKD_RUNTIME_DIR:-/run/systemd/network}
networkd_dhcp_file=${SCOUT_NETWORKD_DHCP_FILE:-/etc/systemd/network/dhcp.network}
network_wait_seconds=${SCOUT_NETWORK_WAIT_SECONDS:-120}
network_poll_interval=${SCOUT_NETWORK_POLL_INTERVAL:-1}

uptime_seconds() {
	uptime_value=
	if read -r uptime_value _ < /proc/uptime; then
		printf '%s\n' "${uptime_value%%.*}"
	else
		printf '0\n'
	fi
}

script_result=failed
script_exit_reason=unexpected_exit
script_start_seconds=$(uptime_seconds)

report_exit() {
	exit_status=$?
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

if ! cmdline=$(cat "$cmdline_file"); then
	echo "Skipping Scout network configuration: reason=cmdline_unreadable" >&2
	finish skipped cmdline_unreadable 0
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
set +f

if [ "$mac_parameter_count" -ne 1 ]; then
	echo "Skipping Scout network configuration: reason=mac_parameter_count count=$mac_parameter_count" >&2
	finish skipped mac_parameter_count 0
fi

case "$preferred_mac" in
	[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f]:[0-9A-Fa-f][0-9A-Fa-f])
		;;
	*)
		echo "Skipping Scout network configuration: reason=invalid_mac_parameter" >&2
		finish skipped invalid_mac_parameter 0
		;;
esac
preferred_mac=$(printf '%s\n' "$preferred_mac" | tr '[:upper:]' '[:lower:]')

probe_attempts=0
probe_max_attempts=$(((network_wait_seconds + network_poll_interval - 1) / network_poll_interval + 1))

preferred_interface=
while :
do
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
		finish skipped preferred_interface_count 0
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
		elif ! global_addresses=$("$ip_command" -o address show dev "$preferred_interface" scope global); then
			not_ready=address_inspection_failed
		elif [ -z "$global_addresses" ]; then
			not_ready=no_global_address
		fi
	fi

	[ -z "$not_ready" ] && break

	probe_attempts=$((probe_attempts + 1))
	if [ "$probe_attempts" -ge "$probe_max_attempts" ]; then
		echo "Scout network configuration timed out: interface=${preferred_interface:-<none>} mac=$preferred_mac reason=$not_ready waited=${network_wait_seconds}s" >&2
		finish failed preferred_interface_timeout 1
	fi
	sleep "$network_poll_interval"
done

runtime_network_file="$networkd_runtime_dir/00-forge-scout-nonpreferred.network"

# Build the target set from the configuration networkd selected for each link.
# Name= also matches alternative interface names, so matching only the current
# interface name here could miss links that the wildcard configuration affects.
set --
for net_path in "$sys_class_net"/*
do
	[ -e "$net_path" ] || continue

	interface=${net_path##*/}
	[ "$interface" = "$preferred_interface" ] && continue

	if ! network_status=$(
		"$networkctl_command" --json=short status "$interface" 2>/dev/null
	); then
		# A link can disappear while interfaces are being enumerated.
		[ -e "$net_path" ] || continue

		echo "Skipping Scout network configuration: interface=$interface reason=network_status_inspection_failed" >&2
		finish skipped network_status_inspection_failed 0
	fi

	case "$network_status" in
		*"\"NetworkFile\":\"$networkd_dhcp_file\""*|*"\"NetworkFile\":\"$runtime_network_file\""*)
			set -- "$@" "$interface"
			;;
	esac
done

if ! mkdir -p "$networkd_runtime_dir"; then
	echo "Failed to create networkd runtime directory: directory=$networkd_runtime_dir" >&2
	finish failed runtime_directory_create_failed 1
fi

if ! temporary_network_file=$(mktemp "$networkd_runtime_dir/.forge-scout-network.XXXXXX"); then
	echo "Failed to create temporary networkd configuration: directory=$networkd_runtime_dir" >&2
	finish failed temporary_file_create_failed 1
fi

if ! {
	printf '%s\n' \
		'[Match]' \
		'Name=enx* enp* enP*' \
		"Property=!INTERFACE=$preferred_interface" \
		'' \
		'[Link]' \
		'RequiredForOnline=no' \
		'' \
		'[Network]' \
		'DHCP=no' \
		'IPv6AcceptRA=no' \
		'ConfigureWithoutCarrier=yes'
} >"$temporary_network_file"; then
	echo "Failed to write networkd configuration: path=$temporary_network_file" >&2
	rm -f "$temporary_network_file"
	finish failed network_configuration_write_failed 1
fi

if ! chmod 0644 "$temporary_network_file" ||
	! mv -f "$temporary_network_file" "$runtime_network_file"; then
	echo "Failed to install networkd configuration: path=$runtime_network_file" >&2
	rm -f "$temporary_network_file"
	finish failed network_configuration_install_failed 1
fi

if ! "$networkctl_command" reload; then
	echo "Failed to reload networkd configuration" >&2
	finish failed networkd_reload_failed 1
fi

echo "Selected preferred network interface: interface=$preferred_interface mac=$preferred_mac"
failed=false
failure_reason=unknown_failure

wait_attempts=$probe_max_attempts
wait_complete=false
wait_failed=false
while [ "$#" -gt 0 ] && [ "$wait_attempts" -gt 0 ]
do
	all_interfaces_complete=true
	incomplete_interfaces=
	for interface
	do
		# A removed interface is safe; if it reappears, networkd will apply the
		# persistent wildcard configuration.
		if [ ! -e "$sys_class_net/$interface" ]; then
			continue
		fi

		incomplete_reason=
		incomplete_detail=
		if ! network_status=$(
			"$networkctl_command" --json=short status "$interface" 2>/dev/null
		); then
			incomplete_reason=network_status_inspection_failed
		else
			case "$network_status" in
				*"\"NetworkFile\":\"$runtime_network_file\""*)
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
						if ! global_addresses=$(
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
		if [ "$wait_attempts" -eq 1 ]; then
			echo "Network interface still incomplete: interface=$interface reason=$incomplete_reason${incomplete_detail:+ $incomplete_detail}" >&2
		fi
	done

	if [ "$wait_failed" = true ]; then
		break
	fi
	if [ "$all_interfaces_complete" = true ]; then
		wait_complete=true
		break
	fi

	wait_attempts=$((wait_attempts - 1))
	if [ "$wait_attempts" -gt 0 ]; then
		sleep "$network_poll_interval"
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
