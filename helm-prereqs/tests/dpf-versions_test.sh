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
# shellcheck disable=SC2329 # Called by the evaluated defaults.
python3() { return 1; }
NICO_DPF_IMAGE_TAG="custom-image"
eval "${image_tag_defaults}"
[[ "${NICO_DPF_IMAGE_TAG}" == custom-image ]]
# shellcheck disable=SC2034 # Read by the evaluated defaults.
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

# A DPF bump moves _DPF_PINNED_LINE in preflight.sh with dpf-versions.toml.
# Nothing here checks _DPF_UPGRADE_SOURCE_LINE.
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
        "present:get crd" | "no-object:get crd") echo "customresourcedefinition.apiextensions.k8s.io/dpfoperatorconfigs.operator.dpu.nvidia.com" ;;
        "present:get dpfoperatorconfigs.operator.dpu.nvidia.com") printf 'dpfoperatorconfig:%s' "${FAKE_VERSION}" ;;
        "no-object:get dpfoperatorconfigs.operator.dpu.nvidia.com") ;;
        "absent:get crd") ;;
        *) return 1 ;;
    esac
}
pinned_release="v${_DPF_PINNED_LINE}.0"
source_release="v${_DPF_UPGRADE_SOURCE_LINE}.0"
# name | operator image tag this run installs | DPF CRD (present, no-object,
# absent, or error) | status.version | finding (none, error, or a warning
# substring)
cases=(
    "fresh cluster without the DPF CRD|${pinned_release}|absent||none"
    "DPF CRD without a DPFOperatorConfig|${pinned_release}|no-object||none"
    "previous release line|${pinned_release}|present|${source_release}|none"
    "prerelease on the pinned line|${pinned_release}|present|${pinned_release}-rc.6|a prerelease of ${pinned_release}"
    "empty status.version|${pinned_release}|present||has no status.version"
    "older release line|${pinned_release}|present|v26.4.0|error"
    "tag on another release line is not checked|${source_release}|present|v26.4.0|none"
    "cluster read failure|${pinned_release}|error||Could not read DPFOperatorConfig"
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
        *)
            [[ -z "${errors}" && ${#WARNINGS[@]} -eq 1 && "${warnings}" == *"${finding}"* ]] \
                || fail "${name}: want one warning containing '${finding}', got errors '${errors}' and warnings '${warnings}'"
            ;;
    esac
done
unset -f kubectl

# ---------------------------------------------------------------------------
# setup.sh: the wait for the operator version before Core.
# ---------------------------------------------------------------------------
extract_functions "${SCRIPT_DIR}/setup.sh" _dpf_release_key _dpf_prerelease _dpf_version_at_floor _wait_for_dpf_operator_version

precheck_message="stub pre-upgrade validation message"
# Answers the reads of _wait_for_dpf_operator_version: the status pair of each
# read in turn (the last one repeats), then the conditions for the failure report.
# shellcheck disable=SC2329 # Called by _wait_for_dpf_operator_version.
kubectl() {
    local _pairs _read
    case "$*" in
        *'{.status.version}|{.status.targetVersion}'*)
            echo read >> "${WORK}/reads"
            read -r -a _pairs <<< "${FAKE_PAIRS}"
            _read=$(( $(wc -l < "${WORK}/reads") ))
            (( _read <= ${#_pairs[@]} )) || _read=${#_pairs[@]}
            printf '%s' "${_pairs[_read - 1]}"
            ;;
        *'.status.conditions[?(@.status!="True")]'*) printf 'PreUpgradeValidationReady: False/Failed\n%s\n' "${precheck_message}" ;;
        *) return 1 ;;
    esac
}
# A clock that only sleep advances, so a wait that never returns ends at once.
# shellcheck disable=SC2329 # Called by _wait_for_dpf_operator_version.
sleep() { echo $(( $(cat "${WORK}/clock") + $1 )) > "${WORK}/clock"; }
# shellcheck disable=SC2329 # Called by _wait_for_dpf_operator_version.
date() { cat "${WORK}/clock"; }
# name ; operator image tag ; status.version|status.targetVersion per read (the
# last repeats) ; reads until the wait returns (0 means it times out) ; text of
# the failure report ; text that the failure report must not have
cases=(
    "upgrade finishes after the operator records its target;v30.0.0;v26.8.0|v26.8.0 v26.8.0|v30.0.0 v30.0.0|v30.0.0;3;"
    "tag without a release waits for the recorded target;custom-image;v26.8.0|v30.0.0 v30.0.0|v30.0.0;2;"
    "operator does not finish;v30.0.0;v26.8.0|v30.0.0;0;${precheck_message}"
    "image tag ahead of the build TAG;v30.1.0;v30.0.0|v30.0.0;0;built with TAG=v30.0.0"
    "prerelease of the release in the tag does not pass;v30.0.0;v30.0.0-rc.4|v30.0.0-rc.4;0;Use an operator image built with a plain release TAG, such as TAG=v30.0.0,;Resolve what is reported above"
    "prerelease tag accepts the same prerelease;v30.0.0-rc.4;v30.0.0-rc.4|v30.0.0-rc.4;1;"
    "prerelease above the release in the tag passes;v30.0.0;v30.0.1-rc.1|v30.0.1-rc.1;1;"
    "release with a leading zero and a digit above 7;v0.1.8;v0.1.8|v0.1.8;1;"
)
for row in "${cases[@]}"; do
    IFS=';' read -r name tag FAKE_PAIRS want_reads want_report want_absent <<< "${row}"
    : > "${WORK}/reads"
    echo 0 > "${WORK}/clock"
    wait_seconds=600
    [[ "${want_reads}" != 0 ]] || wait_seconds=0
    status=0
    output="$(_wait_for_dpf_operator_version "${wait_seconds}" "${tag}" 2>&1)" || status=$?
    reads=$(( $(wc -l < "${WORK}/reads") ))
    if [[ "${want_reads}" == 0 ]]; then
        last_pair="${FAKE_PAIRS##* }"
        [[ "${status}" -ne 0 && "${output}" == *"status.version is '${last_pair%|*}', status.targetVersion is '${last_pair#*|}'"*"${want_report}"* ]] \
            || fail "${name}: want a failure that prints both status values and '${want_report}', got exit ${status}: ${output}"
        [[ -z "${want_absent}" || "${output}" != *"${want_absent}"* ]] \
            || fail "${name}: the failure report must not print '${want_absent}': ${output}"
    else
        [[ "${status}" -eq 0 && "${reads}" -eq "${want_reads}" ]] \
            || fail "${name}: want success after ${want_reads} reads, got exit ${status} after ${reads}: ${output}"
    fi
done
unset -f kubectl sleep date

# ---------------------------------------------------------------------------
# Call sites: preflight resolves the tag before it checks the upgrade source,
# and setup.sh waits for the operator version between the DPFOperatorConfig
# apply and Core.
# ---------------------------------------------------------------------------
# Prints the number of the only line of file $2 that contains $1, or that
# equals it with $3 set to x. Prints nothing without exactly one such line.
line_of() {
    local _lines
    _lines="$(grep -n"${3:-}"F -- "$1" "$2" | cut -d: -f1 || true)"
    [[ "${_lines}" != *$'\n'* ]] || return 0
    echo "${_lines}"
}
resolve_line="$(line_of '    _resolve_dpf_target_tag' "${PREFLIGHT_SH}" x)"
# shellcheck disable=SC2016 # The line is matched as written in preflight.sh.
check_line="$(line_of '        _check_dpf_upgrade_source "${_DPF_TARGET_TAG:-}"' "${PREFLIGHT_SH}" x)"
if [[ -z "${resolve_line}" || -z "${check_line}" ]] || (( resolve_line >= check_line )); then
    fail "preflight.sh must resolve the DPF image tag before its one DPF upgrade source check"
fi
apply_line="$(line_of '< operators/dpf/dpfoperatorconfig.yaml.tmpl | kubectl apply -f -' "${SCRIPT_DIR}/setup.sh")"
# shellcheck disable=SC2016 # The line is matched as written in setup.sh.
wait_line="$(line_of '    _wait_for_dpf_operator_version 600 "${NICO_DPF_IMAGE_TAG}"' "${SCRIPT_DIR}/setup.sh" x)"
core_line="$(line_of 'DPF stack installed (Core will start' "${SCRIPT_DIR}/setup.sh")"
if [[ -z "${apply_line}" || -z "${wait_line}" || -z "${core_line}" ]] \
    || (( apply_line >= wait_line || wait_line >= core_line )); then
    fail "setup.sh must wait 600 seconds for the operator version after the DPFOperatorConfig apply and before Core"
fi

echo "DPF version selection tests passed"
