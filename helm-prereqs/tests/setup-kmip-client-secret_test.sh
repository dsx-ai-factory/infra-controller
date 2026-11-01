#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# setup.sh creates the KMIP client TLS Secret from NICO_KMIP_CLIENT_DIR before
# the Core deployment, refuses to touch a Secret it does not manage, replaces
# only the Secret version it inspected, and never puts key material on a
# command line.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

# Without a Core deployment nothing mounts the Secret, so the variable is an
# error with --skip-core, like the BMC bootstrap password.
conflict_output=""
if conflict_output="$(NICO_KMIP_CLIENT_DIR="${TEST_TMP_DIR}" \
        bash "${SETUP_SH}" --skip-core 2>&1)"; then
    echo "setup accepted NICO_KMIP_CLIENT_DIR with --skip-core" >&2
    exit 1
fi
if ! grep -Fq 'NICO_KMIP_CLIENT_DIR cannot be used with --skip-core' <<< "${conflict_output}"; then
    echo "setup did not explain the NICO_KMIP_CLIENT_DIR conflict with --skip-core" >&2
    exit 1
fi

# The Secret is written only after the operator confirms the Core deployment,
# so a declined run leaves a mounted Secret alone, and before the Core release,
# so the nico-api mount exists at install.
core_confirmation_line="$(grep -nF 'if [[ "${_reply:-Y}" =~ ^[Yy]$ ]]; then' "${SETUP_SH}" | cut -d: -f1)"
kmip_secret_line="$(grep -nF '            _create_kmip_client_secret "${NICO_KMIP_CLIENT_DIR}"' "${SETUP_SH}" | cut -d: -f1)"
core_deploy_line="$(grep -nF '(cd "${SCRIPT_DIR}/.." && "${NICO_CORE_CMD[@]}")' "${SETUP_SH}" | cut -d: -f1)"
if ! (( core_confirmation_line < kmip_secret_line && kmip_secret_line < core_deploy_line )); then
    echo "the KMIP client Secret must be created after the Core deployment is confirmed and before the Core release" >&2
    exit 1
fi

secret_creator="$(sed -n '/^_create_kmip_client_secret()/,/^}/p' "${SETUP_SH}")"
if [[ -z "${secret_creator}" ]]; then
    echo "could not extract _create_kmip_client_secret from setup.sh" >&2
    exit 1
fi
eval "${secret_creator}"
eval "$(grep '^_KMIP_CLIENT_SECRET_NAME=' "${SETUP_SH}")"
eval "$(grep '^_KMIP_CLIENT_SECRET_FILES=' "${SETUP_SH}")"

export TEST_SECRET_STATE="${TEST_TMP_DIR}/secret.json"
export TEST_KUBECTL_LOG="${TEST_TMP_DIR}/kubectl.log"
mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_KUBECTL_LOG}"
case "$*" in
    'get secret nico-kmip-client -n nico-system --ignore-not-found -o json')
        [[ "${FAKE_GET_FAILS:-false}" != "true" ]] || exit 1
        [[ ! -f "${TEST_SECRET_STATE}" ]] || cat "${TEST_SECRET_STATE}"
        if [[ "${FAKE_CHANGE_AFTER_GET:-false}" == "true" && -f "${TEST_SECRET_STATE}" ]]; then
            # Another writer updates the Secret right after setup.sh read it.
            changed="$(jq '.metadata.resourceVersion = ((.metadata.resourceVersion | tonumber) + 1 | tostring)' "${TEST_SECRET_STATE}")"
            printf '%s' "${changed}" > "${TEST_SECRET_STATE}"
        fi
        ;;
    'create secret generic nico-kmip-client -n nico-system --from-file=ca.pem='*' --from-file=client.pem='*' --from-file=client.key='*' --dry-run=client -o json')
        # Render the Secret the way kubectl does: each key holds its file, base64-encoded.
        data='{}'
        for arg in "$@"; do
            case "${arg}" in
                --from-file=*)
                    spec="${arg#--from-file=}"
                    data="$(jq --arg k "${spec%%=*}" --arg v "$(base64 < "${spec#*=}" | tr -d '\n')" \
                        '. + {($k): $v}' <<< "${data}")"
                    ;;
            esac
        done
        jq -n --argjson data "${data}" '{apiVersion: "v1", kind: "Secret",
            metadata: {name: "nico-kmip-client", namespace: "nico-system"}, data: $data}'
        ;;
    'create -f -'|'replace -f -')
        payload="$(cat)"
        if [[ "${FAKE_WRITE_FAILS:-false}" == "client" ]]; then
            echo 'Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout' >&2
            exit 1
        fi
        if [[ "${FAKE_WRITE_FAILS:-false}" == "true" ]]; then
            # A verbose server rejection that echoes the attempted object.
            printf 'Error from server (Invalid): error when replacing "STDIN": %s\nThe Secret "nico-kmip-client" is invalid: data: Forbidden: field is immutable when `immutable` is set\n' \
                "${payload}" >&2
            exit 1
        fi
        sent_version="$(jq -r '.metadata.resourceVersion // ""' <<< "${payload}")"
        if [[ "${1}" == "create" ]]; then
            if [[ -n "${sent_version}" ]]; then
                echo 'Error from server (BadRequest): resourceVersion should not be set on objects to be created' >&2
                exit 1
            fi
            stored_version=0
        else
            # Like the API server, honour the version in the manifest. A replacement
            # without one would be unconditional: kubectl fetches the current version.
            stored_version="$(jq -r '.metadata.resourceVersion' "${TEST_SECRET_STATE}")"
            if [[ -z "${sent_version}" ]]; then
                echo 'fake kubectl: replace without metadata.resourceVersion would be unconditional' >&2
                exit 1
            fi
            if [[ "${sent_version}" != "${stored_version}" ]]; then
                echo 'Error from server (Conflict): error when replacing "STDIN": Operation cannot be fulfilled on secrets "nico-kmip-client": the object has been modified; please apply your changes to the latest version and try again' >&2
                exit 1
            fi
        fi
        jq --arg rv "$((stored_version + 1))" '.metadata.resourceVersion = $rv' <<< "${payload}" > "${TEST_SECRET_STATE}"
        echo "secret/nico-kmip-client ${1}d"
        ;;
    *)
        echo "unexpected kubectl invocation: $*" >&2
        exit 1
        ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"
export PATH="${TEST_TMP_DIR}/bin:${PATH}"

client_dir="${TEST_TMP_DIR}/kmip"
mkdir -p "${client_dir}"
printf -- '-----BEGIN CERTIFICATE-----\nca-bundle-v1\n' > "${client_dir}/ca.pem"
printf -- '-----BEGIN CERTIFICATE-----\nclient-cert\n' > "${client_dir}/client.pem"

# A missing file fails before any cluster call.
: > "${TEST_KUBECTL_LOG}"
if missing_output="$(_create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup created the KMIP client Secret without client.key" >&2
    exit 1
fi
if ! grep -Fq 'has no readable, non-empty client.key' <<< "${missing_output}" || [[ -s "${TEST_KUBECTL_LOG}" ]]; then
    echo "a missing client.key was not reported before calling kubectl" >&2
    exit 1
fi

printf -- '-----BEGIN PRIVATE KEY-----\nsecret-key-material\n' > "${client_dir}/client.key"

# First run creates the labelled Secret from the three files.
: > "${TEST_KUBECTL_LOG}"
create_output="$(_create_kmip_client_secret "${client_dir}")"
if ! grep -Fq 'Creating Secret nico-system/nico-kmip-client' <<< "${create_output}" || \
   ! grep -Fxq 'create -f -' "${TEST_KUBECTL_LOG}"; then
    echo "first run did not create the KMIP client Secret" >&2
    exit 1
fi
if [[ "$(jq -r '.metadata.labels["app.kubernetes.io/managed-by"]' "${TEST_SECRET_STATE}")" != "helm-prereqs" ]]; then
    echo "the KMIP client Secret is missing its managed-by label" >&2
    exit 1
fi
for key in ca.pem client.pem client.key; do
    if [[ "$(jq -r --arg k "${key}" '.data[$k]' "${TEST_SECRET_STATE}" | base64 -d)" != "$(cat "${client_dir}/${key}")" ]]; then
        echo "Secret key ${key} does not hold the file content" >&2
        exit 1
    fi
done
if grep -Fq 'secret-key-material' "${TEST_KUBECTL_LOG}"; then
    echo "key material leaked into kubectl arguments" >&2
    exit 1
fi

# A rerun with renewed files updates the managed Secret, including under --debug tracing.
printf -- '-----BEGIN CERTIFICATE-----\nca-bundle-v2\n' > "${client_dir}/ca.pem"
: > "${TEST_KUBECTL_LOG}"
{
    set -x
    update_output="$(_create_kmip_client_secret "${client_dir}")"
    set +x
} 2>"${TEST_TMP_DIR}/xtrace.log"
if ! grep -Fq 'Updating Secret nico-system/nico-kmip-client' <<< "${update_output}" || \
   ! grep -Fxq 'replace -f -' "${TEST_KUBECTL_LOG}" || \
   [[ "$(jq -r '.data["ca.pem"]' "${TEST_SECRET_STATE}" | base64 -d)" != "$(cat "${client_dir}/ca.pem")" ]]; then
    echo "rerun did not update the managed KMIP client Secret" >&2
    exit 1
fi
encoded_key="$(base64 < "${client_dir}/client.key" | tr -d '\n')"
if grep -Fq 'secret-key-material' "${TEST_TMP_DIR}/xtrace.log" || \
   grep -Fq "${encoded_key}" "${TEST_TMP_DIR}/xtrace.log"; then
    echo "key material leaked into setup --debug output" >&2
    exit 1
fi

# A Secret without the managed-by label belongs to the operator and is left alone.
jq 'del(.metadata.labels)' "${TEST_SECRET_STATE}" > "${TEST_TMP_DIR}/operator-secret.json"
mv "${TEST_TMP_DIR}/operator-secret.json" "${TEST_SECRET_STATE}"
before_refusal="$(sha256sum "${TEST_SECRET_STATE}")"
: > "${TEST_KUBECTL_LOG}"
if refusal_output="$(_create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup overwrote a KMIP client Secret it does not manage" >&2
    exit 1
fi
if ! grep -Fq 'exists but is not managed by setup.sh' <<< "${refusal_output}" || \
   [[ "$(sha256sum "${TEST_SECRET_STATE}")" != "${before_refusal}" ]] || \
   grep -Eq '^(create|replace) -f -$' "${TEST_KUBECTL_LOG}"; then
    echo "the operator-managed Secret was not left untouched with an explanation" >&2
    exit 1
fi

# A rejected write is reported with kubectl's reason code only: a rejected object
# could carry the key, so kubectl's diagnostics are never relayed.
jq '.metadata.labels = {"app.kubernetes.io/managed-by": "helm-prereqs"}' "${TEST_SECRET_STATE}" > "${TEST_TMP_DIR}/managed-secret.json"
mv "${TEST_TMP_DIR}/managed-secret.json" "${TEST_SECRET_STATE}"
before_rejection="$(sha256sum "${TEST_SECRET_STATE}")"
if rejected_output="$(FAKE_WRITE_FAILS=true _create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup reported success although the server rejected the Secret" >&2
    exit 1
fi
if ! grep -Fq 'could not replace Secret nico-system/nico-kmip-client (kubectl reported Invalid)' <<< "${rejected_output}" || \
   [[ "$(sha256sum "${TEST_SECRET_STATE}")" != "${before_rejection}" ]]; then
    echo "a rejected Secret write was not reported with kubectl's reason code" >&2
    exit 1
fi
if grep -Fq 'field is immutable' <<< "${rejected_output}" || grep -Fq '"kind"' <<< "${rejected_output}" || \
   grep -Fq "${encoded_key}" <<< "${rejected_output}" || grep -Fq 'secret-key-material' <<< "${rejected_output}"; then
    echo "a rejected Secret write relayed kubectl's diagnostics or key material into setup output" >&2
    exit 1
fi

# A failure before the server answers has no reason code; setup says that
# kubectl's output is withheld and names the command that reproduces it.
if client_output="$(FAKE_WRITE_FAILS=client _create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup reported success although kubectl could not reach the server" >&2
    exit 1
fi
if ! grep -Fq 'could not replace Secret nico-system/nico-kmip-client.' <<< "${client_output}" || \
   ! grep -Fq "its output is withheld because a rejected object could carry the key. Run 'kubectl -n nico-system get secret nico-kmip-client'" <<< "${client_output}" || \
   grep -Fq 'i/o timeout' <<< "${client_output}"; then
    echo "a client-side kubectl failure was not reported with the reproduction hint only" >&2
    exit 1
fi

# A Secret that changes between the ownership check and the write is not
# overwritten: the replacement names the inspected version, and the server's
# conflict is reported with a hint to rerun.
printf -- '-----BEGIN CERTIFICATE-----\nca-bundle-v3\n' > "${client_dir}/ca.pem"
before_conflict="$(jq -c '.data' "${TEST_SECRET_STATE}")"
if changed_output="$(FAKE_CHANGE_AFTER_GET=true _create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup overwrote a Secret that changed after it was inspected" >&2
    exit 1
fi
if ! grep -Fq 'could not replace Secret nico-system/nico-kmip-client (kubectl reported Conflict)' <<< "${changed_output}" || \
   ! grep -Fq 'rerun setup.sh to re-check it' <<< "${changed_output}" || \
   [[ "$(jq -c '.data' "${TEST_SECRET_STATE}")" != "${before_conflict}" ]]; then
    echo "a Secret changed after inspection was not reported as a conflict and left as it was" >&2
    exit 1
fi

# An inspection failure is reported rather than treated as absence.
if inspect_output="$(FAKE_GET_FAILS=true _create_kmip_client_secret "${client_dir}" 2>&1)"; then
    echo "setup proceeded although it could not inspect the existing Secret" >&2
    exit 1
fi
if ! grep -Fq 'could not inspect Secret nico-system/nico-kmip-client' <<< "${inspect_output}"; then
    echo "a failed Secret inspection was not reported" >&2
    exit 1
fi

echo "setup-kmip-client-secret_test: OK"
