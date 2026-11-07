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

HOSTS_FILE="/etc/hosts"
HOSTNAME="$1"

if [[ -z "${HOSTNAME}" ]]; then
    echo "Usage: $0 <hostname>" >&2
    exit 1
fi

PATTERN='^127\.0\.0\.1[[:space:]]+localhost\.localdomain[[:space:]]+localhost'
PATTERN+='(\s|$).*'"${HOSTNAME}"'\b'

# If the hostname is already present on the 127.0.0.1 localhost line, nothing to do
if grep -qE "$PATTERN" "$HOSTS_FILE"; then
    echo "$HOSTNAME is already aliased to 127.0.0.1, nothing to do."
    exit 0
fi

# Otherwise, append it to that line
tmp="$(mktemp)"
awk -v h="$HOSTNAME" '
    $1 == "127.0.0.1" && $2 == "localhost.localdomain" && $3 == "localhost" {
        # append hostname once
        print $0, h
        next
    }
    { print }
' "$HOSTS_FILE" > "$tmp"

cp "$tmp" "$HOSTS_FILE"
rm -f "$tmp"
