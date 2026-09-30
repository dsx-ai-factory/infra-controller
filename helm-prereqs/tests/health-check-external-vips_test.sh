#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_LOG="${TEST_TMP_DIR}/probes.log"

# Run the real VIP sections and summary without unrelated cluster checks.
# Exact markers keep a moved or renamed section from silently dropping coverage.
extract_section() {
    awk -v start="$1" -v end="$2" '
        $0 == start { starts++; copying = 1 }
        copying { print }
        $0 == end { ends++; copying = 0 }
        END { if (starts != 1 || ends != 1 || copying) exit 1 }
    ' "${SCRIPT_DIR}/../health-check.sh"
}

OUTPUT_HELPERS=$(extract_section 'set -uo pipefail' '# Preflight: kubectl accessible')
VIP_CHECKS=$(extract_section '# 8. External Services — LoadBalancer VIPs' '# 10. In-cluster service connectivity')
SUMMARY=$(extract_section '# Summary' '[[ "${FAIL}" -eq 0 ]]')

kubectl() {
    case "$*" in
        'get svc -n nico-system --no-headers')
            printf '%s LoadBalancer\n' "${TEST_SERVICE}"
            ;;
        "get svc -n nico-system ${TEST_SERVICE} -o jsonpath={range .status.loadBalancer.ingress[*]}{.ip}{\"\n\"}{end}")
            printf '%s\n' "${TEST_IPS}"
            ;;
        "get svc -n nico-system ${TEST_SERVICE} -o jsonpath={.spec.ports[0].port}")
            printf '443'
            ;;
        "get svc -n nico-system ${TEST_SERVICE} -o jsonpath={.spec.ports[0].protocol}")
            printf '%s' "${TEST_PROTOCOL}"
            ;;
        'get pod -n metallb-system -l app.kubernetes.io/component=speaker,app.kubernetes.io/name=metallb --no-headers -o custom-columns=NAME:.metadata.name')
            printf 'speaker-0\n'
            ;;
        'exec -n metallb-system speaker-0 -c speaker -- sh -c command -v nc >/dev/null 2>&1') ;;
        'exec -n metallb-system speaker-0 -c speaker -- nc -zw2 '*)
            printf '%s\n' "$*" >> "${TEST_LOG}"
            [[ "${10}" != "${TEST_FAILED_IP}" ]]
            ;;
        *)
            printf 'unexpected kubectl call: %s\n' "$*" >&2
            return 1
            ;;
    esac
}
export -f kubectl

assert_output() {
    if ! grep -Fxq -- "$1" <<< "${output}"; then
        printf '%s: missing output: %s\n%s\n' "${name}" "$1" "${output}" >&2
        exit 1
    fi
}

export NICO_NS=nico-system METALLB_NS=metallb-system
export TEST_SERVICE TEST_IPS TEST_PROTOCOL TEST_FAILED_IP
while IFS='|' read -r name TEST_SERVICE ips TEST_PROTOCOL TEST_FAILED_IP passed failed probe_ips; do
    TEST_IPS="${ips//,/$'\n'}"
    : > "${TEST_LOG}"
    status=0
    output=$(bash -c "${OUTPUT_HELPERS}
${VIP_CHECKS}
${SUMMARY}" 2>&1) || status=$?

    expected_status=$((failed > 0))
    if [[ "${status}" -ne "${expected_status}" ]]; then
        printf '%s: expected exit %s, got %s\n%s\n' "${name}" "${expected_status}" "${status}" "${output}" >&2
        exit 1
    fi
    if [[ "${failed}" -eq 0 ]]; then
        assert_output '  ALL CHECKS PASSED'
    else
        assert_output "  ${failed} CHECK(S) FAILED"
    fi
    if ! grep -Eq "✓ ${passed} +passed  ✗ ${failed} +failed" <<< "${output}"; then
        printf '%s: incorrect summary counts\n%s\n' "${name}" "${output}" >&2
        exit 1
    fi

    if [[ -z "${ips}" ]]; then
        assert_output "  ✗ FAIL  svc/${TEST_SERVICE}: no external IP (still pending)"
    else
        while IFS= read -r ip; do
            address="${ip}"
            [[ "${ip}" == *:* ]] && address="[${ip}]"
            assert_output "  ✓ PASS  svc/${TEST_SERVICE}: ${address}:443"
        done <<< "${TEST_IPS}"
    fi

    expected_probes=''
    if [[ -n "${probe_ips}" ]]; then
        while IFS= read -r ip; do
            expected_probes+="exec -n metallb-system speaker-0 -c speaker -- nc -zw2 ${ip} 443"$'\n'
            address="${ip}"
            [[ "${ip}" == *:* ]] && address="[${ip}]"
            if [[ "${ip}" == "${TEST_FAILED_IP}" ]]; then
                assert_output "  ✗ FAIL  svc/${TEST_SERVICE}: ${address}:443 not reachable (BGP route missing or service not listening)"
            else
                assert_output "  ✓ PASS  svc/${TEST_SERVICE}: ${address}:443 reachable from host network"
            fi
        done <<< "${probe_ips//,/$'\n'}"
    fi
    actual_probes=$(< "${TEST_LOG}")
    if [[ "${actual_probes}" != "${expected_probes%$'\n'}" ]]; then
        printf '%s: unexpected probes\nexpected:\n%sactual:\n%s\n' \
            "${name}" "${expected_probes}" "${actual_probes}" >&2
        exit 1
    fi
done <<'CASES'
healthy dual-stack|nico-api-external|192.0.2.10,2001:db8::10|TCP||4|0|192.0.2.10,2001:db8::10
second VIP unreachable|nico-api-external|192.0.2.10,2001:db8::10|TCP|2001:db8::10|3|1|192.0.2.10,2001:db8::10
unassigned service|nico-api-external||TCP||0|1|
UDP protocol|nico-ntp-external|192.0.2.10,2001:db8::10|UDP||2|0|
UDP duplicate name|nico-unbound-external-udp|192.0.2.10,2001:db8::10|TCP||2|0|
CASES

echo "health-check external VIP tests passed"
