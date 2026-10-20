#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
export TEST_TMP_DIR

# Exercise the actual recovery block and EXIT cleanup without installing anything.
sed -n '/^_on_failure()/,/^}/p' "${SCRIPT_DIR}/../setup.sh" > "${TEST_TMP_DIR}/cleanup.sh"
sed -n '/^    if ! helmfile sync -l name=kamaji; then/,/^    fi/p' \
    "${SCRIPT_DIR}/../setup.sh" > "${TEST_TMP_DIR}/recovery.sh"
[[ -s "${TEST_TMP_DIR}/cleanup.sh" && -s "${TEST_TMP_DIR}/recovery.sh" ]]

helmfile() {
    if [[ ! -e "${TEST_TMP_DIR}/first-sync" ]]; then
        touch "${TEST_TMP_DIR}/first-sync"
        return 1
    fi
}

helm() {
    local output_dir="" path kind
    while (( $# )); do
        if [[ "$1" == --output-dir ]]; then
            output_dir="$2"
            shift
        fi
        shift
    done
    # Helm 4 OCI status messages share stdout with rendered manifests.
    printf 'Pulled: ghcr.io/nvidia/charts/kamaji:1.2.0\nDigest: sha256:test\n'
    for path in \
        kamaji/templates/certmanager_issuer.yaml \
        kamaji/templates/certmanager_certificate.yaml \
        kamaji/charts/kamaji-etcd/templates/etcd_datastore.yaml \
        kamaji/templates/controller.yaml; do
        case "${path}" in
            *issuer.yaml) kind=Issuer ;;
            *certificate.yaml) kind=Certificate ;;
            *datastore.yaml) kind=DataStore ;;
            *) kind=Deployment ;;
        esac
        if [[ -n "${output_dir}" ]]; then
            mkdir -p "${output_dir}/$(dirname "${path}")"
            printf 'apiVersion: test/v1\nkind: %s\n' "${kind}" > "${output_dir}/${path}"
        else
            printf -- '---\napiVersion: test/v1\nkind: %s\n' "${kind}"
        fi
    done
    [[ "${TEST_SCENARIO}" != render_failure ]] || return 42
}

kubectl() {
    local file
    case "$1" in
        get) printf '{"webhooks":[{"name":"vdatastore.kb.io"}]}\n' ;;
        patch) printf '%s\n' "$*" >> "${TEST_TMP_DIR}/patches" ;;
        rollout) touch "${TEST_TMP_DIR}/rollout" ;;
        apply)
            touch "${TEST_TMP_DIR}/apply"
            [[ "${TEST_SCENARIO}" != apply_failure ]] || return 43
            shift
            while (( $# )); do
                [[ "$1" == -f ]] || return 1
                file="$2"
                if [[ "${file}" == - ]]; then
                    file="${TEST_TMP_DIR}/stdin.yaml"
                    cat > "${file}"
                fi
                if grep -q '^Pulled:' "${file}"; then
                    echo 'apiVersion not set, kind not set' >&2
                    return 1
                fi
                sed -n 's/^kind: //p' "${file}" >> "${TEST_TMP_DIR}/applied"
                shift 2
            done
            ;;
        *) echo "unexpected kubectl command: $*" >&2; return 1 ;;
    esac
}
export -f helmfile helm kubectl

for TEST_SCENARIO in success render_failure apply_failure; do
    export TEST_SCENARIO
    rm -f "${TEST_TMP_DIR}"/{first-sync,patches,rollout,apply,applied}
    mkdir -p "${TEST_TMP_DIR}/render"
    status=0
    TMPDIR="${TEST_TMP_DIR}/render" bash -eu -o pipefail <<'RUN' > "${TEST_TMP_DIR}/output" 2>&1 || status=$?
source "${TEST_TMP_DIR}/cleanup.sh"
# Avoid the interactive cleanup prompt; all kubectl operations are test doubles.
_SETUP_PHASE=complete
_LEGACY_DPF_BOOTSTRAP_CLEANED=true
_KAMAJI_WH_RELAXED=false
trap _on_failure EXIT
source "${TEST_TMP_DIR}/recovery.sh"
RUN
    case "${TEST_SCENARIO}" in
        success)
            if [[ "${status}" != 0 ]]; then
                cat "${TEST_TMP_DIR}/output" >&2
                echo "Kamaji bootstrap failed with OCI status output" >&2
                exit 1
            fi
            diff -u <(printf '%s\n' Issuer Certificate DataStore | sort) \
                <(sort "${TEST_TMP_DIR}/applied")
            [[ -e "${TEST_TMP_DIR}/rollout" ]]
            ;;
        render_failure)
            [[ "${status}" == 42 && ! -e "${TEST_TMP_DIR}/apply" ]]
            ;;
        apply_failure)
            [[ "${status}" == 43 && ! -e "${TEST_TMP_DIR}/rollout" ]]
            ;;
    esac
    # Recovery must restore admission validation and remove rendered files on exit.
    tail -1 "${TEST_TMP_DIR}/patches" | grep -q '"value":"Fail"'
    [[ -z "$(find "${TEST_TMP_DIR}/render" -mindepth 1 -print -quit)" ]]
done

echo "setup Kamaji bootstrap tests passed"
