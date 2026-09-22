#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/kubectl.log"
export TEST_LOG

recreate_definition="$(
    sed -n '/^_recreate_rest_postgres_statefulset()/,/^}/p' "${SETUP_SH}"
)"
if [[ -z "${recreate_definition}" ]]; then
    echo "could not extract postgres StatefulSet recreation helper" >&2
    exit 1
fi
eval "${recreate_definition}"

mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_LOG}"

if [[ "$*" == *'--dry-run=server'* ]]; then
    case "${TEST_SCENARIO}" in
        template_differs)
            echo 'The StatefulSet "postgres" is invalid: spec: Forbidden: updates to statefulset spec for fields other than '"'"'replicas'"'"', '"'"'ordinals'"'"', '"'"'template'"'"', '"'"'updateStrategy'"'"', '"'"'revisionHistoryLimit'"'"', '"'"'persistentVolumeClaimRetentionPolicy'"'"' and '"'"'minReadySeconds'"'"' are forbidden' >&2
            exit 1
            ;;
        other_error)
            echo 'error: unable to build kustomization' >&2
            exit 1
            ;;
        template_same|absent) printf 'statefulset.apps/postgres configured (server dry run)\n' ;;
    esac
fi
FAKE_KUBECTL
chmod +x "${TEST_TMP_DIR}/bin/kubectl"

run_recreate() {
    TEST_SCENARIO="$1" PATH="${TEST_TMP_DIR}/bin:${PATH}" \
        _recreate_rest_postgres_statefulset
}

delete_cmd='delete statefulset postgres -n postgres --cascade=orphan'
dry_run_cmd='apply -k deploy/kustomize/base/postgres --dry-run=server'

: > "${TEST_LOG}"
output="$(run_recreate template_differs)"
if ! grep -Fqx -- "${dry_run_cmd}" "${TEST_LOG}"; then
    echo "helper did not dry-run the postgres manifest" >&2
    exit 1
fi
if ! grep -Fqx -- "${delete_cmd}" "${TEST_LOG}"; then
    echo "helper did not orphan-delete the StatefulSet when the template differs" >&2
    exit 1
fi
if grep -Fq -- 'delete pvc' "${TEST_LOG}" || grep -Fq -- 'delete persistentvolumeclaim' "${TEST_LOG}"; then
    echo "helper must never delete PVCs" >&2
    exit 1
fi
if ! grep -Fq -- 'pod and PVC are kept' <<< "${output}"; then
    echo "helper did not explain the recreation" >&2
    exit 1
fi

for unchanged in template_same absent other_error; do
    : > "${TEST_LOG}"
    run_recreate "${unchanged}"
    if grep -Fq -- 'delete' "${TEST_LOG}"; then
        echo "helper deleted the StatefulSet in scenario: ${unchanged}" >&2
        exit 1
    fi
done

apply_line="$(grep -nF '(cd "${NICO_REST_DIR}" && kubectl apply -k deploy/kustomize/base/postgres)' "${SETUP_SH}" | cut -d: -f1)"
recreate_line="$(grep -nF '(cd "${NICO_REST_DIR}" && _recreate_rest_postgres_statefulset)' "${SETUP_SH}" | cut -d: -f1)"
if [[ -z "${apply_line}" || -z "${recreate_line}" ]] || ! (( recreate_line < apply_line )); then
    echo "setup.sh must run the recreation helper immediately before the postgres apply" >&2
    exit 1
fi

echo "setup REST postgres StatefulSet recreation tests passed"
