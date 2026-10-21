#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/helm.log"
KUBECTL_LOG="${TEST_TMP_DIR}/kubectl.log"
export TEST_LOG KUBECTL_LOG

# A missing file is rejected before any cluster access.
if missing_output="$(bash "${SETUP_SH}" --flow-values "${TEST_TMP_DIR}/missing.yaml" 2>&1)"; then
    echo "setup accepted a missing --flow-values file" >&2
    exit 1
fi
if ! grep -Fq -- '--flow-values file not found' <<< "${missing_output}"; then
    echo "setup did not report the missing --flow-values file" >&2
    exit 1
fi

# Run the argument parser and the Flow phase with fake helm and kubectl, and
# check that every Flow helm invocation carries the values file.
flow_values="${TEST_TMP_DIR}/flow.yaml"
printf 'flowConfig:\n  inventoryRunFrequency: 1m30s\n' > "${flow_values}"

arg_parser="$(sed -n '/^AUTO_YES=false$/,/^done$/p' "${SETUP_SH}")"
flow_phase="$(sed -n '/^_SETUP_PHASE="\[7h\/7\] NICo Flow"$/,/^echo "NICo Flow deployed"$/p' "${SETUP_SH}")"
if [[ -z "${arg_parser}" || -z "${flow_phase}" ]]; then
    echo "could not extract the setup argument parser or the Flow phase" >&2
    exit 1
fi

mkdir -p "${TEST_TMP_DIR}/bin"
# The fake helm records every call and renders the namespace or Certificate
# documents for `template --show-only`, honouring the names a -f values file
# sets (certificate.name, temporalCert.name, namespaceOverride).
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${TEST_LOG}"
values=""; show=""
while (( $# )); do
    case "$1" in
        -f) values="$2"; shift ;;
        --show-only) show="$2"; shift ;;
    esac
    shift
done
val() { # $1 dotted key, $2 default
    [[ -n "$values" && -f "$values" ]] || { printf '%s' "$2"; return; }
    awk -v key="$1" -v def="$2" '
        BEGIN { split(key, parts, "."); depth = length(parts) }
        /^[^ #]/ { top = $1; sub(":", "", top) }
        depth == 1 && $1 == parts[1] ":" { print $2; found = 1; exit }
        depth == 2 && top == parts[1] && $1 == parts[2] ":" { print $2; found = 1; exit }
        END { if (!found) print def }' "$values"
}
ns="$(val namespaceOverride flow)"
case "$show" in
    templates/namespace.yaml) printf 'apiVersion: v1\nkind: Namespace\nmetadata:\n  name: %s\n' "$ns" ;;
    templates/certificate.yaml)
        printf 'apiVersion: cert-manager.io/v1\nkind: Certificate\nmetadata:\n  name: %s\n  namespace: %s\n---\napiVersion: cert-manager.io/v1\nkind: Certificate\nmetadata:\n  name: %s\n  namespace: %s\n' \
            "$(val certificate.name flow-certificate)" "$ns" "$(val temporalCert.name temporal-client-certs)" "$ns" ;;
esac
FAKE_HELM
# The fake kubectl records every call and, for `apply --dry-run=client -o name`,
# prints one <kind>/<name> per document on stdin as the real one does.
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
printf 'kubectl %s\n' "$*" >> "${KUBECTL_LOG}"
input="$(cat)"
case " $* " in
    *" --dry-run=client "*" -o name "*|*" -o name "*" --dry-run=client "*)
        printf '%s\n' "$input" | awk '
            /^kind:/ { kind = tolower($2) }
            /^  name:/ && !seen { name = $2; seen = 1 }
            /^---/ { if (kind != "") print kind "/" name; kind = ""; seen = 0 }
            END { if (kind != "") print kind "/" name }'
        ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/helm" "${TEST_TMP_DIR}/bin/kubectl"

: > "${TEST_LOG}"
(
    cd "${TEST_TMP_DIR}"
    export PATH="${TEST_TMP_DIR}/bin:${PATH}"
    export SCRIPT_DIR="${SCRIPT_DIR}/.."
    export NICO_IMAGE_REGISTRY="registry.example.com/nico"
    export NICO_REST_IMAGE_TAG="test-tag"
    unset REGISTRY_PULL_SECRET
    set -- --flow-values "flow.yaml"
    eval "${arg_parser}"
    eval "${flow_phase}"
) >/dev/null </dev/null

helm_calls="$(grep -c '^' "${TEST_LOG}")"
if [[ "${helm_calls}" -ne 3 ]]; then
    echo "expected 3 Flow helm invocations, got ${helm_calls}" >&2
    cat "${TEST_LOG}" >&2
    exit 1
fi
if [[ "$(grep -Fc -- "-f ${flow_values}" "${TEST_LOG}")" -ne 3 ]]; then
    echo "not every Flow helm invocation carries the --flow-values file" >&2
    cat "${TEST_LOG}" >&2
    exit 1
fi
if ! grep -Fq -- "upgrade --install flow " "${TEST_LOG}"; then
    echo "Flow helm install was not run" >&2
    exit 1
fi

# Without the option the Flow release is installed with no values file.
: > "${TEST_LOG}"
(
    export PATH="${TEST_TMP_DIR}/bin:${PATH}"
    export SCRIPT_DIR="${SCRIPT_DIR}/.."
    export NICO_IMAGE_REGISTRY="registry.example.com/nico"
    export NICO_REST_IMAGE_TAG="test-tag"
    unset REGISTRY_PULL_SECRET
    set --
    eval "${arg_parser}"
    eval "${flow_phase}"
) >/dev/null </dev/null
if grep -Fq -- ' -f ' "${TEST_LOG}"; then
    echo "Flow helm invocation carries a values file without --flow-values" >&2
    exit 1
fi

# A values file that renames the Certificates: the rendered names are the ones
# annotated and waited on.
renamed="${TEST_TMP_DIR}/renamed.yaml"
printf 'certificate:\n  name: site-flow-certificate\ntemporalCert:\n  name: site-temporal-certs\n' > "${renamed}"
: > "${TEST_LOG}"; : > "${KUBECTL_LOG}"
(
    cd "${TEST_TMP_DIR}"
    export PATH="${TEST_TMP_DIR}/bin:${PATH}"
    export SCRIPT_DIR="${SCRIPT_DIR}/.."
    export NICO_IMAGE_REGISTRY="registry.example.com/nico"
    export NICO_REST_IMAGE_TAG="test-tag"
    unset REGISTRY_PULL_SECRET
    set -- --flow-values "renamed.yaml"
    eval "${arg_parser}"
    eval "${flow_phase}"
) >/dev/null </dev/null
for cert in certificate/site-flow-certificate certificate/site-temporal-certs; do
    for verb in annotate label wait; do
        if ! grep -Fq -- "kubectl ${verb} " "${KUBECTL_LOG}" || ! grep -F -- "kubectl ${verb} " "${KUBECTL_LOG}" | grep -Fq -- "${cert} "; then
            echo "renamed Certificate ${cert} was not the target of kubectl ${verb}" >&2
            cat "${KUBECTL_LOG}" >&2
            exit 1
        fi
    done
done
if grep -Eq 'certificate/flow-certificate |certificate/temporal-client-certs ' "${KUBECTL_LOG}"; then
    echo "default Certificate names were still used after the rename" >&2
    cat "${KUBECTL_LOG}" >&2
    exit 1
fi

# A values file that moves the namespace is rejected before any resource is created.
moved="${TEST_TMP_DIR}/moved.yaml"
printf 'namespaceOverride: flow-site\n' > "${moved}"
: > "${TEST_LOG}"; : > "${KUBECTL_LOG}"
moved_rc=0
(
    cd "${TEST_TMP_DIR}"
    export PATH="${TEST_TMP_DIR}/bin:${PATH}"
    export SCRIPT_DIR="${SCRIPT_DIR}/.."
    export NICO_IMAGE_REGISTRY="registry.example.com/nico"
    export NICO_REST_IMAGE_TAG="test-tag"
    unset REGISTRY_PULL_SECRET
    set -- --flow-values "moved.yaml"
    eval "${arg_parser}"
    eval "${flow_phase}"
) > "${TEST_TMP_DIR}/moved.out" 2>&1 </dev/null || moved_rc=$?
if [[ "${moved_rc}" -eq 0 ]]; then
    echo "setup accepted a Flow values file with namespaceOverride" >&2
    exit 1
fi
if ! grep -Fq -- "Remove namespaceOverride" "${TEST_TMP_DIR}/moved.out"; then
    echo "setup did not explain the namespaceOverride rejection" >&2
    cat "${TEST_TMP_DIR}/moved.out" >&2
    exit 1
fi
if grep -v -- '--dry-run=client' "${KUBECTL_LOG}" | grep -q .; then
    echo "setup touched the cluster before rejecting namespaceOverride" >&2
    cat "${KUBECTL_LOG}" >&2
    exit 1
fi

echo "setup Flow values test passed"
