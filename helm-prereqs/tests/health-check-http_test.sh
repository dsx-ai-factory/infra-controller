#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Exercise the real helpers and hardware-health call without a cluster.
extract_section() {
    awk -v start="$1" -v end="$2" '
        $0 == start { starts++; copying = 1 }
        copying { print }
        copying && $0 == end { ends++; copying = 0 }
        END { if (starts != 1 || ends != 1 || copying) exit 1 }
    ' "${SCRIPT_DIR}/../health-check.sh"
}
OUTPUT_HELPERS=$(extract_section 'set -uo pipefail' '# Preflight: kubectl accessible')
HTTP_CHECK=$(extract_section '_http_check() {' '}')
HARDWARE_CHECK=$(extract_section 'if kc get deployment -n "${NICO_NS}" nico-hardware-health &>/dev/null; then' 'fi')

kubectl() {
    case "$*" in
        'get deployment -n nico-system nico-hardware-health') return 0 ;;
        'get pod -n nico-system -l app.kubernetes.io/name=nico-hardware-health --field-selector=status.phase=Running -o jsonpath={.items[0].metadata.name}')
            printf '%s' "${TEST_POD}"
            ;;
        'exec -n nico-system hardware-health-0 -- sh -c command -v curl >/dev/null 2>&1')
            return "${TEST_COMMAND_STATUS}"
            ;;
        'exec -n nico-system hardware-health-0 -- curl -s -o /dev/null -w %{http_code} --max-time 5 http://localhost:9009/metrics')
            printf '%s' "${TEST_CODE}"
            printf 'diagnostic from exec or curl\n' >&2
            return "${TEST_EXEC_STATUS}"
            ;;
        *) printf 'unexpected kubectl call: %s\n' "$*" >&2; return 99 ;;
    esac
}
export -f kubectl
export NICO_NS=nico-system
export TEST_POD=hardware-health-0 TEST_COMMAND_STATUS=0

while IFS='|' read -r name code exec_status expected message; do
    export TEST_CODE="${code}" TEST_EXEC_STATUS="${exec_status}"
    output=$(bash -c "${OUTPUT_HELPERS}
${HTTP_CHECK}
_http_check 'probe' nico-system app.kubernetes.io/name=nico-hardware-health http://localhost:9009/metrics '${expected}'
printf 'counts=%s,%s,%s' \"\${PASS}\" \"\${FAIL}\" \"\${SKIP}\"")
    if [[ "${output}" != *"${message}"* || "${output}" == *diagnostic* ]]; then
        printf '%s: unexpected result\n%s\n' "${name}" "${output}" >&2
        exit 1
    fi
    if [[ "${message}" == *'PASS'* ]]; then
        [[ "${output}" == *'counts=1,0,0' ]]
    else
        [[ "${output}" == *'counts=0,1,0' ]]
    fi
done <<'CASES'
OK|200|0|200|PASS  probe: HTTP 200
not found|404|0|200|FAIL  probe: HTTP 404 (expected 200)
server error|500|0|200|FAIL  probe: HTTP 500 (expected 200)
connection failure|000|7|200|FAIL  probe: request failed (exec exit code 7, HTTP 000)
timeout after headers|200|28|200|FAIL  probe: request failed (exec exit code 28, HTTP 200)
exec failure||1|200|FAIL  probe: request failed (exec exit code 1, HTTP unknown)
2xx accepted|204|0|2xx|PASS  probe: HTTP 204
malformed 2xx|200000|0|2xx|FAIL  probe: invalid HTTP status '200000' (expected 2xx)
non-2xx|404|0|2xx|FAIL  probe: HTTP 404 (expected 2xx)
CASES

export TEST_CODE=200 TEST_EXEC_STATUS=0
while IFS='|' read -r name pod command_status message; do
    export TEST_POD="${pod}" TEST_COMMAND_STATUS="${command_status}"
    output=$(bash -c "${OUTPUT_HELPERS}
${HTTP_CHECK}
${HARDWARE_CHECK}")
    if [[ "${output}" != *"${message}"* || "${output}" == *diagnostic* ]]; then
        printf '%s: unexpected result\n%s\n' "${name}" "${output}" >&2
        exit 1
    fi
done <<'CASES'
hardware metrics wiring|hardware-health-0|0|PASS  nico-hardware-health metrics: HTTP 200
no pod||0|FAIL  nico-hardware-health metrics: no running pod found
missing curl|hardware-health-0|1|SKIP  nico-hardware-health metrics: curl not available in pod/hardware-health-0
CASES

echo 'health-check HTTP tests passed'
