#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# health-check.sh with and without Vault, across the states a site can be in:
# Vault installed and required by the nico-api Deployment (with its ConfigMap
# missing), Vault absent (an install made with --skip-vault), a Vault namespace
# without the StatefulSet (partial install), Vault still running after
# --skip-vault moved NICo off it, NICo configured for a Vault that is gone
# before Core is installed (vault-cluster-info present, no Deployment), and the
# probe failures that leave the integration or the install unknown. The NICo
# namespace is detected from the database ConfigMap when vault-cluster-info is
# absent, with the default as the last fallback.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_LOG="${TEST_TMP_DIR}/kubectl.log"

mkdir -p "${TEST_TMP_DIR}/bin"
# TEST_VAULT is installed, partial (namespace only), or absent; TEST_NICO_API is
# the set of Deployments in the NICo namespace: vault (a renamed Core whose env
# carries VAULT_ADDR), novault, or absent; TEST_CERTS is the cert-manager
# Certificates: vault (Flow's still issued by vault-nico-issuer), none, nocrd
# (cert-manager not installed), or error; TEST_VAULT_CONFIGMAP says whether
# vault-cluster-info exists. Every other resource is missing, so the unrelated
# checks fail and the summary is reached.
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_LOG}"
case "$*" in
    cluster-info) ;;
    'get configmap -A --field-selector metadata.name=nico-system-nico-database-config -o jsonpath={.items[0].metadata.namespace}')
        printf '%s' "${TEST_DB_CONFIGMAP_NS}" ;;
    'get configmap -A --field-selector metadata.name=vault-cluster-info -o jsonpath={.items[0].metadata.namespace}')
        exit 1 ;;
    'get configmap -n '*' vault-cluster-info -o name')
        case "${TEST_VAULT_CONFIGMAP}" in
            present) printf 'configmap/vault-cluster-info\n' ;;
            error) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; exit 1 ;;
            *) echo 'Error from server (NotFound): configmaps "vault-cluster-info" not found' >&2; exit 1 ;;
        esac ;;
    'get certificates.cert-manager.io -A -o jsonpath='*)
        case "${TEST_CERTS}" in
            vault) printf 'flow/flow-certificate vault-nico-issuer\nnico-rest/core-grpc-client-site-agent-certs site-issuer\n' ;;
            none) printf 'nico-system/nico-api-certificate site-issuer\n' ;;
            nocrd) echo 'error: the server doesn'"'"'t have a resource type "certificates"' >&2; exit 1 ;;
            *) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; exit 1 ;;
        esac ;;
    'get deployments -n '*' -o jsonpath={range .items[*]}'*)
        case "${TEST_NICO_API}" in
            vault) printf 'carbide-api DATASTORE_USER,VAULT_ADDR,VAULT_PKI_ROLE_NAME,\nnico-bmc-proxy RUST_LOG,\n' ;;
            novault) printf 'nico-api DATASTORE_USER,\n' ;;
            absent) ;;
            *) exit 1 ;;
        esac ;;
    'get statefulset -n vault vault -o name')
        case "${TEST_VAULT}" in
            installed) printf 'statefulset.apps/vault\n' ;;
            error) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; exit 1 ;;
            *) echo 'Error from server (NotFound): statefulsets.apps "vault" not found' >&2; exit 1 ;;
        esac ;;
    'get statefulset -n vault vault -o jsonpath={.spec.replicas}'|'get statefulset -n vault vault -o jsonpath={.status.readyReplicas}')
        printf '1' ;;
    'get namespace vault')
        [[ "${TEST_VAULT}" == partial ]] || exit 1 ;;
    *'jsonpath={.data.DB_HOST}') printf 'nico-pg-cluster.postgres.svc.cluster.local' ;;
    *'jsonpath={.data.DB_NAME}') printf 'nico_system_nico' ;;
    *) exit 1 ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"

run_health_check() { # $1 TEST_VAULT, $2 TEST_NICO_API, $3 TEST_CERTS, $4 TEST_VAULT_CONFIGMAP, $5 NICO_NS (empty = auto-detect), $6 database ConfigMap namespace
    : > "${TEST_LOG}"
    status=0
    (
        export TEST_VAULT="$1" TEST_NICO_API="$2" TEST_CERTS="$3" TEST_VAULT_CONFIGMAP="$4" TEST_DB_CONFIGMAP_NS="$6"
        if [[ -n "$5" ]]; then export NICO_NS="$5"; else unset NICO_NS; fi
        export VAULT_NS=vault POSTGRES_NS=postgres CERT_MANAGER_NS=cert-manager ESO_NS=external-secrets \
            METALLB_NS=metallb-system CONTOUR_NS='' DPF_NS='' FLOW_NS=flow RMS_NS=rack-manager
        PATH="${TEST_TMP_DIR}/bin:${PATH}" bash "${SCRIPT_DIR}/../health-check.sh"
    ) > "${TEST_TMP_DIR}/output" 2>&1 || status=$?
    if [[ "${status}" -ne 1 ]] || ! grep -Fq 'CHECK(S) FAILED' "${TEST_TMP_DIR}/output"; then
        printf '%s: health check did not reach its failure summary (exit %s)\n' "${CASE}" "${status}" >&2
        cat "${TEST_TMP_DIR}/output" >&2
        exit 1
    fi
}
assert_output() {
    grep -Fq -- "$1" "${TEST_TMP_DIR}/output" || {
        printf '%s: missing output: %s\n' "${CASE}" "$1" >&2; cat "${TEST_TMP_DIR}/output" >&2; exit 1; }
}
assert_no_output() {
    ! grep -Fq -- "$1" "${TEST_TMP_DIR}/output" || {
        printf '%s: unexpected output: %s\n' "${CASE}" "$1" >&2; cat "${TEST_TMP_DIR}/output" >&2; exit 1; }
}
assert_called() {
    grep -Fq -- "$1" "${TEST_LOG}" || {
        printf '%s: missing kubectl call: %s\n' "${CASE}" "$1" >&2; cat "${TEST_LOG}" >&2; exit 1; }
}
assert_not_called() {
    ! grep -Fq -- "$1" "${TEST_LOG}" || {
        printf '%s: unexpected kubectl call: %s\n' "${CASE}" "$1" >&2; cat "${TEST_LOG}" >&2; exit 1; }
}

VAULT_ABSENT_SKIP='SKIP  not installed (--skip-vault was used)'
VAULT_UNUSED_WARN='WARN  vault: running, but NICo is not configured to use it (--skip-vault); uninstall the vault release and delete the namespace when the rollback window closes'
INTEGRATION_SKIPS=(
    'SKIP  job/vault-pki-config: not configured (--skip-vault was used)'
    'SKIP  clusterissuer/vault-nico-issuer: not configured (--skip-vault was used)'
    'SKIP  secret/nico-vault-token, secret/nico-vault-approle-tokens: not configured (--skip-vault was used)'
    'SKIP  configmap/vault-cluster-info: not configured (--skip-vault was used)'
)
INTEGRATION_CHECKS=(
    'job/vault-pki-config:'
    'clusterissuer/vault-nico-issuer:'
    'secret/nico-vault-token:'
    'secret/nico-vault-approle-tokens'
    'configmap/vault-cluster-info['
)

CASE="Vault absent after --skip-vault, Core deployed without Vault, namespace detected from the database ConfigMap"
run_health_check absent novault none absent "" nico-site
grep -Eq '^  NICo namespace: +nico-site$' "${TEST_TMP_DIR}/output" \
    || { printf '%s: NICo namespace was not detected from nico-system-nico-database-config\n' "${CASE}" >&2; cat "${TEST_TMP_DIR}/output" >&2; exit 1; }
assert_output 'vault address:             not installed (no statefulset/vault in vault)'
assert_output "${VAULT_ABSENT_SKIP}"
for line in "${INTEGRATION_SKIPS[@]}"; do assert_output "${line}"; done
for check in "${INTEGRATION_CHECKS[@]}"; do assert_no_output "FAIL  ${check}"; done
assert_no_output 'statefulset/vault:'
assert_no_output 'vault-0:'
assert_no_output "${VAULT_UNUSED_WARN}"
# The non-Vault PKI, Secrets, and ConfigMaps are still checked, in the detected namespace.
assert_output 'clusterissuer/selfsigned-bootstrap:'
assert_output 'clusterissuer/site-issuer:'
assert_output 'secret/nico-roots:'
assert_output 'secret/nico-api-certificate:'
assert_output 'configmap/nico-system-nico-database-config[DB_HOST]'
assert_called 'get statefulset -n vault vault -o name'
assert_called 'get namespace vault'
# Core is deployed, so its Deployment decides the integration; the ConfigMap is not consulted.
assert_called 'get deployments -n nico-site -o jsonpath={range .items[*]}'
assert_not_called 'get configmap -n nico-site vault-cluster-info -o name'
assert_called 'get deployment -n nico-site nico-api -o jsonpath={.spec.replicas}'
assert_not_called 'exec -n vault'

CASE="Vault namespace without a StatefulSet, Core not installed, no ConfigMap"
run_health_check partial absent none absent nico-system ""
assert_output 'FAIL  statefulset/vault: not found, but namespace vault exists (partial/failed Vault install)'
assert_no_output "${VAULT_ABSENT_SKIP}"
assert_no_output "${VAULT_UNUSED_WARN}"
for line in "${INTEGRATION_SKIPS[@]}"; do assert_output "${line}"; done
assert_not_called 'exec -n vault'

CASE="Vault installed and required by a renamed Core Deployment, vault-cluster-info missing"
run_health_check installed vault none absent nico-system ""
assert_output 'vault address:             https://vault.vault.svc.cluster.local:8200'
assert_output 'PASS  statefulset/vault: 1/1 ready'
assert_output 'FAIL  vault-0: not initialized'
assert_output 'FAIL  job/vault-pki-config: not Complete'
assert_output 'FAIL  clusterissuer/vault-nico-issuer: not Ready'
assert_output 'FAIL  secret/nico-vault-token: not found in nico-system'
assert_output 'FAIL  secret/nico-vault-approle-tokens[VAULT_ROLE_ID]: missing or empty'
assert_output 'FAIL  configmap/vault-cluster-info[VAULT_SERVICE]: missing or empty'
assert_output 'FAIL  configmap/vault-cluster-info[NICO_VAULT_PKI_MOUNT]: missing or empty'
assert_no_output '(--skip-vault was used)'
assert_no_output "${VAULT_UNUSED_WARN}"
assert_not_called 'get namespace vault'
assert_called 'exec -n vault vault-0 -c vault -- vault status'
# The Deployments decided; the ConfigMap is not consulted for integration.
assert_called 'get deployments -n nico-system -o jsonpath={range .items[*]}'
assert_not_called 'get configmap -n nico-system vault-cluster-info -o name'

CASE="Vault running after --skip-vault moved NICo off it"
run_health_check installed novault none absent nico-system ""
assert_output 'PASS  statefulset/vault: 1/1 ready'
assert_output 'FAIL  vault-0: not initialized'
assert_output "${VAULT_UNUSED_WARN}"
for line in "${INTEGRATION_SKIPS[@]}"; do assert_output "${line}"; done
for check in "${INTEGRATION_CHECKS[@]}"; do assert_no_output "FAIL  ${check}"; done
assert_no_output "${VAULT_ABSENT_SKIP}"
assert_called 'exec -n vault vault-0 -c vault -- vault status'

CASE="NICo configured for a Vault that is gone, before Core is installed"
run_health_check absent absent none present nico-system ""
assert_output 'FAIL  statefulset/vault: not found, but NICo is configured to use Vault (vault-cluster-info exists in nico-system)'
assert_no_output "${VAULT_ABSENT_SKIP}"
assert_no_output 'partial/failed Vault install'
assert_no_output "${VAULT_UNUSED_WARN}"
# NICo still expects the Vault integration, so those checks run and fail.
assert_output 'FAIL  job/vault-pki-config: not Complete'
assert_output 'FAIL  clusterissuer/vault-nico-issuer: not Ready'
assert_output 'FAIL  secret/nico-vault-token: not found in nico-system'
for line in "${INTEGRATION_SKIPS[@]}"; do assert_no_output "${line}"; done
assert_not_called 'get namespace vault'
assert_not_called 'exec -n vault'
assert_called 'get configmap -n nico-system vault-cluster-info -o name'

CASE="Core requires Vault but the StatefulSet is gone"
run_health_check absent vault none absent nico-system ""
assert_output 'FAIL  statefulset/vault: not found, but NICo is configured to use Vault (deployment/carbide-api references VAULT_ADDR)'
assert_no_output "${VAULT_ABSENT_SKIP}"
assert_output 'FAIL  clusterissuer/vault-nico-issuer: not Ready'

# After `setup.sh --skip-vault --skip-rest` on a Vault site, Core is off Vault but
# Flow's SPIFFE certificate still renews through vault-nico-issuer: setup retained
# the Vault resources, and health-check must not call Vault unused.
CASE="Vault running, Core off Vault, a Flow Certificate still issued by vault-nico-issuer"
run_health_check installed novault vault absent nico-system ""
assert_output 'PASS  statefulset/vault: 1/1 ready'
assert_no_output "${VAULT_UNUSED_WARN}"
assert_output 'FAIL  clusterissuer/vault-nico-issuer: not Ready'
assert_output 'FAIL  job/vault-pki-config: not Complete'
for line in "${INTEGRATION_SKIPS[@]}"; do assert_no_output "${line}"; done
assert_called 'get certificates.cert-manager.io -A -o jsonpath='

CASE="Vault gone while a Flow Certificate is still issued by vault-nico-issuer"
run_health_check absent novault vault absent nico-system ""
assert_output 'FAIL  statefulset/vault: not found, but NICo is configured to use Vault (certificate/flow/flow-certificate is issued by vault-nico-issuer)'
assert_no_output "${VAULT_ABSENT_SKIP}"

CASE="Vault absent before cert-manager has CRDs"
run_health_check absent absent nocrd absent nico-system ""
assert_output "${VAULT_ABSENT_SKIP}"
assert_no_output 'could not list cert-manager Certificates'

CASE="Vault absent and the Certificate query fails"
run_health_check absent absent error absent nico-system ""
assert_output 'WARN  could not list cert-manager Certificates; Vault integration is decided without them'
assert_output "${VAULT_ABSENT_SKIP}"

CASE="Vault absent and no database ConfigMap: the default namespace"
run_health_check absent absent none absent "" ""
grep -Eq '^  NICo namespace: +nico-system$' "${TEST_TMP_DIR}/output" \
    || { printf '%s: NICo namespace did not fall back to nico-system\n' "${CASE}" >&2; cat "${TEST_TMP_DIR}/output" >&2; exit 1; }
assert_called 'get configmap -A --field-selector metadata.name=vault-cluster-info -o jsonpath={.items[0].metadata.namespace}'

echo "health-check optional Vault tests passed"

# A failed probe leaves the integration unknown: the integration checks say so
# instead of "not configured", and a running Vault is not offered for removal.
CASE="Vault running, Core without Vault, Certificate query fails: integration unknown"
run_health_check installed novault error absent nico-system nico-system
assert_output 'WARN  could not list cert-manager Certificates; Vault integration is decided without them'
assert_output 'WARN  vault: running; whether NICo still uses it could not be determined (a Deployment or Certificate query failed above), so keep it until a complete check says otherwise'
assert_no_output "${VAULT_UNUSED_WARN}"
assert_output 'SKIP  job/vault-pki-config: Vault integration could not be determined (a Deployment or Certificate query failed above)'
assert_no_output '(--skip-vault was used)'

# An API error on the StatefulSet query is reported, never read as "Vault was skipped".
CASE="statefulset/vault cannot be queried"
run_health_check error novault none absent nico-system nico-system
assert_output 'FAIL  statefulset/vault: could not be queried (Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout)'
assert_no_output "${VAULT_ABSENT_SKIP}"
assert_no_output "${VAULT_UNUSED_WARN}"
assert_not_called 'exec -n vault'

# The ConfigMap fallback is the last probe: an API error there is unknown too.
CASE="Vault running, no Deployment, vault-cluster-info query fails: integration unknown"
run_health_check installed absent none error nico-system nico-system
assert_output 'WARN  could not query configmap/vault-cluster-info in nico-system; Vault integration is decided without it'
assert_output 'WARN  vault: running; whether NICo still uses it could not be determined'
assert_no_output "${VAULT_UNUSED_WARN}"
assert_no_output '(--skip-vault was used)'
