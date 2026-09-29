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

#
# Parses a json file containing an array of `fqdn -> ip-addr` mappings at the
# given dot-delimited json key (such as "a.b.c") and adds them to /etc/hosts if
# they don't already exist. The mappings in the specified array are assumed to
# be in this form:
#
#   {
#      "fqdn": "some.fqdn",
#      "ip": "10.0.0.1"
#   },
#

JSON_FILE="$1"
JSON_KEY="${2:-endpoints}"
HOSTS_FILE="/etc/hosts"
NEWLINE_ADDED=false

if [[ ! -f "$JSON_FILE" ]]; then
    echo "file not found: $JSON_FILE" >&2
    exit 1
fi

if ! jq -e --arg key "$JSON_KEY" 'has($key)' "$JSON_FILE" > /dev/null; then
    echo "'$JSON_KEY' not found in $JSON_FILE" >&2
    exit 1
fi

jq -r --arg key "$JSON_KEY" '
    .[$key][] | "\(.ip) \(.fqdn)"
' "$JSON_FILE" | while read -r ip fqdn; do
    # Trim whitespace
    fqdn=$(echo "$fqdn" | xargs)
    ip=$(echo "$ip" | xargs)

    [[ -z "$fqdn" ]] && continue
    [[ -z "$ip" ]] && continue

    # Keep each address for a name so dual-stack endpoints retain both families.
    if ! awk -v ip="$ip" -v fqdn="$fqdn" '
        $1 == ip {
            for (i = 2; i <= NF && $i !~ /^#/; i++) {
                if ($i == fqdn) found = 1
            }
        }
        END { exit !found }
    ' "$HOSTS_FILE"; then
        echo "Adding $ip $fqdn to $HOSTS_FILE"
        if [[ $NEWLINE_ADDED == "false" ]]; then
            echo >> $HOSTS_FILE
            NEWLINE_ADDED=true
        fi
        printf "%s %s\n" "$ip" "$fqdn" >> "$HOSTS_FILE"
    else
        echo "Entry for $ip $fqdn already exists in $HOSTS_FILE"
    fi
done
