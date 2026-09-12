#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"

mode_definition="$(
    sed -n '/^_resolve_nico_servicemonitors_mode()/,/^}/p' "${SETUP_SH}"
)"
if [[ -z "${mode_definition}" ]]; then
    echo "could not extract NICo ServiceMonitor mode resolver" >&2
    exit 1
fi
eval "${mode_definition}"

assert_mode() {
    local expected="$1"
    local core_installed_this_run="$2"
    local requested_mode="${3-__unset__}"
    local actual

    if [[ "${requested_mode}" == "__unset__" ]]; then
        actual="$(unset NICO_SERVICEMONITORS; \
            _resolve_nico_servicemonitors_mode "${core_installed_this_run}")"
    else
        actual="$(NICO_SERVICEMONITORS="${requested_mode}" \
            _resolve_nico_servicemonitors_mode "${core_installed_this_run}")"
    fi

    if [[ "${actual}" != "${expected}" ]]; then
        echo "expected mode ${expected}, got ${actual} " \
            "(core_installed=${core_installed_this_run}, requested=${requested_mode})" >&2
        exit 1
    fi
}

# A Core release installed from this checkout can be reconciled automatically,
# while callers can still request the existing hint or false modes explicitly.
assert_mode true true
assert_mode true true true
assert_mode hint true hint
assert_mode false true false

# An existing or skipped Core release must never be upgraded from this checkout.
# Even an inherited true value is reduced to the safe hint behavior.
assert_mode hint false
assert_mode hint false true
assert_mode hint false hint
assert_mode false false false

marker_init_line="$(grep -nF '_CORE_INSTALLED_THIS_RUN=false' "${SETUP_SH}" | cut -d: -f1)"
core_upgrade_line="$(grep -nF '(cd "${SCRIPT_DIR}/.." && "${NICO_CORE_CMD[@]}")' \
    "${SETUP_SH}" | cut -d: -f1)"
marker_set_line="$(grep -nF '_CORE_INSTALLED_THIS_RUN=true' "${SETUP_SH}" | cut -d: -f1)"
mode_call_line="$(grep -nF '"${_CORE_INSTALLED_THIS_RUN}")"' "${SETUP_SH}" | cut -d: -f1)"

if ! (( marker_init_line < core_upgrade_line && core_upgrade_line < marker_set_line && \
        marker_set_line < mode_call_line )); then
    echo "Core installation state must gate observability after the Core phase" >&2
    exit 1
fi

echo "setup observability Core gate tests passed"
