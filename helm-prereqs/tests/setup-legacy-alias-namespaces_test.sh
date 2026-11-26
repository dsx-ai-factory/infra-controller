#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Exercise the real Core chart with setup's namespace helper and a fake kubectl.
# The Core phase checks below also run its actual prompt and argument parser.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${TEST_DIR}/../setup.sh"
export TEST_REPO="$(cd "${TEST_DIR}/../.." && pwd)"
TEST_REAL_HELM="$(command -v helm)" \
    || { echo "the legacy alias namespace tests require Helm" >&2; exit 1; }
export TEST_REAL_HELM
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_CALLS="${TEST_TMP_DIR}/calls.log"
export TEST_RENDER_ARGS="${TEST_TMP_DIR}/render.args"
export TEST_INSTALL_ARGS="${TEST_TMP_DIR}/install.args"

fail() {
    echo "$1" >&2
    cat "${TEST_CALLS}" "${TEST_TMP_DIR}/output" >&2
    exit 1
}

namespace_helper="$(sed -n '/^_prepare_core_legacy_alias_namespaces()/,/^}/p' "${SETUP_SH}")"
arg_parser="$(sed -n '/^AUTO_YES=false$/,/^done$/p' "${SETUP_SH}")"
core_phase="$(sed -n '/^_CORE_INSTALLED_THIS_RUN=false$/,/^fi$/p' "${SETUP_SH}")"
[[ -n "${namespace_helper}" && -n "${arg_parser}" && -n "${core_phase}" ]] \
    || { echo "could not extract the Core namespace helper and deployment phase" >&2; exit 1; }

mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf 'kubectl %s\n' "$*" >> "${TEST_CALLS}"
case "$*" in
    'get namespace '*' --ignore-not-found -o name')
        [[ "${TEST_FAIL_GET}" != true ]] || exit 1
        if [[ " ${TEST_EXISTING_NAMESPACES} " == *" $3 "* ]]; then
            printf 'namespace/%s\n' "$3"
        fi
        ;;
    'create namespace '*)
        [[ "${TEST_FAIL_CREATE}" != true ]] || exit 1
        ;;
    *)
        echo "unexpected kubectl invocation: $*" >&2
        exit 1
        ;;
esac
KUBECTL
cat > "${TEST_TMP_DIR}/bin/helm" <<'HELM'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    template)
        echo 'helm template' >> "${TEST_CALLS}"
        printf '%s\n' "${@:2}" > "${TEST_RENDER_ARGS}"
        "${TEST_REAL_HELM}" "$@"
        if [[ "${TEST_RENDER_MODE}" == malformed ]]; then
            printf '\n---\nbroken: [\n'
        elif [[ "${TEST_RENDER_MODE}" == decoys ]]; then
            # These are not legacy alias Services. The Secret also detects
            # accidental capture of the full manifest stream under bash -x.
            cat <<'YAML'
---
apiVersion: v1
kind: Secret
metadata:
  name: test-rendered-secret
  namespace: untouched-secret
  labels:
    nico.nvidia.com/legacy-alias-of: nico-api
stringData:
  password: namespace-render-must-not-leak
---
apiVersion: v1
kind: Service
metadata:
  name: unrelated-external-name
  namespace: untouched-external
spec:
  type: ExternalName
  externalName: example.invalid
---
apiVersion: v1
kind: Service
metadata:
  name: labelled-cluster-ip
  namespace: untouched-clusterip
  labels:
    nico.nvidia.com/legacy-alias-of: nico-api
spec:
  type: ClusterIP
YAML
        fi
        ;;
    upgrade)
        [[ "$2" == --install ]] || exit 1
        echo 'helm upgrade' >> "${TEST_CALLS}"
        printf '%s\n' "${@:3}" > "${TEST_INSTALL_ARGS}"
        ;;
    *)
        echo "unexpected helm invocation: $*" >&2
        exit 1
        ;;
esac
HELM
chmod +x "${TEST_TMP_DIR}/bin/"*
export PATH="${TEST_TMP_DIR}/bin:${PATH}"

# A separate shell preserves set -e behavior when a failed helper is expected.
# Arguments after the values file model overrides appended to the final command.
printf 'set -euo pipefail\n%s\n' "${namespace_helper}" > "${TEST_TMP_DIR}/helper.sh"
cat >> "${TEST_TMP_DIR}/helper.sh" <<'RUNNER'
SCRIPT_DIR="${TEST_REPO}/helm-prereqs"
values_file="$1"
shift
NICO_CORE_CMD=(
    helm upgrade --install nico ./helm
    --namespace nico-system -f "${values_file}"
    --set-string global.image.repository=registry.example.invalid/nvmetal-carbide
    --set-string global.image.tag=test
    --timeout 600s --wait
    "$@"
)
_prepare_core_legacy_alias_namespaces
(cd "${TEST_REPO}" && "${NICO_CORE_CMD[@]}")
RUNNER

printf 'set -euo pipefail\n%s\n' "${namespace_helper}" > "${TEST_TMP_DIR}/core.sh"
cat >> "${TEST_TMP_DIR}/core.sh" <<'RUNNER'
SCRIPT_DIR="${TEST_REPO}/helm-prereqs"
NICO_IMAGE_REGISTRY=registry.example.invalid
NICO_CORE_IMAGE_TAG=test
NICO_KMIP_CLIENT_DIR=""
_BMC_V0_BOOTSTRAP_ENABLED=false
_current_core_uses_setup_bmc_v0_secret() { return 1; }
RUNNER
printf '%s\n%s\n' "${arg_parser}" "${core_phase}" >> "${TEST_TMP_DIR}/core.sh"

printf '{}\n' > "${TEST_TMP_DIR}/default.yaml"
cat > "${TEST_TMP_DIR}/disabled.yaml" <<'YAML'
nico-api:
  legacyAlias:
    enabled: false
nico-pxe:
  legacyAlias:
    enabled: false
YAML
printf 'nico-api: {legacyAlias: {enabled: false}}\n' > "${TEST_TMP_DIR}/pxe-only.yaml"
cat > "${TEST_TMP_DIR}/custom values.yaml" <<'YAML'
nico-api:
  legacyAlias:
    aliases:
      - name: carbide-api
        namespace: api-compat
nico-pxe:
  legacyAlias:
    aliases:
      - name: carbide-pxe
        namespace: pxe-compat
YAML
cat > "${TEST_TMP_DIR}/chart-owned.yaml" <<'YAML'
nico-api:
  legacyAlias:
    aliases:
      - name: carbide-api
        namespace: forge-system
        createNamespace: true
YAML
printf 'invalid: [\n' > "${TEST_TMP_DIR}/invalid.yaml"

reset_case() {
    : > "${TEST_CALLS}"
    : > "${TEST_TMP_DIR}/output"
    export TEST_FAIL_GET=false TEST_FAIL_CREATE=false
    export TEST_EXISTING_NAMESPACES="" TEST_RENDER_MODE=normal
}

assert_calls() {
    [[ "$(cat "${TEST_CALLS}")" == "$1" ]] || fail "${CASE}: unexpected command order"
}

default_calls=$'helm template\nkubectl get namespace forge-system --ignore-not-found -o name\nkubectl create namespace forge-system\nhelm upgrade'

# Each row adds a distinct selection or ownership boundary. The default proves
# that the API and PXE aliases sharing forge-system create it only once.
while IFS='|' read -r CASE values_file namespaces; do
    reset_case
    bash "${TEST_TMP_DIR}/helper.sh" "${TEST_TMP_DIR}/${values_file}" \
        > "${TEST_TMP_DIR}/output" 2>&1 || fail "${CASE}: namespace preparation failed"
    expected='helm template'
    for namespace in ${namespaces}; do
        expected+=$'\n'"kubectl get namespace ${namespace} --ignore-not-found -o name"
        expected+=$'\n'"kubectl create namespace ${namespace}"
    done
    expected+=$'\nhelm upgrade'
    assert_calls "${expected}"
    cmp -s "${TEST_RENDER_ARGS}" "${TEST_INSTALL_ARGS}" \
        || fail "${CASE}: rendering and installation received different arguments"
done <<'CASES'
shared default namespace|default.yaml|forge-system
aliases disabled|disabled.yaml|
PXE alias without API alias|pxe-only.yaml|forge-system
separate custom namespaces|custom values.yaml|api-compat pxe-compat
namespace rendered by the chart|chart-owned.yaml|
CASES

CASE="existing namespace remains untouched"
reset_case
export TEST_EXISTING_NAMESPACES=forge-system
bash "${TEST_TMP_DIR}/helper.sh" "${TEST_TMP_DIR}/default.yaml" \
    > "${TEST_TMP_DIR}/output" 2>&1 || fail "${CASE}: preparation failed"
assert_calls $'helm template\nkubectl get namespace forge-system --ignore-not-found -o name\nhelm upgrade'

CASE="final --set overrides win over the values file"
reset_case
bash "${TEST_TMP_DIR}/helper.sh" "${TEST_TMP_DIR}/custom values.yaml" \
    --set-string 'nico-api.legacyAlias.aliases[0].namespace=final-compat' \
    --set-string 'nico-pxe.legacyAlias.aliases[0].namespace=final-compat' \
    > "${TEST_TMP_DIR}/output" 2>&1 || fail "${CASE}: preparation failed"
assert_calls $'helm template\nkubectl get namespace final-compat --ignore-not-found -o name\nkubectl create namespace final-compat\nhelm upgrade'
cmp -s "${TEST_RENDER_ARGS}" "${TEST_INSTALL_ARGS}" \
    || fail "${CASE}: rendering lost final overrides"

CASE="only labelled ExternalName Services select namespaces, without leaking manifests"
reset_case
export TEST_RENDER_MODE=decoys
bash -x "${TEST_TMP_DIR}/helper.sh" "${TEST_TMP_DIR}/default.yaml" \
    > "${TEST_TMP_DIR}/output" 2>&1 || fail "${CASE}: preparation failed"
assert_calls "${default_calls}"
! grep -Fq 'namespace-render-must-not-leak' "${TEST_TMP_DIR}/output" \
    || fail "${CASE}: debug tracing exposed a rendered Secret"

# A rendering, parsing, lookup, or creation error must stop the deployment.
for CASE in render parse lookup create; do
    reset_case
    values_file="${TEST_TMP_DIR}/default.yaml"
    expected='helm template'
    case "${CASE}" in
        render) values_file="${TEST_TMP_DIR}/invalid.yaml" ;;
        parse) export TEST_RENDER_MODE=malformed ;;
        lookup) export TEST_FAIL_GET=true ;;
        create) export TEST_FAIL_CREATE=true ;;
    esac
    if [[ "${CASE}" == lookup || "${CASE}" == create ]]; then
        expected+=$'\nkubectl get namespace forge-system --ignore-not-found -o name'
    fi
    if [[ "${CASE}" == create ]]; then
        expected+=$'\nkubectl create namespace forge-system'
    fi
    if bash "${TEST_TMP_DIR}/core.sh" --skip-dpf --install-vault -y \
            --core-values "${values_file}" \
            > "${TEST_TMP_DIR}/output" 2>&1; then
        fail "${CASE}: failure did not stop deployment"
    fi
    assert_calls "${expected}"
done

# Run setup's actual argument parsing, consent gate, helper call, and Helm call.
# Other phases are excluded; the unrelated existing-BMC query is stubbed above.
for CASE in auto accepted declined skipped; do
    reset_case
    flags=(--skip-dpf --install-vault --core-values "${TEST_TMP_DIR}/default.yaml")
    reply=""
    expected="${default_calls}"
    case "${CASE}" in
        auto) flags+=(-y) ;;
        accepted) reply=Y ;;
        declined) reply=n; expected="" ;;
        skipped) flags+=(--skip-core); expected="" ;;
    esac
    : > "${TEST_TMP_DIR}/input"
    if [[ -n "${reply}" ]]; then
        printf '%s\n' "${reply}" > "${TEST_TMP_DIR}/input"
    fi
    bash "${TEST_TMP_DIR}/core.sh" "${flags[@]}" \
        < "${TEST_TMP_DIR}/input" > "${TEST_TMP_DIR}/output" 2>&1 \
        || fail "${CASE}: the Core deployment phase failed"
    assert_calls "${expected}"
done

echo "setup legacy alias namespace tests passed"
