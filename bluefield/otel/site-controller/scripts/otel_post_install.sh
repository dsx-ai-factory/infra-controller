#!/usr/bin/env bash
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

set -euo pipefail

SITE_FILE=/etc/site-dpu.json

if [[ ! -f "$SITE_FILE" ]]; then
    echo "file not found: $SITE_FILE" >&2
    exit 1
fi

SITE=$(jq -r '.site // empty' "$SITE_FILE")

if [[ -z $SITE ]]; then
    echo "'site' value missing from $SITE_FILE"
    exit 1
fi

DOMAIN=$(jq -r '.domain // empty' "$SITE_FILE")

if [[ -z $DOMAIN ]]; then
    echo "'domain' value missing from $SITE_FILE"
    exit 1
fi

if ! jq -e '
    has("endpoints") and (.endpoints | type == "array")
' $SITE_FILE > /dev/null; then
    echo "'endpoints' is missing from $SITE_FILE or is not an array"
    exit 1
fi

# Prefer IPv4 and match the hostname format used by `nico-api` IP-based naming.
# Temporary or unusable addresses must not rename the DPU.
HOST_LABEL=$(sudo ip -json addr show oob_net0 | jq -r '
    [.[].addr_info[] | select(
        (.family == "inet" or .family == "inet6") and .scope == "global" and
        .temporary != true and .tentative != true and
        .dadfailed != true and .deprecated != true
    )] | .[].local
' | python3 -c '
import ipaddress
import sys

addresses = [ipaddress.ip_address(line.strip()) for line in sys.stdin]
if addresses:
    address = min(addresses, key=lambda ip: (ip.version, int(ip)))
    print(address.exploded.replace(".", "-").replace(":", "-"))
')
if [[ -z "$HOST_LABEL" ]]; then
    echo "no usable address found on oob_net0" >&2
    exit 1
fi

EXPECTED_HOSTNAME="$HOST_LABEL.$SITE.$DOMAIN"
# `hostnamectl` can truncate names beyond Linux's static hostname limit.
if (( ${#EXPECTED_HOSTNAME} > 64 )); then
    echo "hostname exceeds Linux's 64-character limit: $EXPECTED_HOSTNAME" >&2
    exit 1
fi
ACTUAL_HOSTNAME=$(hostname)
SCRIPT_DIR=/usr/local/sbin

if [[ "$EXPECTED_HOSTNAME" != "$ACTUAL_HOSTNAME" ]]; then
    hostnamectl set-hostname "$EXPECTED_HOSTNAME"
    "$SCRIPT_DIR"/localhost_alias.sh "$EXPECTED_HOSTNAME"
fi

"$SCRIPT_DIR"/map_endpoints.sh /etc/site-dpu.json
