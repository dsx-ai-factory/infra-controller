#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

# Exercise the actual setup block, including its restart and failure handling.
connection_check="$(sed -n "/^# Verify the site-agent's gRPC connection/,/^echo \"\"$/p" \
    "${SCRIPT_DIR}/../setup.sh")"
[[ -n "${connection_check}" ]] || { echo "could not extract connection check" >&2; exit 1; }
printf 'set -euo pipefail\n%s\necho "check completed"\n' "${connection_check}" \
    > "${TEST_TMP_DIR}/check.sh"

mkdir "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${CASE_DIR}/calls"
case "$*" in
    'get pods -n nico-rest -l app.kubernetes.io/name=nico-rest-site-agent -o name')
        count="$(cat "${CASE_DIR}/polls")"
        count=$((count + 1))
        printf '%s\n' "${count}" > "${CASE_DIR}/polls"
        # Discovery errors and an absent pod are transient, not fatal.
        if [[ "${SCENARIO}" == recovered ]]; then
            [[ "${count}" -ne 1 ]] || exit 1
            [[ "${count}" -ne 2 ]] || exit 0
        fi
        echo 'pod/nico-rest-site-agent-0'
        ;;
    'logs -n nico-rest pod/nico-rest-site-agent-0 --since=5m')
        count="$(cat "${CASE_DIR}/polls")"
        if [[ "${SCENARIO}" == healthy ]]; then
            echo 'CoreGrpcClient: Successfully connected to server'
            # A large stream must not produce a false failure under pipefail.
            awk 'BEGIN { for (i = 0; i < 10000; i++) print "subsequent inventory log entry" }'
        elif [[ "${SCENARIO}" == log-error ]]; then
            echo 'CoreGrpcClient: Successfully connected to server'
            exit 1
        elif [[ "${SCENARIO}" == recovered && -f "${CASE_DIR}/rolled-out" && "${count}" -ge 27 ]]; then
            echo 'CoreGrpcClient: Successfully connected to server'
        elif [[ "${SCENARIO}" == recovered && "${count}" -eq 3 ]]; then
            exit 1
        else
            # A different client's success must not satisfy the Core check.
            echo 'FlowGrpcClient: Successfully connected to server'
        fi
        ;;
    'rollout restart statefulset/nico-rest-site-agent -n nico-rest')
        [[ "${SCENARIO}" != restart-error ]] || exit 1
        ;;
    'rollout status statefulset/nico-rest-site-agent -n nico-rest --timeout=120s')
        [[ "${SCENARIO}" != rollout-error ]] || exit 1
        touch "${CASE_DIR}/rolled-out"
        ;;
    *) echo "unexpected kubectl command: $*" >&2; exit 1 ;;
esac
FAKE_KUBECTL
cat > "${TEST_TMP_DIR}/bin/sleep" <<'FAKE_SLEEP'
#!/usr/bin/env bash
[[ "$*" == 5 ]]
FAKE_SLEEP
chmod +x "${TEST_TMP_DIR}/bin/kubectl" "${TEST_TMP_DIR}/bin/sleep"
export PATH="${TEST_TMP_DIR}/bin:${PATH}"

fail() {
    echo "${SCENARIO}: $*" >&2
    cat "${CASE_DIR}/output" "${CASE_DIR}/calls" >&2
    exit 1
}

while read -r SCENARIO expected_status expected_polls expected_restarts expected_rollouts; do
    CASE_DIR="${TEST_TMP_DIR}/${SCENARIO}"
    export SCENARIO CASE_DIR
    mkdir "${CASE_DIR}"
    echo 0 > "${CASE_DIR}/polls"
    : > "${CASE_DIR}/calls"
    status=0
    bash "${TEST_TMP_DIR}/check.sh" > "${CASE_DIR}/output" 2>&1 || status=$?
    [[ "${status}" -eq "${expected_status}" ]] || fail "unexpected exit status ${status}"
    [[ "$(cat "${CASE_DIR}/polls")" -eq "${expected_polls}" ]] || fail "unexpected poll count"
    restarts="$(grep -c '^rollout restart ' "${CASE_DIR}/calls" || true)"
    rollouts="$(grep -c '^rollout status ' "${CASE_DIR}/calls" || true)"
    [[ "${restarts}" -eq "${expected_restarts}" ]] || fail "unexpected restart count"
    [[ "${rollouts}" -eq "${expected_rollouts}" ]] || fail "unexpected rollout count"
    if [[ "${expected_status}" -eq 0 ]]; then
        if [[ "${expected_polls}" -eq 48 ]]; then
            grep -Fq "WARNING: Could not confirm the site-agent's NICo Core gRPC connection after restart. Setup will continue; verify connectivity manually." \
                "${CASE_DIR}/output" || fail "missing final connection warning"
            if grep -Fq 'successfully connected' "${CASE_DIR}/output"; then
                fail "unconfirmed connection was reported as successful"
            fi
        else
            grep -Fq 'Site-agent successfully connected to NICo Core gRPC' "${CASE_DIR}/output" \
                || fail "connection success was not reported"
        fi
        grep -Fq 'check completed' "${CASE_DIR}/output" || fail "setup did not continue"
    else
        if grep -Eq 'successfully connected|check completed' "${CASE_DIR}/output"; then
            fail "setup continued or reported success after failure"
        fi
    fi
    echo "PASS: ${SCENARIO}"
done <<'CASES'
healthy       0  1 0 0
recovered     0 27 1 1
disconnected  0 48 1 1
log-error     0 48 1 1
restart-error 1 24 1 0
rollout-error 1 24 1 1
CASES

echo "setup site-agent connection tests passed"
