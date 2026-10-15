#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/helm.log"
export TEST_LOG

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
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${TEST_LOG}"
FAKE_HELM
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
cat >/dev/null
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

echo "setup Flow values test passed"
