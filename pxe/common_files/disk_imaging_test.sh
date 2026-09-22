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

echo "disk imaging identifier tests passed"
