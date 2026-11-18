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

set -o pipefail

MIN_UPTIME=86400
ROOTFS_INFO_FILE="/rootfs_info.txt"

get_last_modified() {
    awk '
        tolower($0) ~ /^last-modified:/ {
            sub(/^[^:]*:[ \t]*/, "")
            sub(/[ \t\r]*$/, "")
            value = $0
        }
        END {
            if (value == "") exit 1
            print value
        }
    ' "$@"
}

# If we've not reached the minimum uptime, exit straight away
uptime=$(awk -F. '{print $1}' < /proc/uptime)
if (( $uptime < $MIN_UPTIME ))
then
	echo "Min uptime not reached ($uptime < $MIN_UPTIME)"
	exit 0
fi

# Get the last updated value for the main scout image.
# Use the PXE URL from the kernel command line if available (supports
# external hosts that can't resolve internal hostnames), otherwise fall
# back to the default internal hostname.
pxe_uri=$(sed 's/ /\n/g' /proc/cmdline | grep '^pxe_uri=' | cut -d'=' -f2)
static_pxe_base_url=${pxe_uri:-http://carbide-static-pxe.forge}
arch=$(uname -m)
scout_url="${static_pxe_base_url}/public/blobs/internal/${arch}/scout.squashfs"
www_last_modified_str=$(curl -sf --max-time 30 --head "${scout_url}" 2>/dev/null | get_last_modified)
if (( $? != 0 ))
then
	echo "Unable to query last-modified value for ${scout_url}"
	exit 1
fi

# Get the last updated value from when we booted
# This should have been stored by the scout-loader script
my_last_modified_str=$(get_last_modified "${ROOTFS_INFO_FILE}")
if (( $? != 0 ))
then
	echo "Unable to query last-modified value from when we booted"
	echo "Scout loader should have put that in ${ROOTFS_INFO_FILE}"
	exit 1
fi

if [ "${www_last_modified_str}" != "${my_last_modified_str}" ]
then
	echo "Newer scout available, rebooting"
	reboot
fi
