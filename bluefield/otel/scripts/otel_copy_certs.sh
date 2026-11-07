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

# Copy otel's mTLS certs from forge-dpu-agent.

# Number of days before otel certs are considered "old" (should have beeen
# renewed by now if the the mTLS renewal agent is running properly).
MAX_AGE_DAYS=10
MAX_SNAPSHOT_ATTEMPTS=5

SRC_CA="/opt/forge/forge_root.pem"
SRC_CERT="/opt/forge/machine_cert.pem"
SRC_KEY="/opt/forge/machine_cert.key"

DST_CA="/etc/otelcol-contrib/certs/ca.pem"
DST_CERT="/etc/otelcol-contrib/certs/otel-cert.pem"
DST_KEY="/etc/otelcol-contrib/certs/private/otel-key.pem"

# Get modification time of a file in seconds since epoch
file_mtime() {
    stat -c %Y "$1"
}

# Return 0 (true) if any dest file is missing.
any_missing() {
    [[ ! -f "$DST_CA" || ! -f "$DST_CERT" || ! -f "$DST_KEY" ]]
}

# Return 0 (true) if any dest file is older than MAX_AGE_DAYS.
any_too_old() {
    local now_ts dest_ts age_seconds max_age_seconds
    now_ts=$(date +%s)
    max_age_seconds=$(( MAX_AGE_DAYS * 24 * 60 * 60 ))

    for f in "$DST_CA" "$DST_CERT" "$DST_KEY"; do
        [[ -f "$f" ]] || continue
        dest_ts=$(file_mtime "$f")
        age_seconds=$(( now_ts - dest_ts ))
        if (( age_seconds > max_age_seconds )); then
            return 0
        fi
    done

    return 1
}

# Try to capture a consistent snapshot of the three source files into temp files.
# Return 0 on success (temps created), non-zero on failure after retries.
#
# Snapshot logic:
# 1. Record mtimes of all three source files.
# 2. Copy all three to temp files.
# 3. Re‑read mtimes. If any changed, discard all temp files and retry until
#    mtimes match before and after.
#
# There's no way to make the copy of all three mTLS files truly atomic, but the
# snapshot ensures self consistency for all but a tiny, self-correcting window
# across three separate `mv` calls, when the OpenTelemetry collector can still
# see mixtures like “two new, one old,” depending on timing. Given otel’s retry
# behavior, this is fine operationally.
#
# If the number of attempts exceeds the retry limit, then there's a problem
# with the source cert renewal frequency that needs its own fix anyway.
snapshot_sources() {
    local tmp_root="$1"
    local tmp_cert="$2"
    local tmp_key="$3"

    local attempt=1
    local src_root_m1 src_cert_m1 src_key_m1
    local src_root_m2 src_cert_m2 src_key_m2

    while (( attempt <= MAX_SNAPSHOT_ATTEMPTS )); do
        # Record initial mtimes
        src_root_m1=$(file_mtime "$SRC_CA")
        src_cert_m1=$(file_mtime "$SRC_CERT")
        src_key_m1=$(file_mtime "$SRC_KEY")

        # Copy to temp files
        install -D -m 600 "$SRC_CA" "$tmp_root"
        install -D -m 600 "$SRC_CERT" "$tmp_cert"
        install -D -m 600 "$SRC_KEY" "$tmp_key"

        # Re-read mtimes
        src_root_m2=$(file_mtime "$SRC_CA")
        src_cert_m2=$(file_mtime "$SRC_CERT")
        src_key_m2=$(file_mtime "$SRC_KEY")

        # If nothing changed, we have a consistent snapshot
        if [[ "$src_root_m1" == "$src_root_m2" ]] &&
           [[ "$src_cert_m1" == "$src_cert_m2" ]] &&
           [[ "$src_key_m1" == "$src_key_m2" ]]; then
            return 0
        fi

        # Otherwise, retry
        attempt=$(( attempt + 1 ))
        sleep 1
    done

    return 1
}

# Only proceed if all sources exist.
if [[ -f "$SRC_CA" && -f "$SRC_CERT" && -f "$SRC_KEY" ]]; then
    if any_missing || any_too_old; then
        tmp_root="${DST_CA}.tmp.$$"
        tmp_cert="${DST_CERT}.tmp.$$"
        tmp_key="${DST_KEY}.tmp.$$"

        if snapshot_sources "$tmp_root" "$tmp_cert" "$tmp_key"; then
            # Atomically replace each destination with its temp
            mv -f "$tmp_root" "$DST_CA"
            mv -f "$tmp_cert" "$DST_CERT"
            mv -f "$tmp_key" "$DST_KEY"
        else
            echo "Failed to create a consistent snapshot of mTLS certs " \
                 "after $MAX_SNAPSHOT_ATTEMPTS attempts" >&2
            rm -f "$tmp_root" "$tmp_cert" "$tmp_key"
            exit 1
        fi
    fi
fi

# Fail if the required certs are still missing.
if any_missing; then
    echo "Failed to copy mTLS certs from forge-dpu-agent"
    exit 1
fi
