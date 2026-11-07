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

# Script to check if all NVMe devices are writeable
# Returns exit code 0 (true) if all NVMe devices are writeable, 1 (false) otherwise

# Get all NVMe devices
nvme_devices=$(lsblk -d -o NAME,TYPE,RO | grep -i nvme | grep disk)

echo "$nvme_devices"
# Check if any are read-only
readonly_nvme=$(echo "$nvme_devices" | grep '1$')

if [ -n "$readonly_nvme" ]; then
    echo "Found read-only NVMe devices:" >&2
    echo "$readonly_nvme" >&2
    exit 1
else
    echo "All NVMe devices are writeable"
    exit 0
fi
