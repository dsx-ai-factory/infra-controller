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
set -u

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script_under_test="$script_dir/disk_imaging.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

source "$script_under_test"

log_output="$temp_dir/disk-imaging.log"
mock_blkid_status=0
mock_blkid_stdout=
mock_blkid_stderr=

fail() {
	echo "FAIL: $1" >&2
	exit 1
}

assert_eq() {
	local description=$1
	local expected=$2
	local actual=$3

	if [ "$actual" != "$expected" ]; then
		fail "$description: expected [$expected], got [$actual]"
	fi
}

assert_log_contains() {
	local expected=$1

	if ! grep -Fq -- "$expected" "$log_output"; then
		fail "log does not contain [$expected]"
	fi
}

set_blkid_result() {
	mock_blkid_status=$1
	mock_blkid_stdout=$2
	mock_blkid_stderr=$3
	: >"$log_output"
}

blkid() {
	printf '%s' "$mock_blkid_stdout"
	printf '%s' "$mock_blkid_stderr" >&2
	return "$mock_blkid_status"
}

lsblk() {
	case "$2:$3" in
		MAJ:MIN:/dev/target)
			printf '%s\n' "259:0"
			;;
		MAJ:MIN,TYPE:/dev/targetp1)
			printf '%s\n' "259:1 part" "259:0 disk"
			;;
		MAJ:MIN,TYPE:/dev/targetp2)
			printf '%s\n' "259:2 part" "259:0 disk"
			;;
		MAJ:MIN,TYPE:/dev/targetp16)
			printf '%s\n' "259:16 part" "259:0 disk"
			;;
		MAJ:MIN,TYPE:/dev/otherp1)
			printf '%s\n' "259:9 part" "259:8 disk"
			;;
		*)
			return 1
			;;
	esac
}

declare -a devices

echo "no match"
set_blkid_result 2 "" ""
devices=(stale)
find_devices_by_identifier UUID missing devices ||
	fail "silent no-match lookup failed"
assert_eq "silent no-match device count" 0 "${#devices[@]}"
if resolve_device_on_disk UUID missing /dev/target >/dev/null 2>&1; then
	fail "post-image resolver accepted a missing identifier"
fi
assert_log_contains "No device found with UUID=missing"

echo "target disk"
set_blkid_result 0 "/dev/targetp1" ""
resolved=$(resolve_device_on_disk UUID root /dev/target) ||
	fail "target-disk identifier was rejected"
assert_eq "resolved target device" "/dev/targetp1" "$resolved"

echo "another disk"
set_blkid_result 0 "/dev/otherp1" ""
if check_identifier_conflicts LABEL cloudimg-rootfs /dev/target >/dev/null 2>&1; then
	fail "off-target identifier was accepted"
fi
assert_log_contains \
	"Device /dev/otherp1 with LABEL=cloudimg-rootfs is not exclusively backed by image disk /dev/target"

echo "duplicate matches"
set_blkid_result 0 $'/dev/targetp1\n/dev/targetp2' ""
if resolve_device_on_disk UUID duplicate /dev/target >/dev/null 2>&1; then
	fail "duplicate identifiers were accepted"
fi
assert_log_contains \
	"Expected exactly one device with UUID=duplicate, found 2: /dev/targetp1 /dev/targetp2"

echo "successful lookup with warning"
set_blkid_result 0 "/dev/targetp1" "blkid warning"
devices=()
find_devices_by_identifier UUID root devices >/dev/null 2>&1 ||
	fail "successful lookup with a warning was rejected"
assert_eq "warning lookup device count" 1 "${#devices[@]}"
assert_eq "warning lookup device" "/dev/targetp1" "${devices[0]}"
assert_log_contains "blkid warning while looking up UUID=root: blkid warning"

echo "success without output"
set_blkid_result 0 "" ""
if find_devices_by_identifier UUID root devices >/dev/null 2>&1; then
	fail "successful empty lookup was accepted"
fi
assert_log_contains "blkid returned success without a device for UUID=root"

echo "no-match status with diagnostics"
set_blkid_result 2 "" "unexpected diagnostic"
if find_devices_by_identifier UUID root devices >/dev/null 2>&1; then
	fail "status 2 with diagnostics was accepted as no-match"
fi
assert_log_contains "stderr=unexpected diagnostic"

echo "unexpected command failure"
set_blkid_result 4 "" "command failed"
if find_devices_by_identifier UUID root devices >/dev/null 2>&1; then
	fail "unexpected blkid failure was accepted"
fi
assert_log_contains \
	"blkid failed while looking up UUID=root with status 4: stdout=<empty>; stderr=command failed"

assert_parsed_argument() {
	local argument=$1
	local variable_name=$2
	local expected=$3

	printf -v "$variable_name" '%s' "unparsed"
	parse_kernel_cmdline_argument "$argument" ||
		fail "argument was not recognized: $argument"
	assert_eq "$argument" "$expected" "${!variable_name}"
}

echo "kernel command-line arguments"
image_disk=unchanged-disk
assert_parsed_argument \
	'image_url=https://images.example/image.qcow2?token=a=b-image_disk' \
	image_url \
	'https://images.example/image.qcow2?token=a=b-image_disk'
assert_eq "image_url does not alter image_disk" "unchanged-disk" "$image_disk"

image_url=unchanged-url
assert_parsed_argument \
	'image_disk=/dev/disk/by-id/foo=bar-image_url' \
	image_disk \
	'/dev/disk/by-id/foo=bar-image_url'
assert_eq "image_disk does not alter image_url" "unchanged-url" "$image_url"

assert_parsed_argument 'image_sha=sha=value' image_sha 'sha=value'
assert_parsed_argument 'image_auth_type=Bearer' image_auth_type 'Bearer'
assert_parsed_argument 'image_auth_token=token=value' image_auth_token 'token=value'
assert_parsed_argument 'image_distro_name=UbUnTu' distro_name 'ubuntu'
assert_parsed_argument 'image_distro_version=24.04' distro_version '24.04'
assert_parsed_argument 'image_distro_release=server' distro_release 'server'
assert_parsed_argument 'rootfs_uuid=root=value' rootfs_uuid 'root=value'
assert_parsed_argument 'rootfs_label=root-label' rootfs_label 'root-label'
assert_parsed_argument 'bootfs_uuid=boot=value' bootfs_uuid 'boot=value'
assert_parsed_argument 'efifs_uuid=efi=value' efifs_uuid 'efi=value'
assert_parsed_argument 'update_grub_template=yes' update_grub_template 'yes'
assert_parsed_argument 'update_grub_cfg=yes' update_grub_cfg 'yes'
assert_parsed_argument \
	'ds=nocloud;s=https://cloud.example/data?token=a=b' \
	cloud_init_url \
	'https://cloud.example/data?token=a=b'
assert_parsed_argument \
	'ds=nocloud-net;s=https://cloud.example/data?token=c=d' \
	cloud_init_url \
	'https://cloud.example/data?token=c=d'

forge_test_user=
forge_test_pass=
parse_kernel_cmdline_argument \
	'create_forge_test_user=tester:pass:word=value' ||
	fail "test-user argument was not recognized"
assert_eq "test-user name" "tester" "$forge_test_user"
assert_eq "test-user password preserves delimiters" "pass:word=value" "$forge_test_pass"

image_url=unchanged-url
image_disk=unchanged-disk
if parse_kernel_cmdline_argument \
	'other=image_disk=/dev/disk/by-id/foo-image_url'; then
	fail "unknown argument was accepted"
fi
assert_eq "unknown argument leaves image_url unchanged" "unchanged-url" "$image_url"
assert_eq "unknown argument leaves image_disk unchanged" "unchanged-disk" "$image_disk"

echo "partition identifier types"
set_blkid_result 0 "/dev/targetp16" ""
devices=()
find_devices_by_identifier PARTUUID boot devices >/dev/null 2>&1 ||
	fail "PARTUUID lookup was rejected"
assert_eq "PARTUUID lookup device" "/dev/targetp16" "${devices[0]}"
if find_devices_by_identifier DEVNAME boot devices >/dev/null 2>&1; then
	fail "unsupported identifier type was accepted"
fi
assert_log_contains "Unsupported block device identifier type: DEVNAME"

fstab="$temp_dir/fstab"
stderr_file="$temp_dir/stderr"
image_disk=/dev/target
bootfs_uuid=

assert_stderr_contains() {
	local expected=$1

	if ! grep -Fq -- "$expected" "$stderr_file"; then
		fail "stderr does not contain [$expected]"
	fi
}

echo "partition naming"
assert_eq "nvme partition name" "/dev/nvme0n1p16" "$(partition_on_disk /dev/nvme0n1 16)"
assert_eq "sd partition name" "/dev/sda2" "$(partition_on_disk /dev/sda 2)"

echo "fstab: dedicated /boot partition"
printf '%s\n' \
	'# /etc/fstab: static file system information.' \
	'' \
	'LABEL=cloudimg-rootfs	/	ext4	discard,commit=30,errors=remount-ro	0 1' \
	'LABEL=UEFI	/boot/efi	vfat	umask=0077	0 1' \
	'#LABEL=OLDBOOT	/boot	ext4	defaults	0 2' \
	'   LABEL=BOOT	/boot/	ext4	defaults,x-systemd.device-timeout=30	0 2  ' \
	'none	/tmp	tmpfs	defaults	0 0' >"$fstab"
set_blkid_result 0 "/dev/targetp16" ""
resolved=$(resolve_fstab_mount_device /boot /dev/target "$fstab") ||
	fail "fstab /boot entry was not resolved"
assert_eq "fstab /boot device" "/dev/targetp16" "$resolved"
resolve_boot_partition "$fstab" >/dev/null ||
	fail "resolve_boot_partition failed on a split /boot image"
assert_eq "split /boot partition" "/dev/targetp16" "$boot_part"
assert_eq "split /boot source" "fstab" "$boot_part_source"
assert_log_contains "Resolved /boot from $fstab to /dev/targetp16"

echo "fstab: PARTLABEL source with an escaped space"
printf '%s\n' 'PARTLABEL=boot\040fs	/boot	ext4	defaults	0 2' >"$fstab"
set_blkid_result 0 "/dev/targetp16" ""
resolved=$(resolve_fstab_mount_device /boot /dev/target "$fstab") ||
	fail "PARTLABEL /boot entry was not resolved"
assert_eq "PARTLABEL /boot device" "/dev/targetp16" "$resolved"

echo "fstab: /boot on another disk"
printf '%s\n' 'LABEL=BOOT	/boot	ext4	defaults	0 2' >"$fstab"
set_blkid_result 0 "/dev/otherp1" ""
if resolve_boot_partition "$fstab" >/dev/null 2>&1; then
	fail "off-target /boot was accepted"
fi
assert_log_contains \
	"Device /dev/otherp1 with LABEL=BOOT is not exclusively backed by image disk /dev/target"

echo "fstab: /boot inside the root filesystem"
printf '%s\n' 'LABEL=cloudimg-rootfs	/	ext4	defaults	0 1' >"$fstab"
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >/dev/null ||
	fail "resolve_boot_partition failed on a merged /boot image"
assert_eq "merged /boot partition" "" "$boot_part"
assert_eq "merged /boot source" "" "$boot_part_source"
assert_log_contains "$fstab has no /boot entry"

echo "fstab: device path source"
printf '%s\n' '/dev/sda2	/boot	ext4	defaults	0 2' >"$fstab"
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >/dev/null 2>"$stderr_file" ||
	fail "resolve_boot_partition failed on a device-path fstab source"
assert_eq "device-path partition" "/dev/target2" "$boot_part"
assert_eq "device-path source" "guess" "$boot_part_source"
assert_stderr_contains "names /boot by [/dev/sda2], which cannot be resolved on /dev/target"
assert_log_contains "Assuming /boot is /dev/target2, from the $fstab device path /dev/sda2"

echo "fstab: unsupported source"
printf '%s\n' 'none	/boot	tmpfs	defaults	0 0' >"$fstab"
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >/dev/null 2>"$stderr_file" ||
	fail "resolve_boot_partition failed on an unsupported fstab source"
assert_eq "unsupported-source fallback" "/dev/target1" "$boot_part"
assert_eq "unsupported-source source" "guess" "$boot_part_source"
assert_stderr_contains "names /boot by [none], which cannot be resolved on /dev/target"
assert_log_contains "Assuming /boot is /dev/target1"

echo "fstab: missing"
rm -f "$fstab"
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >"$temp_dir/stdout" 2>&1 ||
	fail "resolve_boot_partition failed without an fstab"
grep -Fq -- "No $fstab in the image" "$temp_dir/stdout" ||
	fail "missing fstab was not reported"
assert_eq "missing-fstab fallback" "/dev/target1" "$boot_part"
assert_eq "missing-fstab source" "guess" "$boot_part_source"
assert_log_contains "Assuming /boot is /dev/target1"

echo "bootfs_uuid overrides fstab"
printf '%s\n' 'LABEL=BOOT	/boot	ext4	defaults	0 2' >"$fstab"
bootfs_uuid=boot-override
set_blkid_result 0 "/dev/targetp2" ""
resolve_boot_partition "$fstab" >/dev/null ||
	fail "bootfs_uuid override failed"
assert_eq "override partition" "/dev/targetp2" "$boot_part"
assert_eq "override source" "override" "$boot_part_source"
assert_log_contains "Resolved /boot from bootfs_uuid=boot-override to /dev/targetp2"
bootfs_uuid=

echo "disk imaging identifier tests passed"
