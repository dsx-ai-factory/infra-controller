#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image_tag_defaults="$(sed -n '/^NICO_DPF_IMAGE_TAG=/,/^fi/p' "${SCRIPT_DIR}/setup.sh")"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

version="$(python3 "${SCRIPT_DIR}/read-dpf-version.py")"
printf '%s\n' 'version = "30.0.0-rc.4"' > "${WORK}/prerelease.toml"
[[ "$(python3 "${SCRIPT_DIR}/read-dpf-version.py" "${WORK}/prerelease.toml")" == 30.0.0-rc.4 ]]
for invalid_manifest in 'version = 30' 'version = ""' 'version = [' \
    'version = "v30.0.0"' 'version = "30.0"' 'version = "30.0.0+build"'; do
    printf '%s\n' "${invalid_manifest}" > "${WORK}/invalid.toml"
    if python3 "${SCRIPT_DIR}/read-dpf-version.py" "${WORK}/invalid.toml" 2>/dev/null; then
        echo "an invalid TOML version must be rejected" >&2
        exit 1
    fi
done
INSTALL_DPF=true
NICO_DPF_IMAGE_TAG=""
eval "${image_tag_defaults}"
[[ "${NICO_DPF_IMAGE_TAG}" == "v${version}" ]]

# Explicit image tags and skipped DPF installations do not need a TOML parser.
python3() { return 1; }
NICO_DPF_IMAGE_TAG="custom-image"
eval "${image_tag_defaults}"
[[ "${NICO_DPF_IMAGE_TAG}" == custom-image ]]
INSTALL_DPF=false
NICO_DPF_IMAGE_TAG=""
eval "${image_tag_defaults}"
[[ -z "${NICO_DPF_IMAGE_TAG}" ]]
unset -f python3

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Defines the named functions of script $1 in this shell.
extract_functions() {
    local _script="$1" _fn
    shift
    for _fn in "$@"; do
        eval "$(sed -n "/^${_fn}()/,/^}/p" "${_script}")"
        declare -f "${_fn}" >/dev/null || fail "could not extract ${_fn} from ${_script##*/}"
    done
}

# ---------------------------------------------------------------------------
# preflight.sh: the operator image tag and the DPF upgrade source.
# ---------------------------------------------------------------------------
PREFLIGHT_SH="${SCRIPT_DIR}/preflight.sh"
eval "$(sed -n '/^_DPF_PINNED_LINE=/p; /^_DPF_UPGRADE_SOURCE_LINE=/p' "${PREFLIGHT_SH}")"
[[ -n "${_DPF_PINNED_LINE:-}" && -n "${_DPF_UPGRADE_SOURCE_LINE:-}" ]] \
    || fail "could not extract the DPF release lines from preflight.sh"
extract_functions "${PREFLIGHT_SH}" _resolve_dpf_target_tag _dpf_installed_version _check_dpf_upgrade_source

# A DPF bump moves the release lines in preflight.sh with dpf-versions.toml.
[[ "${version}" == "${_DPF_PINNED_LINE}."* ]] \
    || fail "dpf-versions.toml has ${version}, preflight.sh checks the ${_DPF_PINNED_LINE} release line (_DPF_PINNED_LINE)"

# name | NICO_DPF_IMAGE_TAG | TOML parser | _DPF_TARGET_TAG | error recorded
cases=(
    "default tag is the manifest version||works|v${version}|no"
    "explicit tag needs no parser|custom-image|fails|custom-image|no"
    "default tag without a parser||fails||yes"
)
for row in "${cases[@]}"; do
    IFS='|' read -r name tag parser want_tag want_error <<< "${row}"
    ERRORS=()
    NICO_DPF_IMAGE_TAG="${tag}"
    if [[ "${parser}" == fails ]]; then
        # shellcheck disable=SC2329 # Called by _resolve_dpf_target_tag.
        python3() { return 1; }
    fi
    _resolve_dpf_target_tag
    unset -f python3
    errors="${ERRORS[*]:-}"
    [[ "${_DPF_TARGET_TAG}" == "${want_tag}" ]] \
        || fail "${name}: target tag '${_DPF_TARGET_TAG}', want '${want_tag}'"
    if [[ "${want_error}" == no ]]; then
        [[ -z "${errors}" ]] || fail "${name}: unexpected error: ${errors}"
    elif [[ "${errors}" != *"TOML parser"*"NICO_DPF_IMAGE_TAG"* ]]; then
        fail "${name}: the error does not name the TOML parser and NICO_DPF_IMAGE_TAG: '${errors}'"
    fi
done

# Answers the two reads of _dpf_installed_version. Without the CRD the API
# server has no such resource, so a read of the object fails.
# shellcheck disable=SC2329 # Called by _dpf_installed_version.
kubectl() {
    case "${FAKE_CRD}:$1 $2" in
        "present:get crd") echo "customresourcedefinition.apiextensions.k8s.io/dpfoperatorconfigs.operator.dpu.nvidia.com" ;;
        "present:get dpfoperatorconfigs.operator.dpu.nvidia.com") printf '%s' "${FAKE_VERSION}" ;;
        "absent:get crd") ;;
        *) return 1 ;;
    esac
}
pinned_release="v${_DPF_PINNED_LINE}.0"
source_release="v${_DPF_UPGRADE_SOURCE_LINE}.0"
# name | operator image tag this run installs | DPF CRD (present, absent, or
# error) | status.version | finding
cases=(
    "fresh cluster without the DPF CRD|${pinned_release}|absent||none"
    "previous release line|${pinned_release}|present|${source_release}|none"
    "prerelease on the pinned line|${pinned_release}|present|${pinned_release}-rc.6|none"
    "older release line|${pinned_release}|present|v26.4.0|error"
    "tag on another release line is not checked|${source_release}|present|v26.4.0|none"
    "cluster read failure|${pinned_release}|error||warning"
)
for row in "${cases[@]}"; do
    IFS='|' read -r name tag FAKE_CRD FAKE_VERSION finding <<< "${row}"
    ERRORS=()
    WARNINGS=()
    _check_dpf_upgrade_source "${tag}"
    errors="${ERRORS[*]:-}"
    warnings="${WARNINGS[*]:-}"
    case "${finding}" in
        none)
            [[ -z "${errors}${warnings}" ]] \
                || fail "${name}: unexpected finding: ${errors}${warnings}"
            ;;
        error)
            [[ -z "${warnings}" && "${errors}" == *"reports DPF ${FAKE_VERSION} "*"upgrade DPF to v${_DPF_UPGRADE_SOURCE_LINE}.x first"*"docs/manuals/upgrade.md"* ]] \
                || fail "${name}: want one error that names ${FAKE_VERSION}, v${_DPF_UPGRADE_SOURCE_LINE}.x, and docs/manuals/upgrade.md, got errors '${errors}' and warnings '${warnings}'"
            ;;
        warning)
            [[ -z "${errors}" && "${warnings}" == *"Could not read DPFOperatorConfig"* ]] \
                || fail "${name}: want one warning about the unread DPFOperatorConfig, got errors '${errors}' and warnings '${warnings}'"
            ;;
    esac
done
unset -f kubectl

echo "DPF version selection tests passed"
