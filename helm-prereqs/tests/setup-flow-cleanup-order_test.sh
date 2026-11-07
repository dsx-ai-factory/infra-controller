#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"

if [[ "$(grep -Fc '"${SCRIPT_DIR}/cleanup-legacy-flow-managers.sh"' "${SETUP_SH}")" -ne 1 ]]; then
    echo "setup.sh must invoke legacy manager cleanup exactly once" >&2
    exit 1
fi

if ! guard_line="$(grep -nxF '_reject_bundled_flow_manager_upgrade' "${SETUP_SH}" | cut -d: -f1)"; then
    echo "setup.sh must invoke _reject_bundled_flow_manager_upgrade on its own line" >&2
    exit 1
fi
preflight_line="$(grep -nF 'source "${SCRIPT_DIR}/preflight.sh"' "${SETUP_SH}" | cut -d: -f1)"
first_install_line="$(grep -nF 'helmfile sync -l name=postgres-operator' "${SETUP_SH}" | cut -d: -f1)"
core_upgrade_line="$(grep -nF '(cd "${SCRIPT_DIR}/.." && "${NICO_CORE_CMD[@]}")' "${SETUP_SH}" | cut -d: -f1)"
cleanup_line="$(grep -nF '"${SCRIPT_DIR}/cleanup-legacy-flow-managers.sh"' "${SETUP_SH}" | cut -d: -f1)"
rest_skip_line="$(grep -nF 'if "${SKIP_REST}"; then' "${SETUP_SH}" | head -1 | cut -d: -f1)"

# Flow is mandatory (no SKIP_FLOW / --skip-flow) - only REST's skip exit needs
# to stay after cleanup.
if ! (( guard_line < preflight_line && preflight_line < first_install_line && \
        first_install_line < core_upgrade_line && core_upgrade_line < cleanup_line && \
        cleanup_line < rest_skip_line )); then
    echo "legacy cleanup must run after guard, preflight, and Core, but before the REST skip exit" >&2
    exit 1
fi

echo "setup Flow cleanup ordering test passed"
