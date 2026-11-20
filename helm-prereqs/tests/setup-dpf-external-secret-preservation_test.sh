#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
helper="$(sed -n '/^_preserve_external_dpf_secret()/,/^}/p' "${SETUP_SH}")"
if [[ -z "${helper}" ]]; then
    echo "could not extract external DPF Secret guard" >&2
    exit 1
fi
eval "${helper}"
for function in _apply_dpf_secret_manifest _label_dpf_argo_repo_secret; do
    helper="$(sed -n "/^${function}()/,/^}/p" "${SETUP_SH}")"
    if [[ -z "${helper}" ]]; then
        echo "could not extract ${function}" >&2
        exit 1
    fi
    eval "${helper}"
done

TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
case "${FAKE_SECRET_STATE:-missing}" in
    present) printf '%s\n' 'secret/external-dpf-pull-secret' ;;
    missing) : ;;
    forbidden) echo 'Error from server (Forbidden): secrets is forbidden' >&2; exit 1 ;;
esac
case "${1:-}" in
    label)
        printf 'label %s\n' "$*" >> "${FAKE_KUBECTL_LOG}"
        ;;
    create|apply)
        cat >/dev/null
        printf '%s\n' "$1" >> "${FAKE_KUBECTL_LOG}"
        ;;
esac
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"
export PATH="${TEST_TMP_DIR}/bin:${PATH}"
export FAKE_KUBECTL_LOG="${TEST_TMP_DIR}/kubectl.log"
export NICO_DPF_EXTERNAL=true

if FAKE_SECRET_STATE=present _preserve_external_dpf_secret external-dpf-pull-secret; then
    :
else
    echo "existing external Secret was not detected" >&2
    exit 1
fi

if FAKE_SECRET_STATE=missing _preserve_external_dpf_secret external-dpf-pull-secret; then
    echo "missing external Secret was treated as present" >&2
    exit 1
else
    status=$?
    if [[ "${status}" -ne 1 ]]; then
        echo "missing Secret returned ${status}, expected 1" >&2
        exit 1
    fi
fi

if FAKE_SECRET_STATE=forbidden _preserve_external_dpf_secret external-dpf-pull-secret; then
    echo "failed Secret lookup was treated as success" >&2
    exit 1
else
    status=$?
    if [[ "${status}" -ne 2 ]]; then
        echo "failed Secret lookup returned ${status}, expected 2" >&2
        exit 1
    fi
fi

_label_dpf_argo_repo_secret external-argo-repo
grep -Fq 'argocd.argoproj.io/secret-type=repository' "${FAKE_KUBECTL_LOG}"

printf '%s\n' manifest | _apply_dpf_secret_manifest
grep -Fxq create "${FAKE_KUBECTL_LOG}"
NICO_DPF_EXTERNAL=false
printf '%s\n' manifest | _apply_dpf_secret_manifest
grep -Fxq apply "${FAKE_KUBECTL_LOG}"

echo "external DPF Secret preservation tests passed"
