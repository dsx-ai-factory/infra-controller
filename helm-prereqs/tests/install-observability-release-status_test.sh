#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/../observability/install-observability.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/helm.log"

inspect_definition="$(
    sed -n '/^_inspect_nico_release()/,/^}/p' "${INSTALLER}"
)"
if [[ -z "${inspect_definition}" ]]; then
    echo "could not extract NICo release inspection helper" >&2
    exit 1
fi
eval "${inspect_definition}"

helm() {
    printf '%s\n' "$*" >> "${TEST_LOG}"
    case "${TEST_SCENARIO}" in
        present)
            printf 'NAME: nico\nSTATUS: deployed\n'
            ;;
        missing)
            printf 'Error: release: not found\n' >&2
            return 1
            ;;
        forbidden)
            printf 'Error: query: failed to query with labels: secrets is forbidden\n' >&2
            return 7
            ;;
        unreachable)
            printf 'Error: Kubernetes cluster unreachable: connection refused\n' >&2
            return 9
            ;;
    esac
}

NICO_RELEASE=nico
NICO_NS=nico-system

_NICO_RELEASE_PRESENT=false
TEST_SCENARIO=present _inspect_nico_release
[[ "${_NICO_RELEASE_PRESENT}" == "true" ]] || {
    echo "deployed release was not marked present" >&2
    exit 1
}

_NICO_RELEASE_PRESENT=true
TEST_SCENARIO=missing _inspect_nico_release
[[ "${_NICO_RELEASE_PRESENT}" == "false" ]] || {
    echo "missing release was not classified as absent" >&2
    exit 1
}

assert_status_failure() {
    local scenario="$1"
    local expected_status="$2"
    local expected_error="$3"
    local error_log="${TEST_TMP_DIR}/${scenario}.err"
    local actual_status

    if TEST_SCENARIO="${scenario}" _inspect_nico_release 2> "${error_log}"; then
        echo "${scenario} helm status failure was accepted" >&2
        exit 1
    else
        actual_status=$?
    fi

    [[ "${actual_status}" == "${expected_status}" ]] || {
        echo "${scenario} status ${actual_status} did not preserve ${expected_status}" >&2
        exit 1
    }
    grep -Fq -- "${expected_error}" "${error_log}" || {
        echo "${scenario} error output was not retained" >&2
        exit 1
    }
}

assert_status_failure forbidden 7 'secrets is forbidden'
assert_status_failure unreachable 9 'Kubernetes cluster unreachable'

if [[ "$(grep -Fc -- 'status nico -n nico-system' "${TEST_LOG}")" -ne 4 ]]; then
    echo "release inspection did not call helm status for every scenario" >&2
    exit 1
fi

echo "install-observability release status tests passed"
