#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

###############################################################################
# Arguments
###############################################################################

BMC_IP="${1:-}"
BMC_USER="${2:-}"
BMC_PASS="${3:-}"
NVOS_IP="${4:-}"
NVOS_USER="${5:-}"
NVOS_PASS="${6:-}"
NVOS_FILE="${7:-}"

if [[ -z "$BMC_IP" || -z "$BMC_USER" || -z "$BMC_PASS" || \
      -z "$NVOS_IP" || -z "$NVOS_USER" || -z "$NVOS_PASS" || -z "$NVOS_FILE" ]]; then
    echo "Usage: $0 <BMC_IP> <BMC_USER> <BMC_PASSWORD> <NVOS_IP> <NVOS_USER> <NVOS_PASSWORD> <NVOS_FILE>"
    exit 1
fi

###############################################################################
# Configuration (override via environment variables)
###############################################################################

PING_TIMEOUT_SEC="${PING_TIMEOUT_SEC:-300}"       # max seconds to wait for NVOS to ping
PING_INTERVAL_SEC="${PING_INTERVAL_SEC:-5}"       # seconds between pings
EXTRA_NVOS_WAIT_SEC="${EXTRA_NVOS_WAIT_SEC:-60}"  # extra delay after NVOS is pingable

# Get script directory for relative script calls
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

###############################################################################
# Helper functions
###############################################################################

die() {
    echo "ERROR: $*" >&2
    exit 1
}

run_step() {
    local step_desc="$1"
    shift

    echo "=== START: $step_desc ==="
    if "$@"; then
        echo "=== OK: $step_desc ==="
    else
        local rc=$?
        echo "=== FAILED: $step_desc (exit code $rc) ===" >&2
        exit "$rc"
    fi
}

wait_for_ping() {
    local host="$1"
    local timeout="${2:-300}"
    local interval="${3:-5}"

    echo "Waiting for $host to become pingable (timeout ${timeout}s, interval ${interval}s)..."
    local elapsed=0

    while ! ping -c1 -W1 "$host" &>/dev/null; do
        if (( elapsed >= timeout )); then
            echo "Timeout waiting for $host to respond to ping after ${elapsed}s." >&2
            return 1
        fi
        sleep "$interval"
        elapsed=$((elapsed + interval))
    done

    echo "$host is reachable."
    return 0
}

###############################################################################
# Pre-flight checks
###############################################################################

[[ -f "$NVOS_FILE" ]] || die "NVOS file not found: $NVOS_FILE"

# Check for required scripts (in same directory or PATH)
if [[ -x "${SCRIPT_DIR}/nvswpwrcyc.sh" ]]; then
    PWRCYC_SCRIPT="${SCRIPT_DIR}/nvswpwrcyc.sh"
elif command -v nvswpwrcyc.sh >/dev/null 2>&1; then
    PWRCYC_SCRIPT="nvswpwrcyc.sh"
else
    die "nvswpwrcyc.sh not found in ${SCRIPT_DIR} or PATH"
fi

if [[ -x "${SCRIPT_DIR}/nvswupdNVOS.sh" ]]; then
    NVOS_SCRIPT="${SCRIPT_DIR}/nvswupdNVOS.sh"
elif command -v nvswupdNVOS.sh >/dev/null 2>&1; then
    NVOS_SCRIPT="nvswupdNVOS.sh"
else
    die "nvswupdNVOS.sh not found in ${SCRIPT_DIR} or PATH"
fi

###############################################################################
# NVOS Update Sequence
###############################################################################

run_step "Power cycle switch via BMC $BMC_IP" \
    "$PWRCYC_SCRIPT" "$BMC_IP" "$BMC_USER" "$BMC_PASS"

run_step "Wait for NVOS $NVOS_IP to become reachable" \
    wait_for_ping "$NVOS_IP" "$PING_TIMEOUT_SEC" "$PING_INTERVAL_SEC"

run_step "Extra wait ${EXTRA_NVOS_WAIT_SEC}s for services to start" \
    sleep "$EXTRA_NVOS_WAIT_SEC"

run_step "Update NVOS on $NVOS_IP" \
    "$NVOS_SCRIPT" "$NVOS_IP" "$NVOS_USER" "$NVOS_PASS" "$NVOS_FILE"

echo "=== NVOS bundle update completed successfully ==="
