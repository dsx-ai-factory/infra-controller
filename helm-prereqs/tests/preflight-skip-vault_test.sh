#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# preflight.sh --skip-vault: the standalone flags and variables resolve and
# validate INSTALL_VAULT; _check_vault_free_core_values invokes the checker
# on the values file and maps its findings onto the preflight lists under
# the values-file label, turning a missing python3 or an unusable file into
# one error; the running-Vault warning and the three-node requirement follow
# INSTALL_VAULT; legacy PSM/NSM hook resources make --skip-vault an error
# before any mutation. Both --skip-vault errors block setup even with -y.
# Vault consumers that this run does not redeploy (Core with --skip-core, Flow
# and the site-agent with --skip-rest, RMS with --skip-rms, Certificates outside
# those releases) are listed in a warning that the Vault resources are kept.
# No Python is needed: a fake python3 stands in for the checker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

fail() {
    printf '%s\n' "$@" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# 1. Standalone resolution and validation.
# ---------------------------------------------------------------------------
standalone="$(sed -n '/^if ! ${_SOURCED}; then$/,/^fi$/p' "${PREFLIGHT_SH}")"
[[ -n "${standalone}" ]] || fail "could not extract the standalone flag block from preflight.sh"
resolve() { # $1 environment assignments (may be empty); remaining args: flags
    local assignments="$1"
    shift
    (
        _SOURCED=false
        eval "${assignments}"
        eval "${standalone}"
        printf '%s' "${INSTALL_VAULT}"
    ) 2>&1
}
while IFS='|' read -r name assignments flags expected; do
    # shellcheck disable=SC2086 # The flags column holds zero or more words.
    actual="$(resolve "${assignments}" ${flags})" || true
    [[ "${actual}" == "${expected}" ]] || fail "${name}: got '${actual}', want '${expected}'"
done <<'CASES'
default|||true
--skip-vault||--skip-vault|false
NICO_SKIP_VAULT=true|NICO_SKIP_VAULT=true||false
NICO_INSTALL_VAULT=false|NICO_INSTALL_VAULT=false||false
exported INSTALL_VAULT from setup.sh wins over the defaults|INSTALL_VAULT=false||false
--install-vault overrides the environment|NICO_SKIP_VAULT=true|--install-vault|true
CASES
# Standalone runs may inherit INSTALL_VAULT itself, so the message names both spellings.
while IFS='|' read -r assignment expected; do
    if output="$(resolve "${assignment}")"; then
        fail "standalone preflight accepted ${assignment}"
    fi
    grep -Fq "${expected}" <<< "${output}" \
        || fail "standalone preflight did not reject ${assignment} by name:" "${output}"
done <<'CASES'
NICO_INSTALL_VAULT=maybe|INSTALL_VAULT (or NICO_INSTALL_VAULT) must be true or false (got 'maybe')
INSTALL_VAULT=maybe|INSTALL_VAULT (or NICO_INSTALL_VAULT) must be true or false (got 'maybe')
NICO_SKIP_VAULT=yes|NICO_SKIP_VAULT must be true or false (got 'yes')
CASES

# ---------------------------------------------------------------------------
# 2. _check_vault_free_core_values runs the checker on the values file and
#    maps its output. A fake python3 stands in for the checker, whose rules
#    test_vault_free.py covers, so this test needs no PyYAML.
# ---------------------------------------------------------------------------
eval "$(sed -n '/^_check_vault_free_core_values()/,/^}/p' "${PREFLIGHT_SH}")"
declare -f _check_vault_free_core_values >/dev/null || fail "could not extract _check_vault_free_core_values"

mkdir -p "${TEST_TMP_DIR}/bin" "${TEST_TMP_DIR}/no-python"
export FAKE_PYTHON_LOG="${TEST_TMP_DIR}/python3.log"
cat > "${TEST_TMP_DIR}/bin/python3" <<'FAKE_PYTHON'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${FAKE_PYTHON_LOG}"
case "${FAKE_CHECKER}" in
    findings)
        printf 'ERROR: the site TOML has no [secrets] section\n'
        printf 'ERROR: nico-api.vaultClusterInfo is set\n'
        printf 'WARNING: nico-api.vault.enabled is true; setup.sh --skip-vault overrides it\n'
        printf 'a line without a prefix is ignored\n'
        ;;
    clean) ;;
    unusable)
        printf 'Cannot check the Vault-free Core values: nico-api must be a mapping\n' >&2
        exit 1
        ;;
esac
FAKE_PYTHON
chmod +x "${TEST_TMP_DIR}/bin/python3"

run_check() { # $1 fake checker mode, $2 PATH; prints ERRORS then WARNINGS, one per line
    : > "${FAKE_PYTHON_LOG}"
    (
        export FAKE_CHECKER="$1"
        PATH="$2"
        SCRIPT_DIR="${SCRIPT_DIR}/.."   # the function runs ${SCRIPT_DIR}/check-vault-free.py
        _CORE_VALUES_CFG="/site/core-values.yaml"
        _CORE_VALUES_LABEL="site/core-values.yaml"
        ERRORS=()
        WARNINGS=()
        BLOCKING_ERRORS=()
        _check_vault_free_core_values
        printf 'B: %s\n' "${BLOCKING_ERRORS[@]:-}"
        printf 'E: %s\n' "${ERRORS[@]:-}"
        printf 'W: %s\n' "${WARNINGS[@]:-}"
    )
}
fake_path="${TEST_TMP_DIR}/bin:${PATH}"

result="$(run_check findings "${fake_path}")"
expected=$'B: site/core-values.yaml: the site TOML has no [secrets] section\nB: site/core-values.yaml: nico-api.vaultClusterInfo is set\nE: \nW: site/core-values.yaml: nico-api.vault.enabled is true; setup.sh --skip-vault overrides it'
[[ "${result}" == "${expected}" ]] || fail "checker findings were not mapped onto BLOCKING_ERRORS and WARNINGS under the values label:" "${result}"
grep -Eq '^[^ ]+/check-vault-free\.py /site/core-values\.yaml$' "${FAKE_PYTHON_LOG}" \
    || fail "the checker was not invoked as check-vault-free.py <values file>:" "$(cat "${FAKE_PYTHON_LOG}")"
[[ "$(grep -c . "${FAKE_PYTHON_LOG}")" -eq 1 ]] || fail "the checker ran more than once:" "$(cat "${FAKE_PYTHON_LOG}")"
checker_path="$(cut -d' ' -f1 "${FAKE_PYTHON_LOG}")"
[[ -f "${checker_path}" ]] || fail "preflight points at a checker that does not exist: ${checker_path}"

result="$(run_check clean "${fake_path}")"
[[ "${result}" == $'B: \nE: \nW: ' ]] || fail "a clean checker run produced findings:" "${result}"

result="$(run_check unusable "${fake_path}")"
[[ "${result}" == $'B: site/core-values.yaml: Cannot check the Vault-free Core values: nico-api must be a mapping\nE: \nW: ' ]] \
    || fail "a failed checker run was not reported as one labeled blocking error:" "${result}"

result="$(run_check clean "${TEST_TMP_DIR}/no-python")"
[[ "${result}" == $'B: --skip-vault preflight requires python3 with PyYAML and a TOML parser (python3 3.11 or the tomli package) to check site/core-values.yaml — install them before running setup.sh\nE: \nW: ' ]] \
    || fail "missing python3 was not reported as the tool requirement error:" "${result}"
[[ ! -s "${FAKE_PYTHON_LOG}" ]] || fail "the checker ran without python3 on PATH"

# The findings are blocking: the real output section stops even with -y and says so generically.
sed -n '/^# Output and prompts/,$p' "${PREFLIGHT_SH}" > "${TEST_TMP_DIR}/output.sh"
blocking_rc=0
blocking_out="$(
    # shellcheck disable=SC2034 # Read by the sourced output section.
    _SOURCED=true AUTO_YES=true _NICO_REST_ENABLED=false
    ERRORS=()
    WARNINGS=()
    BLOCKING_ERRORS=('site/core-values.yaml: the site TOML has no [secrets] section')
    # shellcheck disable=SC1091 # Generated from preflight.sh above.
    source "${TEST_TMP_DIR}/output.sh"
)" || blocking_rc=$?
[[ "${blocking_rc}" -ne 0 ]] || fail "preflight continued past a --skip-vault blocking error with -y:" "${blocking_out}"
grep -Fq 'site/core-values.yaml: the site TOML has no [secrets] section' <<< "${blocking_out}" \
    || fail "the blocking error was not printed:" "${blocking_out}"
grep -Fq 'block setup even with -y' <<< "${blocking_out}" \
    || fail "the blocking header does not say the errors block -y:" "${blocking_out}"
! grep -Fq 'The database errors above' <<< "${blocking_out}" \
    || fail "the blocking header is still database-specific:" "${blocking_out}"

# The check runs only for a --skip-vault Core install against an existing values file.
gate_line="$(grep -nxF 'if [[ "${INSTALL_VAULT:-true}" == "false" && "${SKIP_CORE:-false}" != "true" && -f "${_CORE_VALUES_CFG}" ]]; then' "${PREFLIGHT_SH}" | cut -d: -f1)"
[[ -n "${gate_line}" ]] || fail "the Vault-free check is not gated on INSTALL_VAULT=false, a Core install, and the values file"
[[ "$(sed -n "$((gate_line + 1))p" "${PREFLIGHT_SH}")" == '    _check_vault_free_core_values' ]] \
    || fail "the gate does not call _check_vault_free_core_values"

# ---------------------------------------------------------------------------
# 3. A running Vault is a warning only under --skip-vault.
# ---------------------------------------------------------------------------
vault_warning="$(sed -n '/^    if \[\[ "${INSTALL_VAULT:-true}" == "false" \]\] && \\$/,/^    fi$/p' "${PREFLIGHT_SH}")"
[[ -n "${vault_warning}" ]] || fail "could not extract the running-Vault warning block"
KUBECTL_LOG="${TEST_TMP_DIR}/kubectl.log"
kubectl() {
    printf '%s\n' "$*" >> "${KUBECTL_LOG}"
    [[ "${FAKE_VAULT_PRESENT}" == "true" ]]
}
run_vault_warning() { # $1 INSTALL_VAULT, $2 FAKE_VAULT_PRESENT, $3 VAULT_NS (may be empty)
    : > "${KUBECTL_LOG}"
    (
        INSTALL_VAULT="$1"
        FAKE_VAULT_PRESENT="$2"
        WARNINGS=()
        if [[ -n "$3" ]]; then VAULT_NS="$3"; else unset VAULT_NS; fi
        eval "${vault_warning}"
        printf '%s\n' "${WARNINGS[@]:-}"
    )
}
result="$(run_vault_warning false true "")"
grep -Fq -- '--skip-vault: a vault StatefulSet is running in namespace vault;' <<< "${result}" \
    || fail "running Vault under --skip-vault was not reported:" "${result}"
grep -Fq 'Installing without Vault' <<< "${result}" || fail "the running-Vault warning does not point at the README section:" "${result}"
grep -Fq 'keeps the Vault resources in the nico-prereqs release until no Deployment in nico-system or cert-manager Certificate references Vault, then removes them' <<< "${result}" \
    || fail "the running-Vault warning does not describe the retention of the Vault resources:" "${result}"
grep -Fxq 'get statefulset vault -n vault' "${KUBECTL_LOG}" || fail "the running-Vault check did not query the default namespace:" "$(cat "${KUBECTL_LOG}")"
result="$(run_vault_warning false true "vault-site")"
grep -Fq 'in namespace vault-site;' <<< "${result}" || fail "VAULT_NS was not honored by the running-Vault check:" "${result}"
result="$(run_vault_warning false false "")"
[[ -z "${result}" ]] || fail "an absent Vault produced a warning:" "${result}"
result="$(run_vault_warning true true "")"
[[ -z "${result}" && ! -s "${KUBECTL_LOG}" ]] || fail "the Vault path queried or warned about the Vault StatefulSet:" "${result}"
unset -f kubectl

# ---------------------------------------------------------------------------
# 4. The three-node requirement names only the HA stores that are installed.
# ---------------------------------------------------------------------------
node_check="$(sed -n '/^    if \[\[ "${_schedulable}" -lt 3 \]\]; then$/,/^    fi$/p' "${PREFLIGHT_SH}")"
[[ -n "${node_check}" ]] || fail "could not extract the three-node check"
node_message() { # $1 INSTALL_VAULT
    (
        INSTALL_VAULT="$1"
        _schedulable=2
        _total=3
        ERRORS=()
        eval "${node_check}"
        printf '%s\n' "${ERRORS[@]}"
    )
}
[[ "$(node_message true)" == "Only 2/3 nodes are schedulable (Ready + untainted) — at least 3 required for HA Vault and Postgres" ]] \
    || fail "the Vault path changed the three-node message:" "$(node_message true)"
[[ "$(node_message false)" == "Only 2/3 nodes are schedulable (Ready + untainted) — at least 3 required for HA Postgres" ]] \
    || fail "--skip-vault still demands nodes for HA Vault:" "$(node_message false)"

# ---------------------------------------------------------------------------
# 5. --skip-vault with legacy PSM/NSM hook resources is an error before any
#    mutation, a failed inspection is a warning, and the Vault path never
#    probes. The probes name the same resources as cleanup-legacy-flow-managers.sh.
# ---------------------------------------------------------------------------
CLEANUP_SH="${SCRIPT_DIR}/../cleanup-legacy-flow-managers.sh"
for literal in 'job/flow-vault-tokens serviceaccount/flow-vault-tokens-sa' \
               'secret/psm-vault-token secret/nsm-vault-token' \
               'clusterrole/flow-vault-tokens-writer' \
               'clusterrolebinding/flow-vault-tokens-writer'; do
    { grep -Fq -- "${literal}" "${PREFLIGHT_SH}" && grep -Fq -- "${literal}" "${CLEANUP_SH}"; } \
        || fail "preflight.sh and cleanup-legacy-flow-managers.sh no longer probe the same legacy marker: ${literal}"
done
for fn in _legacy_flow_manager_markers _check_skip_vault_legacy_flow_managers; do
    eval "$(sed -n "/^${fn}()/,/^}/p" "${PREFLIGHT_SH}")"
    declare -f "${fn}" >/dev/null || fail "could not extract ${fn} from preflight.sh"
done
call_line="$(grep -nxF '    _check_skip_vault_legacy_flow_managers' "${PREFLIGHT_SH}" | cut -d: -f1)"
reachable_line="$(grep -nxF 'if [[ "${_CLUSTER_REACHABLE}" == "true" ]]; then' "${PREFLIGHT_SH}" | cut -d: -f1)"
[[ -n "${call_line}" && -n "${reachable_line}" && "${reachable_line}" -lt "${call_line}" ]] \
    || fail "the legacy-marker check is not called once inside the cluster-reachable section"

kubectl() {
    printf '%s\n' "$*" >> "${KUBECTL_LOG}"
    case "${FAKE_MARKERS}:$*" in
        error:*) return 1 ;;
        'present:get job/flow-vault-tokens serviceaccount/flow-vault-tokens-sa -n nico-system --ignore-not-found -o name')
            printf 'job.batch/flow-vault-tokens\nserviceaccount/flow-vault-tokens-sa\n' ;;
        'present:get clusterrole/flow-vault-tokens-writer clusterrolebinding/flow-vault-tokens-writer --ignore-not-found -o name')
            printf 'clusterrole.rbac.authorization.k8s.io/flow-vault-tokens-writer\n' ;;
        *) ;;
    esac
}
run_legacy_check() { # $1 INSTALL_VAULT, $2 fake marker mode (present, absent, error)
    : > "${KUBECTL_LOG}"
    (
        INSTALL_VAULT="$1"
        FAKE_MARKERS="$2"
        ERRORS=()
        WARNINGS=()
        BLOCKING_ERRORS=()
        _check_skip_vault_legacy_flow_managers
        printf 'B: %s\n' "${BLOCKING_ERRORS[@]:-}"
        printf 'E: %s\n' "${ERRORS[@]:-}"
        printf 'W: %s\n' "${WARNINGS[@]:-}"
    )
}
result="$(run_legacy_check false present)"
expected=$'B: --skip-vault: legacy PSM/NSM hook resources exist (job.batch/flow-vault-tokens serviceaccount/flow-vault-tokens-sa clusterrole.rbac.authorization.k8s.io/flow-vault-tokens-writer) and their cleanup revokes Vault tokens, so it needs Vault — run setup.sh once with Vault installed before using --skip-vault (see helm-prereqs/README.md, section "Installing without Vault")\nE: \nW: '
[[ "${result}" == "${expected}" ]] || fail "legacy markers under --skip-vault were not reported as one blocking error naming them:" "${result}"
[[ "$(grep -c . "${KUBECTL_LOG}")" -eq 3 ]] || fail "expected exactly the three marker probes:" "$(cat "${KUBECTL_LOG}")"
grep -Fxq 'get secret/psm-vault-token secret/nsm-vault-token -n flow --ignore-not-found -o name' "${KUBECTL_LOG}" \
    || fail "the Flow token Secrets were not probed in the flow namespace:" "$(cat "${KUBECTL_LOG}")"
result="$(run_legacy_check false absent)"
[[ "${result}" == $'B: \nE: \nW: ' ]] || fail "a site without legacy markers produced findings:" "${result}"
result="$(run_legacy_check false error)"
{ ! grep -Eq '^[BE]: .' <<< "${result}" && grep -Fq 'W: --skip-vault: could not inspect the legacy PSM/NSM hook resources' <<< "${result}"; } \
    || fail "a failed marker inspection was not reported as a warning:" "${result}"
result="$(run_legacy_check true present)"
[[ "${result}" == $'B: \nE: \nW: ' && ! -s "${KUBECTL_LOG}" ]] || fail "the Vault path probed or reported legacy markers:" "${result}"
unset -f kubectl

# ---------------------------------------------------------------------------
# 6. --skip-vault consumers this run does not redeploy are a warning that lists
#    them: Core with --skip-core, Flow and the site-agent with --skip-rest, RMS
#    with --skip-rms, and any Certificate outside those releases.
# ---------------------------------------------------------------------------
for fn in _vault_consumers _check_skip_vault_consumers_not_redeployed; do
    eval "$(sed -n "/^${fn}()/,/^}/p" "${PREFLIGHT_SH}")"
    declare -f "${fn}" >/dev/null || fail "could not extract ${fn} from preflight.sh"
done
consumers_call_line="$(grep -nxF '    _check_skip_vault_consumers_not_redeployed' "${PREFLIGHT_SH}" | cut -d: -f1)"
[[ -n "${consumers_call_line}" && "${reachable_line}" -lt "${consumers_call_line}" ]] \
    || fail "the consumer warning is not called once inside the cluster-reachable section"
# health-check.sh mirrors both consumer probes; keep the literals in step.
for probe in '{range .items[*]}{.metadata.name}{" "}{range .spec.template.spec.containers[*].env[*]}{.name}{","}{end}{"\n"}{end}' \
             '{range .items[*]}{.metadata.namespace}{"/"}{.metadata.name}{" "}{.spec.issuerRef.name}{"\n"}{end}' \
             "doesn't have a resource type|no matches for kind|could not find the requested resource"; do
    { grep -Fq -- "${probe}" "${PREFLIGHT_SH}" && grep -Fq -- "${probe}" "${SCRIPT_DIR}/../health-check.sh"; } \
        || fail "preflight.sh and health-check.sh no longer share the consumer probe: ${probe}"
done

kubectl() {
    printf '%s\n' "$*" >> "${KUBECTL_LOG}"
    case "${FAKE_CLUSTER}:$*" in
        error:*) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; return 1 ;;
        consumers:'get deployments -n nico-system -o jsonpath='*)
            printf 'carbide-api DATASTORE_USER,VAULT_ADDR,\nnico-bmc-proxy RUST_LOG,\n' ;;
        consumers:'get certificates.cert-manager.io -A -o jsonpath='*)
            printf 'flow/flow-certificate vault-nico-issuer\nrack-manager/rms-api-server-certificate vault-nico-issuer\n'
            printf 'nico-system/nico-api-certificate vault-nico-issuer\nmonitoring/grafana-tls vault-nico-issuer\n'
            printf 'nico-rest/core-grpc-client-site-agent-certs site-issuer\n' ;;
        none:'get deployments -n nico-system -o jsonpath='*) printf 'nico-api DATASTORE_USER,\n' ;;
        none:'get certificates.cert-manager.io -A -o jsonpath='*) printf 'nico-system/nico-api-certificate site-issuer\n' ;;
        nocrd:'get certificates.cert-manager.io -A -o jsonpath='*)
            echo 'error: the server doesn'"'"'t have a resource type "certificates"' >&2; return 1 ;;
        nocrd:*) ;;
        *) ;;
    esac
}
run_consumers_check() { # $1 INSTALL_VAULT, $2 SKIP_CORE, $3 SKIP_REST, $4 INSTALL_RMS, $5 fake cluster (consumers, none, nocrd, error)
    : > "${KUBECTL_LOG}"
    (
        INSTALL_VAULT="$1"
        SKIP_CORE="$2"
        SKIP_REST="$3"
        INSTALL_RMS="$4"
        FAKE_CLUSTER="$5"
        WARNINGS=()
        ERRORS=()
        BLOCKING_ERRORS=()
        _check_skip_vault_consumers_not_redeployed
        printf 'B: %s\n' "${BLOCKING_ERRORS[@]:-}"
        printf 'E: %s\n' "${ERRORS[@]:-}"
        printf 'W: %s\n' "${WARNINGS[@]:-}"
    )
}
result="$(run_consumers_check false true true false consumers)"
expected=$'B: \nE: \nW: --skip-vault: these still reference Vault and this run does not redeploy them: deployment/nico-system/carbide-api (VAULT_ADDR), certificate/flow/flow-certificate (vault-nico-issuer), certificate/rack-manager/rms-api-server-certificate (vault-nico-issuer), certificate/nico-system/nico-api-certificate (vault-nico-issuer), certificate/monitoring/grafana-tls (vault-nico-issuer); the Vault resources stay in nico-prereqs until they are migrated (redeployed without Vault, or their Certificates reissued from site-issuer)'
[[ "${result}" == "${expected}" ]] || fail "consumers that --skip-core, --skip-rest and --skip-rms leave in place were not listed in one warning:" "${result}"
result="$(run_consumers_check false false false true consumers)"
expected=$'B: \nE: \nW: --skip-vault: these still reference Vault and this run does not redeploy them: certificate/monitoring/grafana-tls (vault-nico-issuer); the Vault resources stay in nico-prereqs until they are migrated (redeployed without Vault, or their Certificates reissued from site-issuer)'
[[ "${result}" == "${expected}" ]] || fail "a run that redeploys Core, REST and RMS must list only the Certificate outside those releases:" "${result}"
result="$(run_consumers_check false true true false none)"
[[ "${result}" == $'B: \nE: \nW: ' ]] || fail "a site without Vault consumers produced a finding:" "${result}"
result="$(run_consumers_check false true true false nocrd)"
[[ "${result}" == $'B: \nE: \nW: ' ]] || fail "a cluster without cert-manager CRDs produced a finding:" "${result}"
result="$(run_consumers_check false true true false error)"
{ ! grep -Eq '^[BE]: .' <<< "${result}" && grep -Fq 'W: --skip-vault: could not inspect the Vault consumers' <<< "${result}"; } \
    || fail "a failed consumer probe was not reported as a warning:" "${result}"
result="$(run_consumers_check true true true false consumers)"
[[ "${result}" == $'B: \nE: \nW: ' && ! -s "${KUBECTL_LOG}" ]] || fail "the Vault path probed or warned about consumers:" "${result}"
unset -f kubectl

echo "preflight --skip-vault tests passed"
