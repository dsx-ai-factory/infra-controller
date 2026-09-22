#!/bin/bash
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
file=
root_dev=
rootfs_uuid=
rootfs_label=
bootfs_uuid=
efifs_uuid=
efi_dev=
efi_label=
image_disk=
image_url=
image_sha=
image_auth_type=
image_auth_token=
distro_name=
distro_version=
distro_release=
installed_os_bootnum=
serial_port=
serial_port_num=
log_output=
forge_test_user=
forge_test_pass=
update_grub_template="yes"
update_grub_cfg="yes"

function curl_url() {
	url=$1
	auth=$2
	curl --retry 5 --retry-all-errors -k -L -O $auth $url 2>&1 | tee $log_output
}

function verify_sha() {
	sha=$1

	len=$(expr length $sha)
	if [ $len -eq 40 ]; then
		shasum=shasum
	elif [ $len -eq 64 ]; then
		shasum=sha256sum
	elif [ $len -eq 96 ]; then
		shasum=sha384sum
	elif [ $len -eq 128 ]; then
		shasum=sha512sum
	else
		echo "Unknown sha digest length" | tee $log_output
		exit 1;
	fi
	echo "$sha $file" | $shasum --check 2>&1 | tee $log_output
}

function find_efi_disk() {
	for disk in "$@"
	do
		if lsblk -nrpo PARTTYPE "$disk" | grep -Eqi '^(c12a7328-f81f-11d2-ba4b-00a0c93ec93b|0xef)$'; then
			echo "$disk"
			return 0
		fi
	done
	return 1
}

function find_bootdisk() {
	disks=$(lsblk -bdnpo NAME,SIZE,TYPE | awk '$3 == "disk" { print $1, $2 }' | sort -k2,2n -k1,1V)
	disk_names=$(echo "$disks" | awk 'NF { print $1 }' | sort -V)

	if [ "$image_disk" == "smallest" ]; then
		smallest_size=$(echo "$disks" | awk 'NR == 1 { print $2 }')
		candidate_disks=$(echo "$disks" | awk -v size="$smallest_size" '$2 == size { print $1 }' | sort -V)
		candidate_count=$(echo "$candidate_disks" | awk 'NF { count++ } END { print count + 0 }')

		selected_disk=
		if [ "$candidate_count" -gt 1 ]; then
			selected_disk=$(find_efi_disk $candidate_disks)
		fi
		if [ -z "$selected_disk" ]; then
			selected_disk=$(echo "$candidate_disks" | head -n 1)
		fi
		image_disk=$selected_disk
	else
		image_disk=$(find_efi_disk $disk_names)
		if [ -z "$image_disk" ]; then
			if [ -b /dev/nvme0n1 ]; then
				image_disk="/dev/nvme0n1"
			elif [ -b /dev/sda ]; then
				image_disk="/dev/sda"
			fi
		fi
	fi

	if [ -z "$image_disk" ]; then
		echo "Boot drive not detected or specified" | tee $log_output
		exit 1;
	fi
}

function get_distro_image() {
	arch=$(uname -m)
	if [ "$distro_name" == "ubuntu" ]; then
		if [ "$arch" == "x86_64" ]; then
			arch=amd64
		elif [ "$arch" == "aarch64" ]; then
			arch=arm64
		fi
		if [ "$distro_version" == "24.04" ]; then
			codename=noble
		elif [ "$distro_version" == "23.04" ]; then
			codename=lunar
		elif [ "$distro_version" == "22.10" ]; then
			codename=kinetic
		elif [ "$distro_version" == "22.04" ]; then
			codename=jammy
		elif [ "$distro_version" == "21.10" ]; then
			codename=impish
		elif [ "$distro_version" == "21.04" ]; then
			codename=hirsute
		elif [ "$distro_version" == "20.10" ]; then
			codename=groovy
		elif [ "$distro_version" == "20.04" ]; then
			codename=focal
		else
			echo "Ubuntu version $distro_version not supported" | tee $log_output
			exit 1;
		fi

		efi_label="UEFI"
		image_url=https://cloud-images.ubuntu.com/releases/$codename/release/ubuntu-$distro_version-server-cloudimg-$arch.img
		shaurl=https://cloud-images.ubuntu.com/releases/$codename/release/SHA256SUMS
	elif [ "$distro_name" == "debian" ]; then
		if [ "$arch" == "x86_64" ]; then
			arch=amd64
		elif [ "$arch" == "aarch64" ]; then
			arch=arm64
		fi
		if [ "$distro_version" == "10" ]; then
			codename=buster
		elif [ "$distro_version" == "11" ]; then
			codename=bullseye
		elif [ "$distro_version" == "12" ]; then
			codename=bookworm
		elif [ "$distro_version" == "sid" ]; then
			codename=sid
		else
			echo "Debian version $distro_version not supported" | tee $log_output
			exit 1;
		fi
		image_url=http://cdimage.debian.org/cdimage/cloud/$codename/daily/latest/debian-$distro_version-generic-$arch-daily.qcow2
		shaurl=http://cdimage.debian.org/cdimage/cloud/$codename/daily/latest/SHA512SUMS
	elif [ "$distro" == "centos" ]; then
		image_url=https://cloud.centos.org/centos/$distro_version-stream/$arch/images/CentOS-Stream-GenericCloud-$distro_version-latest.$arch.qcow2
		shaurl=https://cloud.centos.org/centos/$distro_version-stream/$arch/images/CentOS-Stream-GenericCloud-$distro_version-latest.$arch.SHA256SUM
	else
		echo "Distro $distro_name not supported" | tee $log_output
		exit 1;
	fi
	curl --retry 5 --retry-all-errors -k -L $shaurl --output shafile 2>&1 | tee $log_output
	file=$(basename $image_url)
	image_sha=$(grep -m 1 $file shafile)
}

function add_cloud_init() {
	echo "fetching from cloud-init url: $cloud_init_url" | tee $log_output
	if [ -d /mnt/etc/cloud ]; then
		mkdir -p /mnt/etc/cloud/cloud.cfg.d
		echo "datasource_list: [ NoCloud, None ]" | tee /mnt/etc/cloud/cloud.cfg.d/98-forge-dslist.cfg
	fi
	seed_dir=/mnt/var/lib/cloud/seed/nocloud-net
	mkdir -p "$seed_dir"
	curl --fail --retry 5 --retry-all-errors -k "$cloud_init_url/user-data" --output "$seed_dir/user-data" 2>&1 | tee "$log_output"
	curl --fail --retry 5 --retry-all-errors -k "$cloud_init_url/meta-data" --output "$seed_dir/meta-data" 2>&1 | tee "$log_output"
	curl --fail --retry 5 --retry-all-errors -k "$cloud_init_url/network-config" --output "$seed_dir/network-config" 2>&1 | tee "$log_output"
}

function expand_root_fs() {
	is_nvme=$(echo $root_dev | grep nvme)
	if [ ! -z "$is_nvme" ]; then
		part_num=$(echo $root_dev | cut -d'p' -f2)
		growpart "$image_disk" "$part_num" 2>&1 | tee $log_output
		partprobe $image_disk 2>&1 | tee $log_output
		udevadm trigger 2>&1 | tee $log_output
		resize2fs -fF "$root_dev" 2>&1 | tee $log_output
	fi
	# not handling lvm resize currently
}

function resolve_esp_partition() {
	# Find the ESP by its GPT partition type code (EF00), reading the GPT
	# directly via sgdisk rather than relying on blkid's label cache, which
	# can be stale right after a raw qemu-img write onto a previously-used
	# (reprovisioned) disk.
	disk=$1
	part_num=$(sgdisk -p "$disk" 2>/dev/null | awk '{for (i=1;i<=NF;i++) if ($i=="EF00") {print $1; exit}}')
	if [ -z "$part_num" ]; then
		return 1
	fi
	is_nvme=$(echo "$disk" | grep nvme)
	if [ ! -z "$is_nvme" ]; then
		echo "$disk"p"$part_num"
	else
		echo "$disk""$part_num"
	fi
}

function device_is_on_disk() {
	local device=$1
	local disk=$2
	local disk_maj_min
	local ancestor_output
	local -a physical_disks

	disk_maj_min=$(lsblk -dnro MAJ:MIN "$disk") || return 1
	if [ -z "$disk_maj_min" ]; then
		return 1
	fi

	ancestor_output=$(lsblk -snro MAJ:MIN,TYPE "$device") || return 1
	mapfile -t physical_disks < <(
		printf '%s\n' "$ancestor_output" |
			awk '$2 == "disk" { print $1 }' |
			sort -u
	)

	[ "${#physical_disks[@]}" -eq 1 ] &&
		[ "${physical_disks[0]}" == "$disk_maj_min" ]
}

function find_devices_by_identifier() {
	local identifier_type=$1
	local identifier=$2
	local -n devices_out=$3
	local blkid_diagnostics
	local blkid_output
	local blkid_status
	local blkid_stderr_file

	case "$identifier_type" in
		UUID|LABEL)
			;;
		*)
			echo "Unsupported block device identifier type: $identifier_type" | tee "$log_output" >&2
			return 1
			;;
	esac

	devices_out=()
	blkid_stderr_file=$(mktemp) || return 1
	blkid_output=$(blkid -c /dev/null -t "$identifier_type=$identifier" -o device 2>"$blkid_stderr_file")
	blkid_status=$?
	blkid_diagnostics=$(<"$blkid_stderr_file")
	rm -f "$blkid_stderr_file"

	case "$blkid_status" in
		0)
			if [ -z "$blkid_output" ]; then
				echo "blkid returned success without a device for $identifier_type=$identifier" | tee "$log_output" >&2
				return 1
			fi
			if [ ! -z "$blkid_diagnostics" ]; then
				echo "blkid warning while looking up $identifier_type=$identifier: $blkid_diagnostics" | tee "$log_output" >&2
			fi
			mapfile -t devices_out <<< "$blkid_output"
			;;
		2)
			# blkid uses status 2 for a token that was not found. Only accept
			# the silent, empty form as an expected no-match result.
			if [ ! -z "$blkid_output" ] || [ ! -z "$blkid_diagnostics" ]; then
				echo "blkid failed while looking up $identifier_type=$identifier: stdout=${blkid_output:-<empty>}; stderr=${blkid_diagnostics:-<empty>}" | tee "$log_output" >&2
				return 1
			fi
			;;
		*)
			echo "blkid failed while looking up $identifier_type=$identifier with status $blkid_status: stdout=${blkid_output:-<empty>}; stderr=${blkid_diagnostics:-<empty>}" | tee "$log_output" >&2
			return 1
			;;
	esac
}

function check_identifier_conflicts() {
	local identifier_type=$1
	local identifier=$2
	local disk=$3
	local -a matching_devices
	local match_device

	find_devices_by_identifier "$identifier_type" "$identifier" matching_devices || return 1
	for match_device in "${matching_devices[@]}"; do
		if ! device_is_on_disk "$match_device" "$disk"; then
			echo "Device $match_device with $identifier_type=$identifier is not exclusively backed by image disk $disk" | tee "$log_output" >&2
			return 1
		fi
	done

	return 0
}

function precheck_image_identifiers() {
	if [ ! -z "$rootfs_uuid" ]; then
		check_identifier_conflicts UUID "$rootfs_uuid" "$image_disk" || return 1
	elif [ ! -z "$rootfs_label" ]; then
		check_identifier_conflicts LABEL "$rootfs_label" "$image_disk" || return 1
	fi
	if [ ! -z "$bootfs_uuid" ]; then
		check_identifier_conflicts UUID "$bootfs_uuid" "$image_disk" || return 1
	fi
	if [ ! -z "$efifs_uuid" ]; then
		check_identifier_conflicts UUID "$efifs_uuid" "$image_disk" || return 1
	fi
}

function resolve_device_on_disk() {
	local identifier_type=$1
	local identifier=$2
	local disk=$3
	local -a matching_devices
	local match_count
	local match_device

	find_devices_by_identifier "$identifier_type" "$identifier" matching_devices || return 1
	match_count=${#matching_devices[@]}
	if [ "$match_count" -eq 0 ]; then
		echo "No device found with $identifier_type=$identifier" | tee "$log_output" >&2
		return 1
	fi
	if [ "$match_count" -ne 1 ]; then
		echo "Expected exactly one device with $identifier_type=$identifier, found $match_count: ${matching_devices[*]}" | tee "$log_output" >&2
		return 1
	fi

	match_device=${matching_devices[0]}
	if ! device_is_on_disk "$match_device" "$disk"; then
		echo "Device $match_device with $identifier_type=$identifier is not exclusively backed by image disk $disk" | tee "$log_output" >&2
		return 1
	fi

	echo "$match_device"
}

function get_root_dev() {
	if [ ! -z "$rootfs_uuid" ]; then
		root_dev=$(resolve_device_on_disk UUID "$rootfs_uuid" "$image_disk") || return 1
	elif [ ! -z "$rootfs_label" ]; then
		root_dev=$(resolve_device_on_disk LABEL "$rootfs_label" "$image_disk") || return 1
	else
		echo "rootfs_uuid not specified and rootfs_label not determined" | tee "$log_output"
		echo "skipping root device changes" | tee "$log_output"
	fi
	if [ ! -z "$efi_label" ]; then
		efi_dev=$(resolve_device_on_disk LABEL "$efi_label" "$image_disk") || efi_dev=
	fi
	if [ -z "$efi_dev" ] && [ ! -z "$image_disk" ]; then
		echo "EFI partition not found by label [$efi_label]; falling back to GPT partition type EF00 (EFI System Partition) on $image_disk" | tee "$log_output"
		efi_dev=$(resolve_esp_partition "$image_disk")
	fi
	return 0
}

function is_port_in_list() {
	my_test_port=$1
	my_port_list=$2
	my_serial_port=""

	for port in $my_port_list
	do
		if [ "$port" == "$my_test_port" ]; then
			my_serial_port=$port
			break
		fi
	done
	echo $my_serial_port
}

function get_serial_port() {
	serial_port=""
	candidate_serial_ports="ttyS0 ttyS1 ttyAMA0"
	working_serial_ports=""
	preferred_port_arm="ttyAMA0"
	preferred_port_lenovo_supermicro="ttyS1"
	default_port="ttyS0"
	my_arch=$(uname -m)

	# See which ports we can write to
	for test_port in $candidate_serial_ports
	do
		echo "" >/dev/$test_port 2>/dev/null
		if [ $? -eq 0 ]; then
			working_serial_ports="$working_serial_ports $test_port"
		fi
	done
	working_serial_ports=$(echo "$working_serial_ports" | sed -e 's/^ *//g' -e 's/ *$//g')
	echo "Working serial ports = [${working_serial_ports}]"

	preferred_port=$default_port
	if [ "$my_arch" == "aarch64" ]; then
		preferred_port=$preferred_port_arm
	else
		if [ -f "/sys/class/dmi/id/sys_vendor" ]; then
			sys_vendor=$(</sys/class/dmi/id/sys_vendor)
			if [[ "$sys_vendor" =~ Lenovo || "$sys_vendor" =~ Supermicro ]]; then
				preferred_port=$preferred_port_lenovo_supermicro
			fi
		fi
	fi
	serial_port=$(is_port_in_list $preferred_port "$working_serial_ports")

	# If we couldn't find a preferred serial port, drop back to the first working one
	if [ "$serial_port" == "" ]; then
		serial_port=$(echo $working_serial_ports | awk '{print $1}')
		# If we still don't have one, default to console
		if [ "$serial_port" == "" ]; then
			serial_port="console"
		fi
	fi

	if [ "$serial_port" == "console" ]; then
		serial_port_num=0
	else
		serial_port_num=$(echo $serial_port | sed 's/[^0-9]//g' )
	fi

	log_output="/dev/$serial_port"
	echo "Using serial port: [$serial_port] ($serial_port_num)" | tee $log_output
}

function modify_grub_cfg() {
	local efi_status
	local grub_cfg_status

	efi_mounted=
	if [ ! -d "/mnt/boot/grub" ]; then
		boot_part=
		if [ ! -z "$bootfs_uuid" ]; then
			boot_part=$(resolve_device_on_disk UUID "$bootfs_uuid" "$image_disk") || return 1
		fi
		is_nvme=$(echo $image_disk | grep nvme)
		if [ -z "$boot_part" ]; then
			if [ ! -z "$is_nvme" ]; then
				boot_part="$image_disk"p1
			else
				boot_part="$image_disk"1
			fi
		fi

		if [ ! -b "$boot_part" ]; then
			# This is not error, as CentOS, for example, does not have dedicated /boot partition
			echo "Boot partition $boot_part not found or is not a block device" | tee $log_output
		else
			mount "$boot_part" /mnt/boot
		fi
		# we want to mount efi now as it can contain uefi grub.cfg
		if ! mount_efi; then
			if [[ $(grep '\/mnt\/boot' /proc/mounts) ]]; then
				umount /mnt/boot
			fi
			return 1
		fi
		efi_mounted=true
		grub_cfg=
		if [ -f "/mnt/boot/grub/grub.cfg" ]; then
			grub_cfg="/mnt/boot/grub/grub.cfg"
		elif [ -f "/mnt/boot/grub.cfg" ]; then
			grub_cfg="/mnt/boot/grub.cfg"
		elif [ -f "/mnt/boot/grub2/grub.cfg" ]; then
			grub_cfg="/mnt/boot/grub2/grub.cfg"
		else
			grub_cfg=$(find /mnt/boot -name grub.cfg -print -quit)
		fi
		if [ -z "$grub_cfg" ]; then
			echo "grub.cfg not found" | tee $log_output
			umount /mnt/boot
			return 0
		fi
	fi
	mount -o bind /dev /mnt/dev
	mount -o bind /proc /mnt/proc
	mount -o bind /sys /mnt/sys
	echo "Updating grub configuration" | tee $log_output
	# if we skipped grub mount before we want to mount efi now
	if [ -z "$efi_mounted" ]; then
		if ! mount_efi; then
			umount /mnt/sys
			umount /mnt/proc
			umount /mnt/dev
			if [[ $(grep '\/mnt\/boot' /proc/mounts) ]]; then
				umount /mnt/boot
			fi
			return 1
		fi
	fi
	# Check if grub2-mkconfig exists, means we are in rhel distro, falback to update-grub if not found
	if [ -f "/mnt/usr/sbin/grub2-mkconfig" ]; then
		is_bls=$(chroot /mnt /bin/sh -c "grub2-mkconfig --help" | grep "\-\-update-bls-cmdline")
		grub_bls_cmd=
		if [ ! -z "$is_bls" ]; then
			grub_bls_cmd="--update-bls-cmdline"
		fi
		chroot /mnt /bin/sh -c "grub2-mkconfig $grub_bls_cmd -o ${grub_cfg#'/mnt'}" 2>&1 | tee $log_output
		grub_cfg_status=${PIPESTATUS[0]}
	else
		chroot /mnt /bin/sh -c update-grub 2>&1 | tee $log_output
		grub_cfg_status=${PIPESTATUS[0]}
	fi
	create_efi_boot_entry
	efi_status=$?
	if [ "$efi_status" -eq 0 ]; then
		set_boot_order
		efi_status=$?
	fi
	umount /mnt/boot/efi 2>&1 | tee $log_output
	umount /mnt/sys
	umount /mnt/proc
	umount /mnt/dev
	if [[ $(grep '\/mnt\/boot' /proc/mounts) ]]; then
		umount /mnt/boot
	fi
	if [ "$grub_cfg_status" -ne 0 ]; then
		return "$grub_cfg_status"
	fi
	return "$efi_status"
}

function get_part_num() {
	dev=$1
	is_nvme=$(echo $dev | grep nvme)
	if [ ! -z "$is_nvme" ]; then
		echo $dev | sed -E 's/.*p([0-9]+)$/\1/'
	else
		echo $dev | sed -E 's/.*[^0-9]([0-9]+)$/\1/'
	fi
}

function efi_boot_entries() {
	# Always pass -v. efibootmgr >= 18 prints the HD(...)/File(...) device
	# path unconditionally, but <= 17 only prints it under -v, and every
	# match below depends on that path being present. The extra "dp:"/"data:"
	# hexdump lines -v adds are indented, so they never match the
	# "^Boot####" filter.
	efibootmgr -v | grep -E "^Boot[0-9A-Fa-f]{4}"
}

function is_network_entry() {
	# Classify from the device path first: PXE/HTTP boot options carry
	# MAC()/IPv4()/IPv6()/Uri() nodes regardless of how the firmware labels
	# them. Fall back to label keywords, because some firmware exposes its
	# native network boot options as an opaque VenHw() path with nothing but
	# the label to go on, e.g. on Dell:
	#   Boot0001* NIC in Slot 7 Port 1 Partition 1	VenHw(986d1755-...)
	# Neither test alone covers both; Supermicro/NVIDIA boards label theirs
	# "UEFI PXEv4 (MAC:...)" / "UEFI HTTPv4 (MAC:...)" and match on both.
	case "$1" in
		*mac\(*|*ipv4\(*|*ipv6\(*|*uri\(*) return 0 ;;
		*pxe*|*http*|*nic*|*network*|*ethernet*) return 0 ;;
	esac
	return 1
}

function efi_entry_label() {
	# Strip the "Boot####[*]" prefix and any device path that follows, leaving
	# just the description. efibootmgr separates the two with a tab.
	printf '%s' "$1" | sed -E 's/^Boot[0-9A-Fa-f]{4}\*?[[:space:]]*//' | cut -d'	' -f1
}

function create_efi_boot_entry() {
	local efi_create_status
	local efi_list_status

	if ! command -v efibootmgr >/dev/null 2>&1; then
		echo "efibootmgr not available, skipping EFI boot entry creation" | tee $log_output
		return 0
	fi
	if [ -z "$efi_dev" ]; then
		echo "EFI device not resolved, skipping EFI boot entry creation" | tee $log_output
		return 0
	fi

	shim_arch=
	boot_csv_name=
	efi_arch=$(uname -m)
	if [ "$efi_arch" == "x86_64" ]; then
		shim_arch="x64"
		boot_csv_name="BOOTX64.CSV"
	elif [ "$efi_arch" == "aarch64" ]; then
		shim_arch="aa64"
		boot_csv_name="BOOTAA64.CSV"
	else
		echo "Unsupported arch $efi_arch for EFI boot entry creation" | tee $log_output
		return 0
	fi

	# Discover the installed bootloader directly from the ESP rather than
	# assuming a path derived from distro_name, which is not always set
	# (e.g. when the image is provisioned via a raw image_url instead of
	# image_distro_name/image_distro_version on the kernel cmdline).
	# Sort rather than taking whatever the filesystem yields first: an ESP
	# reprovisioned over a different distro can hold more than one shim (e.g.
	# a leftover EFI/dgx alongside the new EFI/ubuntu), and -print -quit
	# would pick between them nondeterministically. Prefer the distro_name
	# directory when one was supplied, then a shim paired with an architecture-
	# matching boot hint CSV whose first field names that shim.
	shim_paths=$(find /mnt/boot/efi/EFI -mindepth 2 -maxdepth 2 -type f -iname "shim${shim_arch}.efi" 2>/dev/null | sort)
	if [ -z "$shim_paths" ]; then
		echo "No shim${shim_arch}.efi found under /mnt/boot/efi/EFI/*, skipping EFI boot entry creation" | tee $log_output
		return 0
	fi
	shim_path=$(printf '%s\n' "$shim_paths" | head -n1)
	distro_shim_path=
	paired_shim_path=
	paired_csv_label=
	csv_label=
	while IFS= read -r candidate_shim; do
		candidate_dir=$(dirname "$candidate_shim")
		candidate_distro_name=$(basename "$candidate_dir")
		candidate_is_distro=
		if [ ! -z "$distro_name" ] && [ "${candidate_distro_name,,}" == "${distro_name,,}" ]; then
			candidate_is_distro=true
			if [ -z "$distro_shim_path" ]; then
				distro_shim_path="$candidate_shim"
			fi
		fi

		# Match shim's lookup order: use BOOT.CSV only when the
		# architecture-specific file is absent, cannot be decoded, or has no
		# matching loader record with a non-empty label.
		candidate_shim_name=$(basename "$candidate_shim")
		candidate_csv_matches=
		candidate_csv_label=
		for candidate_csv_name in "$boot_csv_name" "BOOT.CSV"; do
			candidate_csv=$(find "$candidate_dir" -maxdepth 1 -type f -iname "$candidate_csv_name" -print -quit 2>/dev/null)
			[ -z "$candidate_csv" ] && continue
			candidate_csv_contents=$(iconv -f UTF-16 -t UTF-8 "$candidate_csv" 2>/dev/null)
			[ $? -ne 0 ] && continue

			# A boot hint can contain more than one loader record.
			while IFS= read -r candidate_csv_line || [ ! -z "$candidate_csv_line" ]; do
				candidate_csv_line=${candidate_csv_line%$'\r'}
				[ -z "$candidate_csv_line" ] && continue
				IFS=',' read -r candidate_loader candidate_label _ <<< "$candidate_csv_line"
				if [ ! -z "$candidate_label" ] && [ "${candidate_loader,,}" == "${candidate_shim_name,,}" ]; then
					candidate_csv_matches=true
					candidate_csv_label="$candidate_label"
					break
				fi
			done <<< "$candidate_csv_contents"
			[ "$candidate_csv_matches" == true ] && break
		done
		[ "$candidate_csv_matches" != true ] && continue

		if [ "$candidate_is_distro" == true ]; then
			shim_path="$candidate_shim"
			csv_label="$candidate_csv_label"
			break
		fi
		if [ -z "$paired_shim_path" ]; then
			paired_shim_path="$candidate_shim"
			paired_csv_label="$candidate_csv_label"
		fi
	done <<< "$shim_paths"
	if [ -z "$csv_label" ]; then
		if [ ! -z "$distro_shim_path" ]; then
			shim_path="$distro_shim_path"
		elif [ ! -z "$paired_shim_path" ]; then
			shim_path="$paired_shim_path"
			csv_label="$paired_csv_label"
		fi
	fi
	if [ "$(printf '%s\n' "$shim_paths" | wc -l)" -gt 1 ]; then
		echo "Multiple shim${shim_arch}.efi found on ESP, using $shim_path:" | tee $log_output
		printf '%s\n' "$shim_paths" | tee $log_output
	fi

	efi_dir=$(dirname "$shim_path")
	distro_dir=$(basename "$efi_dir")
	loader_rel="/EFI/$distro_dir/$(basename "$shim_path")"
	loader_path=$(echo "$loader_rel" | sed 's#/#\\#g')

	# Prefer the label recorded in the shim's own .csv hint file (the
	# authoritative source per the UEFI shim convention: <loader>,<label>,
	# <optional args>,<description>, UTF-16 encoded), falling back to the
	# ESP directory name.
	label="$csv_label"
	if [ -z "$label" ]; then
		label="$distro_dir"
	fi

	esp_part_num=$(get_part_num "$efi_dev")
	if [ -z "$esp_part_num" ]; then
		echo "Could not determine ESP partition number for $efi_dev, skipping EFI boot entry creation" | tee $log_output
		return 0
	fi

	# Delete every entry carrying our label, then create exactly one. This is
	# deliberately unconditional rather than "create only if missing":
	# reprovisioning the same image accumulates same-labelled entries that
	# cannot be told apart from the live one by label, and whichever of them
	# sorts first is what the machine boots. Replacing the whole set makes the
	# outcome independent of whatever was there before.
	#
	# It also cleans up entries the firmware has already given up on. When
	# the target of a boot option no longer resolves, AMI firmware prepends a
	# broken-entry sentinel and an END_ENTIRE node, and efibootmgr stops
	# rendering there, so the listing shows only:
	#   Boot000B* Ubuntu	VenHw(99e275e7-75a0-4b37-a2e6-c5385e6c00cb)
	# while the variable still holds the original
	# HD(15,GPT,815eb350-...)/File(\EFI\ubuntu\shimaa64.efi). One test node
	# had four such orphans, all labelled "Ubuntu", all pointing at an ESP
	# that no longer existed -- one per reprovision. Deleting by label catches
	# them without having to parse raw variables, because the label is the one
	# field the sentinel leaves intact.
	#
	# Trade-off: this is ESP-blind. A legitimately separate install of the
	# same distro on another disk in the same machine shares the label and
	# would be deleted too. That is acceptable for whole-node provisioning,
	# where the imager owns the box, but it is the reason this is scoped to
	# an exact label rather than a substring.
	#
	# "-B -L" needs efibootmgr >= 18 (delete_label() does not exist in 17)
	# and exits non-zero when nothing matched, which is the normal case on a
	# first install, so the failure is logged rather than treated as fatal.
	echo "Removing any existing EFI boot entries labelled $label" | tee $log_output
	delete_out=$(efibootmgr -B -L "$label" 2>&1)
	if [ $? -ne 0 ]; then
		echo "No existing EFI boot entry labelled $label to remove ($delete_out)" | tee $log_output
	fi

	echo "Creating EFI boot entry for $label ($loader_path on $image_disk part $esp_part_num)" | tee $log_output
	efibootmgr --create --disk "$image_disk" --part "$esp_part_num" --label "$label" --loader "$loader_path" 2>&1 | tee "$log_output"
	efi_create_status=${PIPESTATUS[0]}
	if [ "$efi_create_status" -ne 0 ]; then
		return "$efi_create_status"
	fi

	# Resolve the number efibootmgr assigned, so set_boot_order() can rank
	# this specific entry. It is not echoed in a parseable form, so find it by
	# walking BootOrder and taking the first entry carrying our label:
	# --create places the new entry at the front of BootOrder, which makes the
	# first match the one just created.
	#
	# Deliberately not "the only entry with this label". That would be true
	# whenever the delete above succeeded, but it silently stops being true if
	# delete-by-label is unavailable (efibootmgr <= 17) or fails for any other
	# reason, and picking a leftover then reintroduces exactly the bug this is
	# meant to fix. Ordering holds either way.
	efi_list=$(efibootmgr -v 2>&1)
	efi_list_status=$?
	if [ "$efi_list_status" -ne 0 ]; then
		echo "Failed to read EFI boot entries after creating $label: $efi_list" | tee "$log_output"
		return "$efi_list_status"
	fi
	installed_os_bootnum=
	label_matches=0
	IFS=',' read -r -a created_order <<< "$(echo "$efi_list" | grep '^BootOrder:' | sed -E 's/^BootOrder:[[:space:]]*//')"
	for bootnum in "${created_order[@]}"; do
		bootline=$(echo "$efi_list" | grep -E "^Boot$bootnum")
		[ -z "$bootline" ] && continue
		if [ "$(efi_entry_label "$bootline")" == "$label" ]; then
			label_matches=$((label_matches + 1))
			if [ -z "$installed_os_bootnum" ]; then
				installed_os_bootnum="$bootnum"
			fi
		fi
	done

	if [ -z "$installed_os_bootnum" ]; then
		echo "Warning: created EFI boot entry for $label but could not resolve its Boot####" | tee $log_output
	elif [ "$label_matches" -gt 1 ]; then
		# Only reachable if the delete did not take effect. Not fatal, since
		# the entry resolved above is still the one just created, but it means
		# leftovers are accumulating and should be looked at.
		echo "Warning: $label_matches EFI boot entries labelled $label remain after cleanup; using Boot$installed_os_bootnum" | tee $log_output
	fi
}

function set_boot_order() {
	local boot_order_status
	local efi_list_status

	if ! command -v efibootmgr >/dev/null 2>&1; then
		return 0
	fi

	efi_list=$(efibootmgr -v 2>&1)
	efi_list_status=$?
	if [ "$efi_list_status" -ne 0 ]; then
		echo "Could not read current EFI boot entries" | tee "$log_output"
		return "$efi_list_status"
	fi
	current_order_line=$(echo "$efi_list" | grep '^BootOrder:')
	if [ -z "$current_order_line" ]; then
		echo "Could not read current BootOrder, skipping reorder" | tee $log_output
		return 0
	fi
	current_order_csv=$(echo "$current_order_line" | sed -E 's/^BootOrder:[[:space:]]*//')
	IFS=',' read -r -a current_order <<< "$current_order_csv"

	# Rank the single Boot#### create_efi_boot_entry() resolved for this
	# install, not "every entry whose text looks like the distro". Grouping
	# by label preserves the relative order of same-labelled entries, so the
	# winner among them was decided by the pre-existing BootOrder -- the very
	# thing this function exists to normalize. Where a stale same-labelled
	# entry happened to sort first, the machine booted it.
	#
	# If the identifier is unknown (efibootmgr missing, no shim on the ESP,
	# creation failed) the OS entry falls through to "rest" and still lands
	# behind the network entries, which is the safe direction.
	network=()
	target=()
	rest=()
	for bootnum in "${current_order[@]}"; do
		entry_label=$(echo "$efi_list" | grep -E "^Boot$bootnum" | sed -E 's/^Boot[0-9A-Fa-f]{4}\*?[[:space:]]*//')
		lower_label=$(echo "$entry_label" | tr '[:upper:]' '[:lower:]')
		if [ ! -z "$installed_os_bootnum" ] && [ "$bootnum" == "$installed_os_bootnum" ]; then
			target=("$bootnum")
		elif is_network_entry "$lower_label"; then
			network+=("$bootnum")
		else
			rest+=("$bootnum")
		fi
	done

	new_order=("${network[@]}" "${target[@]}" "${rest[@]}")
	new_order_csv=$(IFS=,; echo "${new_order[*]}")

	if [ -z "$new_order_csv" ]; then
		echo "Computed empty boot order, skipping reorder" | tee $log_output
		return 0
	fi

	if [ "$new_order_csv" == "$current_order_csv" ]; then
		echo "Boot order already network-first then Boot${installed_os_bootnum:-<unresolved>}, no change needed" | tee $log_output
		return 0
	fi

	echo "Setting boot order to: $new_order_csv" | tee $log_output
	efibootmgr -o "$new_order_csv" 2>&1 | tee "$log_output"
	boot_order_status=${PIPESTATUS[0]}
	return "$boot_order_status"
}

function mount_efi() {
	local mount_status

	if [ ! -z "$efifs_uuid" ]; then
		efi_dev=$(resolve_device_on_disk UUID "$efifs_uuid" "$image_disk") || return 1
	fi
	if [ ! -z "$efi_dev" ]; then
		mkdir -p /mnt/boot/efi
		mount $efi_dev /mnt/boot/efi 2>&1 | tee $log_output
		mount_status=${PIPESTATUS[0]}
	else
		chroot /mnt /bin/sh -c 'mount /boot/efi' 2>&1 | tee $log_output
		mount_status=${PIPESTATUS[0]}
	fi
	return "$mount_status"
}

function add_testing_user() {
	if [ -z "$forge_test_user" ]; then
		return 0
	fi
	if [ ! -f "/mnt/etc/passwd" ]; then
		return 0
	fi
	echo "useradd -s /bin/bash -d /home/$forge_test_user -m -G sudo $forge_test_user" > /mnt/test_user.sh 2>&1 | tee $log_output
	echo "echo \"$forge_test_user:$forge_test_pass\" | chpasswd" >> /mnt/test_user.sh 2>&1 | tee $log_output
	echo "passwd --expire $forge_test_user" >> /mnt/test_user.sh 2>&1 | tee $log_output
	chmod +x /mnt/test_user.sh 2>&1 | tee $log_output
	chroot /mnt /bin/sh -c ./test_user.sh 2>&1 | tee $log_output
	rm -f /mnt/test_user.sh 2>&1 | tee $log_output
}

function modify_grub_template() {
	if [ ! -f "/mnt/etc/default/grub" ]; then
		return 0
	fi
	new_grub_template="/mnt/grub_default"
	echo > $new_grub_template
	cmdline_found=
	serial_found=
	terminal_found=
	while read -r tmp; do
		if [[ "$tmp" =~ ^\ *# ]]; then
			echo "$tmp" >> $new_grub_template
		else
			if [[ "$tmp" =~ GRUB_CMDLINE_LINUX= ]]; then
				first_console_set=
				second_console_set=
				if [ -z "$cmdline_found" ]; then
					# ensure console is set
					echo -n "GRUB_CMDLINE_LINUX=\"" >> $new_grub_template
					cmdline_args=$(echo $tmp | sed s/GRUB_CMDLINE_LINUX=//g | sed s/^\"//g | sed s/\"$//g)
					for i in $(echo $cmdline_args); do
						kernel_arg=$(echo $i|grep console)
						if [ ! -z "$kernel_arg" ]; then
							if [ -z "$first_console_set" ]; then
								echo -n "console=tty0 " >> $new_grub_template
								first_console_set=true
							elif [ -z "$second_console_set" ]; then
								echo -n "console=$serial_port " >> $new_grub_template
								second_console_set=true
							else
								echo -n "$kernel_arg " >> $new_grub_template
							fi
						else
							echo -n "$i " >> $new_grub_template
						fi
					done
					# parsed grub cmdline for linux and didnt find any console specified, add it
					if [ -z "$first_console_set" ]; then
						echo -n "console=tty0 " >> $new_grub_template
						first_console_set=true
					fi
					if [ -z "$second_console_set" ]; then
						echo -n "console=$serial_port,115200" >> $new_grub_template
						second_console_set=true
					fi
					echo "\"" >> $new_grub_template
					cmdline_found="started"
				fi
			elif [[ "$tmp" =~ GRUB_TERMINAL ]]; then
				if [ -z "$terminal_found" ]; then
					echo "GRUB_TERMINAL=serial" >> $new_grub_template
					terminal_found=true
				fi
			elif [[ "$tmp" =~ GRUB_SERIAL_COMMAND ]]; then
				if [ -z "$serial_found" ]; then
					echo "GRUB_SERIAL_COMMAND=\"serial --speed=115200 --unit=$serial_port_num --word=8 --parity=no --stop=1\"" >> $new_grub_template
					serial_found=true
				fi
			else
				echo "$tmp" >> $new_grub_template
			fi
		fi
	done < "/mnt/etc/default/grub"
	# done parsing the file, didn't find the grub args
	if [ -z "$cmdline_found" ]; then
		echo "GRUB_CMDLINE_LINUX=\"console=tty0 console=$serial_port,115200\"" >> $new_grub_template
	fi
	if [ -z "$serial_found" ]; then
		echo "GRUB_SERIAL_COMMAND=\"serial --speed=115200 --unit=$serial_port_num --word=8 --parity=no --stop=1\"" >> $new_grub_template
	fi
	if [ -z "$terminal_found" ]; then
		echo "GRUB_TERMINAL=serial" >> $new_grub_template
	fi
	cat $new_grub_template > /mnt/etc/default/grub
}

function parse_kernel_cmdline_argument() {
	local argument=$1

	case "$argument" in
		image_url=*)
			image_url=${argument#image_url=}
			;;
		image_sha=*)
			image_sha=${argument#image_sha=}
			;;
		image_auth_type=*)
			image_auth_type=${argument#image_auth_type=}
			;;
		image_auth_token=*)
			image_auth_token=${argument#image_auth_token=}
			;;
		image_disk=*)
			image_disk=${argument#image_disk=}
			;;
		image_distro_name=*)
			distro_name=${argument#image_distro_name=}
			distro_name=${distro_name,,}
			;;
		image_distro_version=*)
			distro_version=${argument#image_distro_version=}
			;;
		image_distro_release=*)
			distro_release=${argument#image_distro_release=}
			;;
		"ds=nocloud;s="*|"ds=nocloud-net;s="*)
			cloud_init_url=${argument#*;s=}
			;;
		create_forge_test_user=*)
			user_pass=${argument#create_forge_test_user=}
			forge_test_user=${user_pass%%:*}
			forge_test_pass=${user_pass#*:}
			;;
		rootfs_uuid=*)
			rootfs_uuid=${argument#rootfs_uuid=}
			;;
		rootfs_label=*)
			rootfs_label=${argument#rootfs_label=}
			;;
		bootfs_uuid=*)
			bootfs_uuid=${argument#bootfs_uuid=}
			;;
		efifs_uuid=*)
			efifs_uuid=${argument#efifs_uuid=}
			;;
		update_grub_template=*)
			update_grub_template=${argument#update_grub_template=}
			;;
		update_grub_cfg=*)
			update_grub_cfg=${argument#update_grub_cfg=}
			;;
		*)
			return 1
			;;
	esac

	return 0
}

function main() {

	get_serial_port
	# look for a distro and version (and release for centos)
	#  image_distro_name=ubuntu
	#  image_distro_version=20.04
	# or a url for a disk image (and a sha256 optionally)
	#  image_url=<url>
	#  image_sha=[sha1/sha256/sha384/sha512]
	# use the disk the tenant specified optionally
	#  image_disk=/dev/nvme0n1
	#  image_disk=smallest
	local -a kernel_arguments
	local argument
	read -r -a kernel_arguments < /proc/cmdline
	for argument in "${kernel_arguments[@]}"
	do
		parse_kernel_cmdline_argument "$argument" || true
	done
	if [ -z "$rootfs_uuid" ] && [ -z "$rootfs_label" ]; then
		rootfs_label="cloudimg-rootfs" #default rootfs name for cloud images
	fi

	if [ ! -z "$distro_name" ]; then
		get_distro_image
	fi

	if [ -z "$image_url" ]; then
		echo "Could not resolve disk image to use from arguments in /proc/cmdline" | tee $log_output
		return 1;
	fi

	if [ -z $file ]; then
		file=$(basename $image_url)
	fi

	if [ ! -z "$image_auth_token" ]; then
		if [ -z "$image_auth_type" ]; then
		       image_auth_type=Bearer
		fi
		image_auth="-H \"Authorization: $image_auth_type $image_auth_token\""
	fi

	echo "Downloading image from $image_url" | tee $log_output
	curl_url $image_url $image_auth
	if [ ! -z "$image_sha" ]; then
		echo "Verifying image with digest $image_sha" | tee $log_output
		verify_sha $image_sha
		if [ $? -ne 0 ]; then
			echo "Image checksum validation failed" | tee $log_output
			return 1;
		fi
	fi
	if [ -z "$image_disk" -o "$image_disk" == "smallest" ]; then
		find_bootdisk
	fi
	resolved_image_disk=$(readlink -e -- "$image_disk")
	if [ -z "$resolved_image_disk" ] || [ ! -b "$resolved_image_disk" ]; then
		echo "Image disk $image_disk does not exist or is not a block device" | tee "$log_output"
		return 1;
	fi
	image_disk_type=$(lsblk -dnro TYPE "$resolved_image_disk")
	if [ "$image_disk_type" != "disk" ]; then
		echo "Image disk $image_disk is not a whole-disk block device" | tee "$log_output"
		return 1;
	fi
	image_disk=$resolved_image_disk
	precheck_image_identifiers || return 1

	echo "Imaging $file to $image_disk" | tee $log_output
	qemu-img convert -p -O raw -S 0 $file $image_disk 2>&1 | tee $log_output
	ret=$?
	if [ $ret -ne 0 ]; then
		echo "Imaging failed $ret" | tee $log_output
		return $ret;
	fi

	sgdisk -epv $image_disk 2>&1 | tee $log_output
	partprobe $image_disk 2>&1 | tee $log_output
	for cmd in pvscan vgscan lvscan
	do
		$cmd | tee $log_output
		udevadm settle | tee $log_output
	done
	if [ ! -z "$rootfs_uuid" -o ! -z "$rootfs_label" ]; then
		# find the root partition/volume
		get_root_dev || return 1
		echo "Root device [$root_dev]" | tee $log_output
		if [ -b "$root_dev" ]; then
			mount "$root_dev" /mnt 2>&1 | tee $log_output
			if [ "${update_grub_template}" == "yes" ]; then
				echo "Updating grub template" | tee $log_output
				modify_grub_template
			fi
			if [ "${update_grub_cfg}" == "yes" ]; then
				echo "Updating grub cfg" | tee $log_output
				if ! modify_grub_cfg; then
					umount /mnt 2>&1 | tee "$log_output"
					return 1
				fi
			fi
			if [ ! -z "$cloud_init_url" ]; then
				add_cloud_init
			fi
			add_testing_user
			umount /mnt 2>&1 | tee $log_output
			expand_root_fs
		fi
	fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
	main
	echo "Rebooting" | tee $log_output
	systemctl reboot | tee $log_output
fi
