#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/kubectl.log"
export TEST_LOG

guard_definition="$(
    sed -n '/^_reject_bundled_flow_manager_upgrade()/,/^}/p' "${SETUP_SH}"
)"
if [[ -z "${guard_definition}" ]]; then
    echo "could not extract bundled manager upgrade guard" >&2
    exit 1
fi
eval "${guard_definition}"

mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_LOG}"

if [[ "$*" == *'get deployment flow -n flow --ignore-not-found'* ]]; then
    case "${TEST_SCENARIO}" in
        error_deployment) exit 1 ;;
        absent|absent_old_pod) ;;
        psm) printf 'flow\npsm\n' ;;
        nsm) printf 'flow\nnsm\n' ;;
        both) printf 'flow\npsm\nnsm\n' ;;
        *) printf 'flow\n' ;;
    esac
elif [[ "$*" == *'get deployment flow -n flow -o jsonpath='* ]]; then
    case "${TEST_SCENARIO}" in
        error_rollout) exit 1 ;;
        rollout_incomplete) printf '2|1|1|1|1|1|2' ;;
        rollout_scaled_zero) printf '2|2|0||||' ;;
        *) printf '2|2|1|1|1|1|1' ;;
    esac
elif [[ "$*" == *'get pods -n flow -l app=flow'* ]]; then
    case "${TEST_SCENARIO}" in
        error_pods) exit 1 ;;
        absent) ;;
        old_pod|absent_old_pod) printf 'flow-old\tRunning\tflow psm nsm \n' ;;
        terminal_old_pod) printf 'flow-old\tFailed\tflow psm nsm \n' ;;
        *) printf 'flow-new\tRunning\tflow \n' ;;
    esac
fi
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"

run_guard() {
    TEST_SCENARIO="$1" PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        _reject_bundled_flow_manager_upgrade
}

for supported in absent flow rollout_scaled_zero terminal_old_pod; do
    : > "${TEST_LOG}"
    run_guard "${supported}"
    grep -Fqx -- \
        'get deployment flow -n flow --ignore-not-found -o jsonpath={range .spec.template.spec.containers[*]}{.name}{"\n"}{end}' \
        "${TEST_LOG}"
done

for unsupported in psm nsm both; do
    : > "${TEST_LOG}"
    if guard_output="$(run_guard "${unsupported}" 2>&1)"; then
        echo "guard accepted bundled manager scenario: ${unsupported}" >&2
        exit 1
    fi
    if ! grep -Fq -- \
        'upgrade only the flow Helm release to the Flow-only chart' \
        <<< "${guard_output}"; then
        echo "guard did not provide the manual Flow overwrite action" >&2
        exit 1
    fi
done

for unsafe in rollout_incomplete old_pod absent_old_pod; do
    if guard_output="$(run_guard "${unsafe}" 2>&1)"; then
        echo "guard accepted unsafe rollout scenario: ${unsafe}" >&2
        exit 1
    fi
    if ! grep -Eq -- 'rollout is incomplete|active Flow Pods still contain' \
        <<< "${guard_output}"; then
        echo "guard did not explain unsafe rollout scenario: ${unsafe}" >&2
        exit 1
    fi
done

for error_scenario in error_deployment error_rollout error_pods; do
    if (run_guard "${error_scenario}"); then
        echo "guard accepted inspection failure: ${error_scenario}" >&2
        exit 1
    fi
done

if ! grep -Fq -- 'get pods -n flow -l app=flow --ignore-not-found' "${TEST_LOG}"; then
    echo "guard did not inspect actual Flow Pods" >&2
    exit 1
fi

preflight_line="$(grep -nF 'source "${SCRIPT_DIR}/preflight.sh"' "${SETUP_SH}" | cut -d: -f1)"
if ! guard_line="$(grep -nxF '_reject_bundled_flow_manager_upgrade' "${SETUP_SH}" | cut -d: -f1)"; then
    echo "setup.sh must invoke _reject_bundled_flow_manager_upgrade on its own line" >&2
    exit 1
fi
first_install_line="$(grep -nF 'helmfile sync -l name=postgres-operator' "${SETUP_SH}" | cut -d: -f1)"
if ! (( guard_line < preflight_line && preflight_line < first_install_line )); then
    echo "bundled manager guard must run before preflight and installation phases" >&2
    exit 1
fi

echo "setup legacy Flow upgrade guard tests passed"
