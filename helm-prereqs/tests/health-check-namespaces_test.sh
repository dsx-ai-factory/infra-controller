#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_LOG="${TEST_TMP_DIR}/kubectl.log"

mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_LOG}"
case "$*" in
    cluster-info) ;;
    *'jsonpath={.data.VAULT_SERVICE}') printf '%s' "${TEST_VAULT_ADDRESS}" ;;
    *'jsonpath={.data.DB_HOST}') printf '%s' "${TEST_DB_HOST}" ;;
    *'jsonpath={.data.DB_NAME}') printf 'test_database' ;;
    'exec -n '*' nico-pg-cluster-0 -c postgres -- psql -U postgres -lqt')
        printf 'test_database\n'
        ;;
    *) exit 1 ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"

while IFS='|' read -r name vault_address db_host vault_override postgres_override expected_vault expected_postgres; do
    : > "${TEST_LOG}"
    status=0
    TEST_VAULT_ADDRESS="${vault_address}" TEST_DB_HOST="${db_host}" \
        NICO_NS=nico-system VAULT_NS="${vault_override}" POSTGRES_NS="${postgres_override}" \
        CERT_MANAGER_NS=cert-manager ESO_NS=external-secrets METALLB_NS=metallb-system \
        CONTOUR_NS='' DPF_NS='' FLOW_NS=flow RMS_NS=rack-manager \
        PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        bash "${SCRIPT_DIR}/../health-check.sh" > "${TEST_TMP_DIR}/output" 2>&1 || status=$?

    # Missing fixture resources should fail checks, not stop namespace detection.
    if [[ "${status}" -ne 1 ]] || ! grep -Fq 'CHECK(S) FAILED' "${TEST_TMP_DIR}/output"; then
        printf '%s: health check did not reach its failure summary\n' "${name}" >&2
        cat "${TEST_TMP_DIR}/output" >&2
        exit 1
    fi
    for expected in "vault namespace: +${expected_vault}" "postgres namespace: +${expected_postgres}"; do
        if ! grep -Eq "^  ${expected}$" "${TEST_TMP_DIR}/output"; then
            printf '%s: missing namespace output: %s\n' "${name}" "${expected}" >&2
            cat "${TEST_TMP_DIR}/output" >&2
            exit 1
        fi
    done
    for expected in "get statefulset -n ${expected_vault} vault -o" \
                    "get statefulset -n ${expected_postgres} nico-pg-cluster -o"; do
        if ! grep -Fq -- "${expected}" "${TEST_LOG}"; then
            printf '%s: missing kubectl call: %s\n' "${name}" "${expected}" >&2
            cat "${TEST_LOG}" >&2
            exit 1
        fi
    done
done <<'CASES'
service DNS|https://vault.vault-system.svc.cluster.local:8200|nico-pg-cluster.database-system.svc.cluster.local|||vault-system|database-system
two-label service DNS|https://vault.vault-system:8200|nico-pg-cluster.database-system|||vault-system|database-system
short service DNS|http://vault.vault-short.svc:8200/|nico-pg-cluster.database-short.svc|||vault-short|database-short
custom cluster domain|https://vault.vault-site.svc.site.internal.:8200|nico-pg-cluster.database-site.svc.site.internal.|||vault-site|database-site
IPv6|https://[2001:db8::10]:8200|2001:db8::20|||vault|postgres
IPv4|https://192.0.2.10:8200|192.0.2.20|||vault|postgres
external DNS|https://vault.example.com:8200|db.example.com|||vault|postgres
missing configuration|||||vault|postgres
explicit overrides|https://vault.vault-system.svc.cluster.local:8200|nico-pg-cluster.database-system.svc.cluster.local|my-vault|my-postgres|my-vault|my-postgres
CASES

echo "health-check namespace tests passed"
