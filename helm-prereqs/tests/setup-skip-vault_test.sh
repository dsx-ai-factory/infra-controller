#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# setup.sh --skip-vault: the flag and its environment variables resolve and
# validate INSTALL_VAULT; phases 2 to 5 install no Vault, wait on no Vault
# resource, still prepare the nico-system namespace and SSH host key, and keep
# the Vault resources in nico-prereqs while any Deployment or Certificate still
# references Vault (released at the end of the run once nothing does); the
# RMS, Core, Flow, and site-agent Helm commands switch their issuer to
# site-issuer and disable nico-api's Vault client. The default (Vault) path is
# asserted beside every skip case, so a regression there fails here too.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_CALLS="${TEST_TMP_DIR}/calls.log"

# Print the lines of $1 from the unique line $2 through the first line $3 after it.
extract_section() {
    awk -v start="$2" -v end="$3" '
        $0 == start { if (found) exit 1; found = 1; copying = 1 }
        copying { print }
        copying && $0 == end { copying = 0 }
        END { if (!found || copying) exit 1 }
    ' "$1"
}

fail() {
    echo "$1" >&2
    shift
    for extra in "$@"; do
        echo "--- ${extra}" >&2
        cat "${extra}" >&2 2>/dev/null || printf '%s\n' "${extra}" >&2
    done
    exit 1
}

arg_parser="$(sed -n '/^AUTO_YES=false$/,/^done$/p' "${SETUP_SH}")"
[[ -n "${arg_parser}" ]] || fail "could not extract the setup argument parser"

# ---------------------------------------------------------------------------
# 1. Parsing: every spelling of the opt-out clears INSTALL_VAULT, an explicit
#    --install-vault wins over the environment, and the default keeps Vault.
# ---------------------------------------------------------------------------
parse() { # $1 environment assignments (may be empty); remaining args: setup.sh flags
    local assignments="$1"
    shift
    (
        eval "${assignments}"
        eval "${arg_parser}"
        printf '%s' "${INSTALL_VAULT}"
    )
}
while IFS='|' read -r name assignments flags expected; do
    # shellcheck disable=SC2086 # The flags column holds zero or more words.
    actual="$(parse "${assignments}" ${flags})"
    [[ "${actual}" == "${expected}" ]] || fail "${name}: INSTALL_VAULT=${actual}, want ${expected}"
done <<'CASES'
default|||true
--skip-vault flag||--skip-vault|false
NICO_SKIP_VAULT=true|NICO_SKIP_VAULT=true||false
NICO_INSTALL_VAULT=false|NICO_INSTALL_VAULT=false||false
NICO_SKIP_VAULT=false keeps the default|NICO_SKIP_VAULT=false||true
--install-vault overrides NICO_SKIP_VAULT|NICO_SKIP_VAULT=true|--install-vault|true
--skip-vault after --install-vault||--install-vault --skip-vault|false
CASES

# Invalid variable values are rejected by name before anything touches the
# cluster (the full script exits during validation, before preflight).
while IFS='|' read -r assignment expected; do
    if output="$(env "${assignment}" bash "${SETUP_SH}" 2>&1)"; then
        fail "setup accepted ${assignment}"
    fi
    grep -Fq "${expected}" <<< "${output}" || fail "setup did not reject ${assignment} by name" "${output}"
done <<'CASES'
NICO_INSTALL_VAULT=maybe|NICO_INSTALL_VAULT must be true or false (got 'maybe')
NICO_SKIP_VAULT=yes|NICO_SKIP_VAULT must be true or false (got 'yes')
CASES
if usage="$(bash "${SETUP_SH}" --no-such-flag 2>&1)"; then
    fail "setup accepted an unknown flag"
fi
grep -Fq -- '[--skip-vault]' <<< "${usage}" || fail "the usage line does not list --skip-vault" "${usage}"

# ---------------------------------------------------------------------------
# 2. Phases 2 to 5 against fakes that record every call.
# ---------------------------------------------------------------------------
phases="$(extract_section "${SETUP_SH}" \
    '# 2. cert-manager + Prometheus CRDs + site CA / Vault TLS bootstrap' \
    '# The commit helm-prereqs/<name> is pinned to: the gitlink in a git checkout of')" \
    || fail "could not extract phases 2 to 5 from setup.sh"
# Phase 5 calls _vault_consumers, which setup.sh gets from the sourced preflight.sh.
vault_consumers_fn="$(sed -n '/^_vault_consumers()/,/^}/p' "${PREFLIGHT_SH}")"
[[ -n "${vault_consumers_fn}" ]] || fail "could not extract _vault_consumers from preflight.sh"
printf 'set -euo pipefail\n%s\n%s\n' "${vault_consumers_fn}" "${phases}" > "${TEST_TMP_DIR}/phases.sh"
# Surface the retention decision for the assertions below.
cat >> "${TEST_TMP_DIR}/phases.sh" <<'TRAILER'
printf 'RETAINED=%s\n' "${_VAULT_RESOURCES_RETAINED}"
TRAILER

mkdir -p "${TEST_TMP_DIR}/bin" "${TEST_TMP_DIR}/work/operators/crds"
# Answers the queries the Postgres and Vault waits poll, so every loop ends on
# its first pass; everything else succeeds silently and is only recorded.
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf 'kubectl %s\n' "$*" >> "${TEST_CALLS}"
case "$*" in
    'apply '*' -f -') cat >/dev/null ;;
    *'get statefulset vault --ignore-not-found -o jsonpath={.metadata.generation} {.status.observedGeneration}') printf '1 1' ;;
    *'get statefulset vault --ignore-not-found -o jsonpath={.status.updateRevision}') printf 'vault-5d8f' ;;
    'get postgresql nico-pg-cluster -n postgres -o jsonpath={.status.PostgresClusterStatus}') printf 'Running' ;;
    'get pods -n postgres -l application=spilo -o jsonpath='*) printf 'nico-pg-cluster-0 master\n' ;;
    'get secret nico-vault-approle-tokens -n nico-system -o jsonpath={.data.VAULT_ROLE_ID}') printf 'cm9sZQ==' ;;
    'get secret nico-vault-approle-tokens -n nico-system -o jsonpath={.data.VAULT_SECRET_ID}') printf 'c2VjcmV0' ;;
    'get deployments -n nico-system -o jsonpath={range .items[*]}'*)
        # FAKE_CORE_ENV: the Deployments and their env names (vault: a renamed Core that
        # references Vault; novault), none (absent), or a failed query (error).
        case "${FAKE_CORE_ENV:-absent}" in
            vault) printf 'carbide-api DATASTORE_USER,VAULT_ADDR,\nnico-bmc-proxy RUST_LOG,\n' ;;
            novault) printf 'nico-api DATASTORE_USER,\n' ;;
            absent) ;;
            error) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; exit 1 ;;
        esac ;;
    'get certificates.cert-manager.io -A -o jsonpath='*)
        # FAKE_CERTS: Certificates and their issuers (vault: Flow's SPIFFE certificate still
        # from vault-nico-issuer; none), no cert-manager CRDs (nocrd), or a failed query (error).
        case "${FAKE_CERTS:-none}" in
            vault) printf 'flow/flow-certificate vault-nico-issuer\nnico-rest/core-grpc-client-site-agent-certs site-issuer\n' ;;
            none) printf 'nico-system/nico-api-certificate site-issuer\n' ;;
            nocrd) echo 'error: the server doesn'"'"'t have a resource type "certificates"' >&2; exit 1 ;;
            error) echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2; exit 1 ;;
        esac ;;
    *) ;;
esac
FAKE_KUBECTL
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
printf 'helm %s\n' "$*" >> "${TEST_CALLS}"
printf 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: rendered\n'
FAKE_HELM
# The fake helmfile records the variable the gotmpl renders vault.enabled from.
cat > "${TEST_TMP_DIR}/bin/helmfile" <<'FAKE_HELMFILE'
#!/usr/bin/env bash
printf 'helmfile %s [NICO_PREREQS_VAULT_ENABLED=%s]\n' "$*" "${NICO_PREREQS_VAULT_ENABLED:-unset}" >> "${TEST_CALLS}"
FAKE_HELMFILE
printf '#!/usr/bin/env bash\nexit 0\n' > "${TEST_TMP_DIR}/bin/sleep"
printf '#!/usr/bin/env bash\nprintf "unseal_vault.sh\\n" >> "${TEST_CALLS}"\n' > "${TEST_TMP_DIR}/work/unseal_vault.sh"
printf '#!/usr/bin/env bash\nprintf "bootstrap_ssh_host_key.sh\\n" >> "${TEST_CALLS}"\n' > "${TEST_TMP_DIR}/work/bootstrap_ssh_host_key.sh"
chmod +x "${TEST_TMP_DIR}"/bin/* "${TEST_TMP_DIR}"/work/*.sh

_run_phases_script() { # $1 INSTALL_VAULT, $2 FAKE_CORE_ENV (vault, novault, absent, error), $3 FAKE_CERTS (vault, none, nocrd, error)
    (
        cd "${TEST_TMP_DIR}/work"
        export PATH="${TEST_TMP_DIR}/bin:${PATH}"
        export INSTALL_VAULT="$1" FAKE_CORE_ENV="$2" FAKE_CERTS="$3" VAULT_NS=vault CERT_MANAGER_NS=cert-manager \
            NICO_STORAGE_CLASS=local-path-persistent
        unset REGISTRY_PULL_SECRET NICO_PREREQS_VAULT_ENABLED
        timeout 60 bash "${TEST_TMP_DIR}/phases.sh" </dev/null
    )
}
run_phases() { # same arguments; the phases must succeed; output lands in PHASES_OUT
    : > "${TEST_CALLS}"
    PHASES_OUT="${TEST_TMP_DIR}/phases-$1-$2-$3.out"
    _run_phases_script "$1" "$2" "$3" > "${PHASES_OUT}" 2>&1 \
        || fail "phases 2 to 5 failed with INSTALL_VAULT=$1, Deployments $2, Certificates $3" "${PHASES_OUT}" "${TEST_CALLS}"
}
run_phases_expecting_abort() { # same arguments; the phases must exit non-zero
    : > "${TEST_CALLS}"
    PHASES_OUT="${TEST_TMP_DIR}/phases-$1-$2-$3.out"
    if _run_phases_script "$1" "$2" "$3" > "${PHASES_OUT}" 2>&1; then
        fail "phases 2 to 5 succeeded with INSTALL_VAULT=$1, Deployments $2, Certificates $3, expected an abort" "${PHASES_OUT}" "${TEST_CALLS}"
    fi
}
assert_called() {
    grep -Fq -- "$1" "${TEST_CALLS}" || fail "${CASE}: missing call: $1" "${TEST_CALLS}"
}
assert_not_called() {
    ! grep -Fq -- "$1" "${TEST_CALLS}" || fail "${CASE}: unexpected call: $1" "${TEST_CALLS}"
}
assert_printed() {
    grep -Fq -- "$1" "${PHASES_OUT}" || fail "${CASE}: missing output: $1" "${PHASES_OUT}"
}
assert_not_printed() {
    ! grep -Fq -- "$1" "${PHASES_OUT}" || fail "${CASE}: unexpected output: $1" "${PHASES_OUT}"
}
call_line() {
    grep -nF -- "$1" "${TEST_CALLS}" | head -1 | cut -d: -f1
}

CASE="--skip-vault phases on a fresh site"
run_phases false absent none
assert_printed "=== [2/6] cert-manager + site CA bootstrap ==="
assert_printed "Site CA bootstrap complete"
assert_printed "=== [3/6] vault === Skipped"
assert_printed "=== [4/6] nico-system namespace + SSH host key ==="
assert_called "helmfile sync -l name=cert-manager"
assert_called "kubectl apply --server-side -f operators/crds/ --field-manager=helmfile --force-conflicts"
# The render runs in the mode phase 5 installs (--set vault.enabled=false), so
# site-issuer comes out whatever the Vault toggles in values.yaml say.
assert_called "helm template nico-prereqs . --namespace nico-system --set imagePullSecrets.ngcNicoPull= --show-only templates/site-root-certificate.yaml --show-only templates/site-issuer.yaml --set vault.enabled=false"
assert_not_called "templates/vault-tls-certs.yaml"
assert_called "kubectl wait --for=condition=Ready certificate/site-root -n cert-manager --timeout=120s"
assert_not_called "certificate/nicoca-vault-client"
assert_not_called "certificate/vault-raft-tls"
assert_not_called "kubectl create namespace vault"
assert_not_called "helmfile sync -l name=vault"
assert_not_called "get statefulset vault"
assert_not_called "unseal_vault.sh"
assert_called "kubectl create namespace nico-system"
assert_called "kubectl label namespace nico-system app.kubernetes.io/managed-by=Helm --overwrite"
assert_called "kubectl annotate namespace nico-system meta.helm.sh/release-name=nico-prereqs meta.helm.sh/release-namespace=nico-system --overwrite"
assert_called "bootstrap_ssh_host_key.sh"
assert_called "helmfile sync -l name=external-secrets"
assert_called "helmfile sync -l name=nico-prereqs"
assert_called "kubectl get secret nico-system.nico.nico-pg-cluster.credentials -n nico-system"
assert_not_called "nico-vault-approle-tokens"
# No Deployment and no Vault-issued Certificate exists, so nothing needs the Vault
# resources: nico-prereqs renders without them.
assert_called "kubectl get deployments -n nico-system -o jsonpath={range .items[*]}"
assert_called "kubectl get certificates.cert-manager.io -A -o jsonpath="
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=false]"
assert_printed "RETAINED=false"
assert_not_printed "still reference Vault"
if ! (( $(call_line "kubectl label namespace nico-system") < $(call_line "bootstrap_ssh_host_key.sh") && \
        $(call_line "bootstrap_ssh_host_key.sh") < $(call_line "helmfile sync -l name=nico-prereqs") )); then
    fail "${CASE}: nico-system must be labeled before the SSH host key and the nico-prereqs sync" "${TEST_CALLS}"
fi

CASE="--skip-vault phases while a renamed Core still references Vault"
run_phases false vault none
assert_printed "These still reference Vault:"
assert_printed "  deployment/nico-system/carbide-api (VAULT_ADDR)"
assert_not_printed "nico-bmc-proxy"
assert_printed "Keeping the Vault resources in nico-prereqs (the Vault ConfigMap and token Secrets, and the"
assert_printed "cert-manager Vault-auth ServiceAccount and token that vault-nico-issuer renews through) until"
assert_printed "no consumer references Vault."
# nico-prereqs keeps rendering the Vault resources for the old Core, while Vault itself is
# still neither installed, unsealed, nor waited for.
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=true]"
assert_printed "RETAINED=true"
assert_called "--set vault.enabled=false"
assert_not_called "helmfile sync -l name=vault"
assert_not_called "unseal_vault.sh"
assert_not_called "nico-vault-approle-tokens"
assert_printed "=== [3/6] vault === Skipped"

CASE="--skip-vault phases while only a Certificate still references Vault"
run_phases false absent vault
assert_printed "  certificate/flow/flow-certificate (vault-nico-issuer)"
assert_not_printed "deployment/"
assert_not_printed "site-agent-certs"
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=true]"
assert_printed "RETAINED=true"

CASE="--skip-vault phases before cert-manager has CRDs"
run_phases false absent nocrd
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=false]"
assert_printed "RETAINED=false"

# A failed probe must never pass as "no consumers": the run stops before phase 5 touches anything.
CASE="--skip-vault phases when the Deployment probe fails"
run_phases_expecting_abort false error none
assert_printed "could not inspect the Vault consumers (Deployments in nico-system and cert-manager Certificates) to decide whether the Vault resources can go; fix cluster access and rerun"
assert_printed "Unable to connect to the server"
assert_not_called "helmfile sync -l name=external-secrets"
assert_not_called "helmfile sync -l name=nico-prereqs"
assert_not_printed "RETAINED="
CASE="--skip-vault phases when the Certificate probe fails"
run_phases_expecting_abort false absent error
assert_printed "could not inspect the Vault consumers"
assert_printed "Unable to connect to the server"
assert_not_called "helmfile sync -l name=nico-prereqs"

CASE="default (Vault) phases"
run_phases true absent none
assert_printed "=== [2/6] cert-manager + Vault TLS bootstrap ==="
assert_printed "Vault TLS bootstrap complete"
assert_printed "=== [3/6] vault ==="
assert_not_printed "Skipped"
assert_printed "=== [4/6] unseal vault ==="
assert_printed "Vault AppRole credentials ready"
assert_called "kubectl create namespace vault"
assert_called "helm template nico-prereqs . --namespace nico-system --set imagePullSecrets.ngcNicoPull= --show-only templates/site-root-certificate.yaml --show-only templates/site-issuer.yaml --show-only templates/vault-tls-certs.yaml"
assert_called "kubectl wait --for=condition=Ready certificate/site-root -n cert-manager --timeout=120s"
assert_called "kubectl wait --for=condition=Ready certificate/nicoca-vault-client -n vault --timeout=120s"
assert_called "kubectl wait --for=condition=Ready certificate/vault-raft-tls -n vault --timeout=120s"
assert_called "helmfile sync -l name=vault --set server.dataStorage.storageClass=local-path-persistent --set server.auditStorage.storageClass=local-path-persistent"
assert_called "kubectl -n vault get statefulset vault --ignore-not-found -o jsonpath={.status.updateRevision}"
assert_called "unseal_vault.sh"
assert_called "bootstrap_ssh_host_key.sh"
# The Vault path leaves the nico-system ownership markers to unseal_vault.sh.
assert_not_called "kubectl label namespace nico-system"
assert_not_called "kubectl annotate namespace nico-system"
assert_called "kubectl get secret nico-vault-approle-tokens -n nico-system -o jsonpath={.data.VAULT_ROLE_ID}"
assert_called "kubectl get secret nico-vault-approle-tokens -n nico-system -o jsonpath={.data.VAULT_SECRET_ID}"
# The Vault path never probes the consumers and renders nico-prereqs with its Vault resources.
assert_not_called "kubectl get deployments"
assert_not_called "kubectl get certificates"
assert_not_called "vault.enabled"
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=true]"
assert_printed "RETAINED=false"
if ! (( $(call_line "helmfile sync -l name=vault") < $(call_line "unseal_vault.sh") && \
        $(call_line "unseal_vault.sh") < $(call_line "bootstrap_ssh_host_key.sh") && \
        $(call_line "bootstrap_ssh_host_key.sh") < $(call_line "helmfile sync -l name=nico-prereqs") )); then
    fail "${CASE}: vault sync, unseal, SSH host key, and nico-prereqs sync ran out of order" "${TEST_CALLS}"
fi

# ---------------------------------------------------------------------------
# 2b. Retained Vault resources are released at the end of the run only when a
#     re-probe finds no consumer; otherwise the remaining ones are listed, and
#     a failed re-probe keeps them.
# ---------------------------------------------------------------------------
release_fn="$(sed -n '/^_release_retained_vault_resources()/,/^}/p' "${SETUP_SH}")"
[[ -n "${release_fn}" ]] || fail "could not extract _release_retained_vault_resources from setup.sh"
run_release() { # $1 _VAULT_RESOURCES_RETAINED, $2 FAKE_CORE_ENV, $3 FAKE_CERTS; output lands in RELEASE_OUT
    : > "${TEST_CALLS}"
    RELEASE_OUT="${TEST_TMP_DIR}/release-$1-$2-$3.out"
    (
        set -euo pipefail
        export PATH="${TEST_TMP_DIR}/bin:${PATH}"
        export NICO_PREREQS_VAULT_ENABLED=true FAKE_CORE_ENV="$2" FAKE_CERTS="$3"
        SCRIPT_DIR="${TEST_TMP_DIR}/work"
        _VAULT_RESOURCES_RETAINED="$1"
        _SETUP_PHASE=""
        eval "${vault_consumers_fn}"
        eval "${release_fn}"
        _release_retained_vault_resources
        printf 'NICO_PREREQS_VAULT_ENABLED=%s\n' "${NICO_PREREQS_VAULT_ENABLED}"
    ) > "${RELEASE_OUT}" 2>&1 || fail "_release_retained_vault_resources failed with retained=$1, Deployments $2, Certificates $3" "${RELEASE_OUT}"
}
CASE="retained, no consumer remains"
run_release true absent none
assert_called "helmfile sync -l name=nico-prereqs [NICO_PREREQS_VAULT_ENABLED=false]"
grep -Fq 'No consumer references Vault any more: removing the Vault resources from nico-prereqs' "${RELEASE_OUT}" \
    || fail "${CASE}: the removal was not announced" "${RELEASE_OUT}"
grep -Fxq 'NICO_PREREQS_VAULT_ENABLED=false' "${RELEASE_OUT}" || fail "${CASE}: the render variable was not exported as false" "${RELEASE_OUT}"
CASE="retained, a Certificate still references Vault"
run_release true absent vault
! grep -Fq "helmfile" "${TEST_CALLS}" || fail "${CASE}: nico-prereqs was re-synced while a consumer remains" "${TEST_CALLS}"
grep -Fq 'These still reference Vault, so the Vault resources stay in nico-prereqs:' "${RELEASE_OUT}" \
    || fail "${CASE}: the retention notice is missing" "${RELEASE_OUT}"
grep -Fxq '  certificate/flow/flow-certificate (vault-nico-issuer)' "${RELEASE_OUT}" || fail "${CASE}: the remaining consumer is not listed" "${RELEASE_OUT}"
grep -Fq 'A later setup.sh --skip-vault run that redeploys them (or reissues those Certificates from' "${RELEASE_OUT}" \
    || fail "${CASE}: the notice does not say how to finish" "${RELEASE_OUT}"
CASE="retained, the re-probe fails"
run_release true error none
! grep -Fq "helmfile" "${TEST_CALLS}" || fail "${CASE}: nico-prereqs was re-synced after a failed re-probe" "${TEST_CALLS}"
grep -Fq 'WARNING: could not re-inspect the Vault consumers; the Vault resources stay in nico-prereqs.' "${RELEASE_OUT}" \
    || fail "${CASE}: a failed re-probe was not reported as keeping the resources" "${RELEASE_OUT}"
CASE="nothing retained"
run_release false vault vault
[[ ! -s "${TEST_CALLS}" ]] || fail "${CASE}: the cluster was probed or nico-prereqs re-synced without retained resources" "${TEST_CALLS}"
! grep -Fq 'Vault' "${RELEASE_OUT}" || fail "${CASE}: a notice was printed without retained resources" "${RELEASE_OUT}"

# The release runs at every completion point: before the --skip-rest exit, before
# the exit taken when the REST deployment prompt is declined, and after phase 7i
# before the final completion marker; never between Core and the overlay.
[[ "$(grep -cE '^[[:space:]]*_release_retained_vault_resources$' "${SETUP_SH}")" -eq 3 ]] \
    || fail "setup.sh must call _release_retained_vault_resources at all three completion points"
sed -n '/^if "${SKIP_REST}"; then$/,/^fi$/p' "${SETUP_SH}" | grep -qxF '    _release_retained_vault_resources' \
    || fail "the --skip-rest exit does not release the retained Vault resources first"
rest_prompt_block="$(extract_section "${SETUP_SH}" 'if [[ "${_nico_reply:-Y}" =~ ^[Yy]$ ]]; then' 'fi')" \
    || fail "could not extract the REST deployment prompt decision"
rest_release_line="$(grep -nxF '    _release_retained_vault_resources' <<< "${rest_prompt_block}" | cut -d: -f1)"
rest_exit_line="$(grep -nxF '    exit 0' <<< "${rest_prompt_block}" | cut -d: -f1)"
[[ -n "${rest_release_line}" && -n "${rest_exit_line}" && "${rest_release_line}" -lt "${rest_exit_line}" ]] \
    || fail "a declined REST deployment prompt must release the retained Vault resources before exiting" "${rest_prompt_block}"
site_agent_line="$(grep -nF 'echo "=== [7i/7] NICo REST site-agent' "${SETUP_SH}" | cut -d: -f1)"
final_release_line="$(grep -nxF '_release_retained_vault_resources' "${SETUP_SH}" | cut -d: -f1)"
complete_line="$(grep -nF '_SETUP_PHASE="complete"  # signals' "${SETUP_SH}" | cut -d: -f1)"
core_installed_line="$(grep -nF '        _CORE_INSTALLED_THIS_RUN=true' "${SETUP_SH}" | cut -d: -f1)"
overlay_line="$(grep -nF '_SETUP_PHASE="site overlay"' "${SETUP_SH}" | cut -d: -f1)"
if ! (( site_agent_line < final_release_line && final_release_line < complete_line )); then
    fail "the final release must run after phase 7i and before the completion marker"
fi
if sed -n "${core_installed_line},${overlay_line}p" "${SETUP_SH}" | grep -q '_release_retained_vault_resources'; then
    fail "the retained Vault resources must not be released between the Core phase and the site overlay"
fi

# The gotmpl renders vault.enabled and the vaultroottoken exec from one expression:
# NICO_PREREQS_VAULT_ENABLED, then INSTALL_VAULT, then true.
GOTMPL="${SCRIPT_DIR}/../values/nico-prereqs-dynamic.yaml.gotmpl"
grep -Fxq '{{- $vaultEnabled := ne (env "NICO_PREREQS_VAULT_ENABLED" | default (env "INSTALL_VAULT" | default "true")) "false" }}' "${GOTMPL}" \
    || fail "the dynamic values do not derive vault.enabled from NICO_PREREQS_VAULT_ENABLED, then INSTALL_VAULT, then true"
grep -Fxq '  enabled: {{ $vaultEnabled }}' "${GOTMPL}" || fail "vault.enabled is not rendered from the shared expression"
sed -n '/^{{- if \$vaultEnabled }}$/,/^{{- end }}$/p' "${GOTMPL}" | grep -Fq 'exec "sh"' \
    || fail "the vaultroottoken exec is not gated by the shared expression"
[[ "$(grep -c 'env "INSTALL_VAULT"' "${GOTMPL}")" -eq 1 && "$(grep -c 'env "NICO_PREREQS_VAULT_ENABLED"' "${GOTMPL}")" -eq 1 ]] \
    || fail "the dynamic values read the Vault variables outside the precedence expression"

# ---------------------------------------------------------------------------
# 3. Helm command building for RMS (5c), Core (6), Flow (7h), and the
#    site-agent (7i): the issuer and Vault --set values appear only under
#    --skip-vault, and the manual Core command (printed when the operator
#    declines the deployment prompt with DPF off) shows them.
# ---------------------------------------------------------------------------
rms_block="$(extract_section "${SETUP_SH}" '    NICO_RMS_CMD=(' '    fi')" || fail "could not extract the RMS command block"
core_block="$(extract_section "${SETUP_SH}" '    NICO_CORE_CMD=(' '    done')" || fail "could not extract the Core command block"
flow_block="$(extract_section "${SETUP_SH}" 'NICO_FLOW_ARGS=(' 'fi')" || fail "could not extract the Flow arguments block"
agent_block="$(extract_section "${SETUP_SH}" 'NICO_SITE_AGENT_ARGS=(' 'fi')" || fail "could not extract the site-agent arguments block"

build() { # $1 block, $2 array name, $3 INSTALL_VAULT; prints the array on one line
    (
        set -euo pipefail
        INSTALL_VAULT="$3"
        INSTALL_DPF=true
        _BMC_V0_BOOTSTRAP_ENABLED=false
        SCRIPT_DIR=/prereqs
        _RMS_CHART=/charts/rms _RMS_NS=rack-manager
        NICO_RMS_IMAGE_REPO=registry.example.com/rms NICO_RMS_IMAGE_TAG=v0.8.0
        _CORE_VALUES_ARG=/values/core.yaml
        NICO_IMAGE_REGISTRY=registry.example.com/nico NICO_CORE_IMAGE_TAG=core-tag NICO_REST_IMAGE_TAG=rest-tag
        NICO_FLOW_NAMESPACE=flow
        eval "$1"
        local -n _built="$2"
        printf '%s\n' "${_built[*]}"
        [[ "$2" != NICO_CORE_CMD ]] || printf 'DISPLAY: %s\n' "${_NICO_CORE_CMD_DISPLAY}"
    )
}
expect_command() { # $1 case, $2 block, $3 array, $4 INSTALL_VAULT, $5 expected first line
    local built
    built="$(build "$2" "$3" "$4")" || fail "$1: the block failed under INSTALL_VAULT=$4"
    [[ "$(head -1 <<< "${built}")" == "$5" ]] || fail "$1: unexpected command" "${built}" "want: $5"
    printf '%s\n' "${built}"
}

issuer_sets="--set global.certificate.issuerRef.name=site-issuer --set global.certificate.issuerRef.kind=ClusterIssuer --set global.certificate.issuerRef.group=cert-manager.io"
vault_free_core_sets="${issuer_sets} --set global.certificate.useCertManager=true --set global.certificate.useCertManagerUfm=true --set nico-api.vault.enabled=false"
expect_command "RMS default" "${rms_block}" NICO_RMS_CMD true \
    "helm upgrade --install rack /charts/rms --namespace rack-manager -f /prereqs/values/rms.yaml --set namespace=rack-manager --set-string apiServer.image.repository=registry.example.com/rms --set-string global.image.tag=v0.8.0" >/dev/null
expect_command "RMS --skip-vault" "${rms_block}" NICO_RMS_CMD false \
    "helm upgrade --install rack /charts/rms --namespace rack-manager -f /prereqs/values/rms.yaml --set namespace=rack-manager --set-string apiServer.image.repository=registry.example.com/rms --set-string global.image.tag=v0.8.0 --set certificates.issuerRef.name=site-issuer --set certificates.issuerRef.kind=ClusterIssuer --set certificates.issuerRef.group=cert-manager.io" >/dev/null
core_default="$(expect_command "Core default" "${core_block}" NICO_CORE_CMD true \
    "helm upgrade --install nico ./helm --namespace nico-system -f /values/core.yaml --set-string global.image.repository=registry.example.com/nico/nvmetal-carbide --set-string global.image.tag=core-tag --timeout 600s --wait --set nico-api.dpf.rbacCreate=true")"
! grep -Fq "site-issuer" <<< "${core_default}" || fail "Core default: the manual command names site-issuer" "${core_default}"
core_skip="$(expect_command "Core --skip-vault" "${core_block}" NICO_CORE_CMD false \
    "helm upgrade --install nico ./helm --namespace nico-system -f /values/core.yaml --set-string global.image.repository=registry.example.com/nico/nvmetal-carbide --set-string global.image.tag=core-tag --timeout 600s --wait ${vault_free_core_sets} --set nico-api.dpf.rbacCreate=true")"
grep -Fq "DISPLAY: helm upgrade --install nico ./helm --namespace nico-system -f /values/core.yaml --set-string global.image.repository=registry.example.com/nico/nvmetal-carbide --set-string global.image.tag=core-tag --timeout 600s --wait ${vault_free_core_sets} --set nico-api.dpf.rbacCreate=true" <<< "${core_skip}" \
    || fail "Core --skip-vault: the manual command printed when the operator declines the Core deployment does not show the Vault-free values" "${core_skip}"
expect_command "Flow default" "${flow_block}" NICO_FLOW_ARGS true \
    "--namespace flow --create-namespace --set global.image.repository=registry.example.com/nico --set global.image.tag=rest-tag" >/dev/null
expect_command "Flow --skip-vault" "${flow_block}" NICO_FLOW_ARGS false \
    "--namespace flow --create-namespace --set global.image.repository=registry.example.com/nico --set global.image.tag=rest-tag ${issuer_sets}" >/dev/null
expect_command "site-agent default" "${agent_block}" NICO_SITE_AGENT_ARGS true \
    "--namespace nico-rest -f /prereqs/values/nico-site-agent.yaml --set global.image.repository=registry.example.com/nico --set global.image.tag=rest-tag" >/dev/null
expect_command "site-agent --skip-vault" "${agent_block}" NICO_SITE_AGENT_ARGS false \
    "--namespace nico-rest -f /prereqs/values/nico-site-agent.yaml --set global.image.repository=registry.example.com/nico --set global.image.tag=rest-tag ${issuer_sets}" >/dev/null

# The RMS ca.crt failure message names the issuer in use.
rms_issuer_line="$(grep -nF 'echo "  → Check the ${_RMS_ISSUER} CA chain configuration, or pre-create a"' "${SETUP_SH}" | cut -d: -f1)"
[[ -n "${rms_issuer_line}" ]] || fail "the RMS ca.crt failure message no longer names the issuer in use"

echo "setup --skip-vault tests passed"
