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

# Requires GNU bash 4.3 or newer: the script under test uses `local -n`
# namerefs, which Apple's default /bin/bash 3.2 does not support.

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script_under_test="$script_dir/disk_imaging.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

source "$script_under_test"

log_output="$temp_dir/disk-imaging.log"
mock_blkid_status=0
mock_blkid_stdout=
mock_blkid_stderr=
mock_lsblk_scan=
mock_lsblk_esp_disk=

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

# Verifies authentication config files are owner-only so credentials are not
# exposed to other local users.
assert_file_mode_600() {
	local description=$1
	local file=$2

	if [ -z "$(find "$file" -maxdepth 0 -perm 600 2>/dev/null)" ]; then
		fail "$description: $file is not mode 600"
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
	case "$2:${3-}" in
		NAME,SIZE,TYPE:)
			printf '%s\n' "$mock_lsblk_scan"
			;;
		PARTTYPE:*)
			if [ "${3-}" == "$mock_lsblk_esp_disk" ]; then
				printf '%s\n' "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
			else
				printf '%s\n' "0fc63daf-8483-4772-8e79-3d69d8477de4"
			fi
			;;
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

echo "bootdisk selection"
mock_lsblk_scan=$'/dev/vda 10737418240 disk'
mock_lsblk_esp_disk=
image_disk=
find_bootdisk
assert_eq "blank sole virtio disk is selected" "/dev/vda" "$image_disk"

mock_lsblk_scan=$'/dev/sda 21474836480 disk\n/dev/nvme0n1 10737418240 disk'
mock_lsblk_esp_disk=/dev/sda
image_disk=
find_bootdisk
assert_eq "EFI disk wins over the deterministic order" "/dev/sda" "$image_disk"

mock_lsblk_scan=$'/dev/sda 10737418240 disk\n/dev/nvme0n1 21474836480 disk'
mock_lsblk_esp_disk=
image_disk=
find_bootdisk
assert_eq "version-sorted name order is preserved without EFI" "/dev/nvme0n1" "$image_disk"

mock_lsblk_scan=$'/dev/mmcblk0 8589934592 disk\n/dev/nvme0n1 21474836480 disk'
mock_lsblk_esp_disk=
image_disk=
find_bootdisk
assert_eq "fallback skips non-selector disks such as eMMC" "/dev/nvme0n1" "$image_disk"

mock_lsblk_scan=$'/dev/vda 10737418240 disk'
mock_lsblk_esp_disk=
image_disk=smallest
find_bootdisk
assert_eq "smallest selector picks the sole disk" "/dev/vda" "$image_disk"

mock_lsblk_scan=
mock_lsblk_esp_disk=
image_disk=
if ( find_bootdisk ) >/dev/null 2>&1; then
	fail "bootdisk selection accepted an empty disk inventory"
fi
assert_log_contains "Boot drive not detected or specified"

# Validates the explicit-destination curl contract and creates synthetic output
# only for a valid invocation so pipeline status masking cannot hide a failure.
curl() {
	local destination=
	local destination_count=0
	local request=
	local request_count=0

	# Accept the production command's fixed transport options while rejecting
	# remote naming, malformed operands, and unrelated arguments.
	while [ "$#" -gt 0 ]; do
		case "$1" in
			--retry)
				if [ "$#" -lt 2 ]; then
					return 2
				fi
				shift 2
				continue
				;;
			--retry-all-errors|-k|-L)
				shift
				continue
				;;
			-O|--remote-name)
				return 2
				;;
			--output)
				if [ "$#" -lt 2 ]; then
					return 2
				fi
				shift
				destination=$1
				destination_count=$((destination_count + 1))
				;;
			http://*|https://*)
				request=$1
				request_count=$((request_count + 1))
				;;
			*)
				return 2
				;;
		esac
		shift
	done

	if [ "$destination_count" -ne 1 ] || [ "$request_count" -ne 1 ]; then
		return 2
	fi

	# Publish captures and output only after the complete invocation validates.
	printf '%s' "$request" >"$temp_dir/curl-request"
	printf '%s' "$destination" >"$temp_dir/curl-destination"
	printf '%s\n' "synthetic image content" >"$destination"
}

# Exercises production filename selection and download wiring in an isolated
# directory so the test cannot create image files in the repository.
run_download_tests() (
	local -a descriptions=()
	local -a urls=()
	local -a initial_files=()
	local -a expected_files=()
	local actual_request
	local actual_destination
	local index
	local download_dir="$temp_dir/downloads"

	mkdir "$download_dir"
	cd "$download_dir" || fail "could not enter temporary download directory"

	# A query-free URL retains the path basename used by existing callers.
	descriptions+=("query-free URL")
	urls+=("https://images.example/os/image.qcow2")
	initial_files+=("")
	expected_files+=("image.qcow2")

	# A simple query remains part of the request but not the local filename.
	descriptions+=("simple query")
	urls+=("https://images.example/os/image.qcow2?token=synthetic")
	initial_files+=("")
	expected_files+=("image.qcow2")

	# A slash in the query cannot be interpreted as part of the local path.
	descriptions+=("slash in query")
	urls+=("https://images.example/os/image.qcow2?token=segment/part")
	initial_files+=("")
	expected_files+=("image.qcow2")

	# A distro-selected name remains authoritative for checksum lookup.
	descriptions+=("preselected distro filename")
	urls+=("https://images.example/releases/source.img")
	initial_files+=("checksum-selected.img")
	expected_files+=("checksum-selected.img")

	for index in "${!urls[@]}"; do
		echo "download: ${descriptions[$index]}"
		file=${initial_files[$index]}
		rm -f "$temp_dir/curl-request" \
			"$temp_dir/curl-destination"

		# Invoke the production path that selects the filename and calls curl.
		curl_url "${urls[$index]}"
		actual_request=$(<"$temp_dir/curl-request")
		actual_destination=$(<"$temp_dir/curl-destination")

		# The full request and selected destination must agree with the file
		# subsequently consumed by checksum verification and conversion.
		assert_eq \
			"${descriptions[$index]} selected filename" \
			"${expected_files[$index]}" \
			"$file"
		assert_eq \
			"${descriptions[$index]} request URL" \
			"${urls[$index]}" \
			"$actual_request"
		assert_eq \
			"${descriptions[$index]} curl destination" \
			"$file" \
			"$actual_destination"
		if [ ! -f "$file" ]; then
			fail "${descriptions[$index]} did not create [$file]"
		fi
		assert_eq \
			"${descriptions[$index]} downloaded content" \
			"synthetic image content" \
			"$(<"$file")"
		rm -f "$file"
	done
)

run_download_tests || exit $?

echo "image auth header construction"
image_url="https://images.example/image.qcow2"

image_auth_token=""
image_auth_type=""
build_image_auth_args
assert_eq "no-token auth arg count" "0" "${#image_auth[@]}"
assert_eq "no-token header file" "" "$image_auth_header_file"

image_auth_token="my-token"
image_auth_type=""
build_image_auth_args
assert_eq "default auth type" "Bearer" "$image_auth_type"
assert_eq "auth arg count" "2" "${#image_auth[@]}"
assert_eq "auth flag" "-K" "${image_auth[0]}"
assert_eq "auth config file argument matches the header file" \
	"$image_auth_header_file" "${image_auth[1]}"
assert_file_mode_600 "auth header file" "$image_auth_header_file"
assert_eq "auth header file content" \
	'header = "Authorization: Bearer my-token"' "$(<"$image_auth_header_file")"
rm -f "$image_auth_header_file"

image_auth_token="my-token"
image_auth_type="Basic"
build_image_auth_args
assert_eq "explicit auth type is not overridden" "Basic" "$image_auth_type"
assert_eq "basic auth header file content" \
	'header = "Authorization: Basic my-token"' "$(<"$image_auth_header_file")"
rm -f "$image_auth_header_file"

image_auth_type="Token"
build_image_auth_args
assert_eq "token auth header file content" \
	'header = "Authorization: Token my-token"' "$(<"$image_auth_header_file")"
rm -f "$image_auth_header_file"

echo "image auth header escaping"
# A token containing curl config-file special characters: a double quote and
# a backslash. If these are not escaped, the config file either breaks curl's
# quoted-value parsing or changes the header curl actually sends.
image_auth_token='weird"token\with-specials'
image_auth_type="Bearer"
build_image_auth_args
assert_eq "escaped header file content" \
	'header = "Authorization: Bearer weird\"token\\with-specials"' \
	"$(<"$image_auth_header_file")"
rm -f "$image_auth_header_file"

echo "image auth header file creation failure"
# mktemp failing (e.g. a full or read-only /tmp) must stop the download
# rather than silently falling back to an unauthenticated fetch.
mktemp() {
	return 1
}
image_auth_token="my-token"
image_auth_type="Bearer"
if build_image_auth_args; then
	fail "build_image_auth_args accepted a failed mktemp"
fi
assert_eq "failed mktemp leaves auth empty" "0" "${#image_auth[@]}"
unset -f mktemp

echo "image auth header file cleanup when a later step fails"
# mktemp can succeed while chmod or the write still fails (e.g. a quota or a
# filesystem that rejects the mode change). The partially-created file must
# not be left behind.
known_auth_file="$temp_dir/chmod-fail-header"
mktemp() {
	: >"$known_auth_file"
	printf '%s\n' "$known_auth_file"
}
chmod() {
	return 1
}
image_auth_token="my-token"
image_auth_type="Bearer"
if build_image_auth_args; then
	fail "build_image_auth_args accepted a failed chmod"
fi
assert_eq "chmod failure clears the header file variable" "" "$image_auth_header_file"
if [ -e "$known_auth_file" ]; then
	fail "build_image_auth_args left the temp file behind after chmod failed"
fi
unset -f chmod
unset -f mktemp

echo "curl_url forwards the url and the auth config as separate arguments, never the raw token"
# curl_url pipes through tee, so curl runs in a subshell: capture its
# arguments via a file rather than a variable, which would not survive
# back to this shell.
mock_curl_args_file="$temp_dir/curl_args"
# Records curl's argument vector outside the pipeline subshell for assertions.
curl() {
	printf '%s\n' "$@" >"$mock_curl_args_file"
	return 0
}
# Reloads the recorded argument vector without changing argument boundaries.
read_mock_curl_args() {
	mock_curl_args=()
	while IFS= read -r line; do
		mock_curl_args+=("$line")
	done <"$mock_curl_args_file"
}

file=
image_url="https://images.example/image.qcow2"
image_auth=()
curl_url "$image_url"
read_mock_curl_args
assert_eq "unauthenticated curl arg count" "8" "${#mock_curl_args[@]}"
assert_eq "unauthenticated curl output flag" "--output" "${mock_curl_args[5]}"
assert_eq "unauthenticated curl output filename" "$file" "${mock_curl_args[6]}"
assert_eq "unauthenticated curl url is the last argument" "$image_url" "${mock_curl_args[7]}"

file=
query_marker="synthetic-query-marker"
image_url="https://images.example/image.qcow2?token=$query_marker"
image_auth_token="my-token"
image_auth_type="Bearer"
build_image_auth_args

# Exercise the production announcement independently of main so no hardware
# discovery or imaging can run.
: >"$log_output"
announcement_stdout=$(announce_image_download)
assert_eq "download announcement stdout" "Downloading image" "$announcement_stdout"
case "$announcement_stdout" in
	*"$query_marker"*) fail "download announcement exposed the URL query on stdout" ;;
esac
if grep -Fq -- "$query_marker" "$log_output"; then
	fail "download announcement exposed the URL query in the imaging log"
fi

curl_url "$image_url" "${image_auth[@]}"
read_mock_curl_args
assert_eq "authenticated query selects the path filename" "image.qcow2" "$file"
assert_eq "authenticated curl arg count" "10" "${#mock_curl_args[@]}"
assert_eq "authenticated curl auth flag" "-K" "${mock_curl_args[5]}"
assert_eq "authenticated curl auth value is the header file, not the token" \
	"$image_auth_header_file" "${mock_curl_args[6]}"
assert_eq "authenticated curl output flag" "--output" "${mock_curl_args[7]}"
assert_eq "authenticated curl output filename" "$file" "${mock_curl_args[8]}"
assert_eq "authenticated curl url preserves the query" "$image_url" "${mock_curl_args[9]}"
for arg in "${mock_curl_args[@]}"; do
	case "$arg" in
		*my-token*) fail "token leaked into curl's argument list: $arg" ;;
	esac
done
rm -f "$image_auth_header_file"

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
if resolve_boot_partition "$fstab" >/dev/null 2>"$stderr_file"; then
	fail "device-path /boot source was accepted"
fi
assert_eq "device-path partition" "" "$boot_part"
assert_eq "device-path source" "" "$boot_part_source"
assert_stderr_contains "names /boot by [/dev/sda2], which cannot be resolved on /dev/target"

echo "fstab: unsupported source"
printf '%s\n' 'none	/boot	tmpfs	defaults	0 0' >"$fstab"
set_blkid_result 2 "" ""
if resolve_boot_partition "$fstab" >/dev/null 2>"$stderr_file"; then
	fail "unsupported /boot source was accepted"
fi
assert_eq "unsupported-source partition" "" "$boot_part"
assert_stderr_contains "names /boot by [none], which cannot be resolved on /dev/target"

echo "fstab: missing"
rm -f "$fstab"
root_dev=/dev/target3
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >/dev/null ||
	fail "resolve_boot_partition failed without an fstab"
assert_eq "missing-fstab fallback" "/dev/target1" "$boot_part"
assert_eq "missing-fstab source" "default" "$boot_part_source"
assert_log_contains "No $fstab in the image; defaulting /boot to /dev/target1"

echo "fstab: missing and partition 1 is root"
root_dev=/dev/target1
set_blkid_result 2 "" ""
resolve_boot_partition "$fstab" >/dev/null ||
	fail "resolve_boot_partition failed without an fstab when partition 1 is root"
assert_eq "root-on-p1 partition" "" "$boot_part"
assert_eq "root-on-p1 source" "" "$boot_part_source"
assert_log_contains "/dev/target1 is the root filesystem; /boot stays on it"
root_dev=

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
