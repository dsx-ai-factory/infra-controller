#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEST_BASH="${BASH:-bash}"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
failures=0

sed -n '/^# Output and prompts/,$p' "${PREFLIGHT_SH}" > "${TEST_TMP_DIR}/output.sh"

# Run the real output section with prepared check results. Its sourced path
# lets this test exercise the return status setup.sh receives without a cluster.
run_summary() {
    local sourced="$1" auto_yes="$2" ignore_errors="$3" error="$4" warning="$5" blocking="$6"
    (
        _SOURCED="${sourced}"
        AUTO_YES="${auto_yes}"
        IGNORE_PREFLIGHT_ERRORS="${ignore_errors}"
        _NICO_REST_ENABLED=false
        ERRORS=()
        WARNINGS=()
        BLOCKING_ERRORS=()
        [[ -n "${error}" ]] && ERRORS+=("${error}")
        [[ -n "${warning}" ]] && WARNINGS+=("${warning}")
        [[ -n "${blocking}" ]] && BLOCKING_ERRORS+=("${blocking}")
        # shellcheck disable=SC1091 # Generated from preflight.sh above.
        source "${TEST_TMP_DIR}/output.sh"
    )
}

check_summary() {
    local name="$1" expected_status="$2" expected_text="$3"
    shift 3
    local status=0 output
    output="$(run_summary "$@" 2>&1 <<< "${REPLY:-}")" || status=$?
    if [[ "${status}" != "${expected_status}" ]]; then
        echo "FAIL ${name}: status ${status}, expected ${expected_status}; output: ${output}" >&2
        failures=$((failures + 1))
    fi
    if [[ -n "${expected_text}" && "${output}" != *"${expected_text}"* ]]; then
        echo "FAIL ${name}: output missing '${expected_text}'; output: ${output}" >&2
        failures=$((failures + 1))
    fi
}

# Distinct result/precedence boundaries. Warnings remain promptable and -y
# handles their prompt; ordinary errors need the explicit override; blocking
# database errors retain precedence over that override.
check_summary "clean checks" 0 "Pre-flight OK" true false false "" "" ""
check_summary "warning with -y" 0 "Warnings noted — continuing" true true false "" "warning detail" ""
REPLY=n check_summary "warning declined" 1 "Aborted" true false false "" "warning detail" ""
check_summary "ordinary error default" 1 "pass --ignore-preflight-errors" true false false "ordinary detail" "" ""
check_summary "ordinary error with -y" 1 "Fix the errors above" true true false "ordinary detail" "" ""
check_summary "ordinary error override" 0 "Errors above ignored — continuing" true false true "ordinary detail" "" ""
check_summary "ordinary error override with -y" 0 "Errors above ignored — continuing" true true true "ordinary detail" "" ""
check_summary "blocking error with override" 1 "even with --ignore-preflight-errors" true true true "ordinary detail" "" "database detail"

# Direct execution exits on an ordinary error, while sourced execution returns
# failure to its caller (the caller checks that setup's pre-install path stops).
status=0
output="$("${TEST_BASH}" -c '
    _SOURCED=false AUTO_YES=true IGNORE_PREFLIGHT_ERRORS=false
    _NICO_REST_ENABLED=false
    ERRORS=("ordinary detail") WARNINGS=() BLOCKING_ERRORS=()
    source "$1"
' _ "${TEST_TMP_DIR}/output.sh" 2>&1)" || status=$?
if [[ "${status}" -ne 1 || "${output}" != *"Fix the errors above"* ]]; then
    echo "FAIL direct ordinary error: status ${status}, expected 1; output: ${output}" >&2
    failures=$((failures + 1))
fi

# Execute the standalone parser. An inherited variable alone must not enable
# the override; the explicit flag must enable it.
standalone_parser="$(awk '
    /^if ! \$\{_SOURCED\}; then$/ { printing = 1 }
    printing { print }
    printing && /^fi$/ { exit }
' "${PREFLIGHT_SH}")"
for row in "inherited-variable|false|" "explicit-flag|true|--ignore-preflight-errors"; do
    IFS='|' read -r name expected_value flag <<< "${row}"
    standalone_result="$("${TEST_BASH}" -c '
        IGNORE_PREFLIGHT_ERRORS=true
        _SOURCED=false
        INSTALL_DPF=true INSTALL_RMS=true SKIP_CORE=false SKIP_REST=false AUTO_YES=false
        standalone_parser="$1"
        shift
        eval "$standalone_parser"
        printf "%s" "$IGNORE_PREFLIGHT_ERRORS"
    ' _ "${standalone_parser}" ${flag:+"${flag}"})"
    if [[ "${standalone_result}" != "${expected_value}" ]]; then
        echo "FAIL standalone ${name}: override ${standalone_result}, expected ${expected_value}" >&2
        failures=$((failures + 1))
    fi
done

# A sourced preflight returns failure to its caller, while direct execution
# exits the process. Prove the caller can inspect the returned status and run
# subsequent code.
status=0
output="$("${TEST_BASH}" -c '
    _SOURCED=true AUTO_YES=false IGNORE_PREFLIGHT_ERRORS=false
    _NICO_REST_ENABLED=false
    ERRORS=("ordinary detail") WARNINGS=() BLOCKING_ERRORS=()
    source "$1"
    source_status=$?
    printf "source-status=%s caller-survived" "$source_status"
' _ "${TEST_TMP_DIR}/output.sh" 2>&1)" || status=$?
if [[ "${status}" -ne 0 || "${output}" != *"source-status=1 caller-survived"* ]]; then
    echo "FAIL sourced preflight did not return failure to a live caller: status ${status}; output: ${output}" >&2
    failures=$((failures + 1))
fi

# Run the actual setup prefix through its preflight source line in a temporary
# script directory. The stub preflight supplies the real output section with
# one ordinary error; kubectl is stubbed because the prefix checks for the
# retired Flow manager workload before sourcing preflight.
mkdir -p "${TEST_TMP_DIR}/setup/bin"
awk '
    { print }
    /source "\$\{SCRIPT_DIR\}\/preflight.sh"/ { exit }
' "${SETUP_SH}" > "${TEST_TMP_DIR}/setup/setup-prefix.sh"
cat >> "${TEST_TMP_DIR}/setup/setup-prefix.sh" <<'CALLER_MARKER'
printf 'install-reached\n' > "${TEST_INSTALL_MARKER}"
CALLER_MARKER
cat > "${TEST_TMP_DIR}/setup/preflight.sh" <<'PREFLIGHT_STUB'
_SOURCED=true
ERRORS=("ordinary detail")
WARNINGS=()
BLOCKING_ERRORS=()
_NICO_REST_ENABLED=false
source "${TEST_SUMMARY_FILE}"
PREFLIGHT_STUB
cat > "${TEST_TMP_DIR}/setup/bin/kubectl" <<'KUBECTL_STUB'
#!/usr/bin/env bash
if [[ "${1:-}" != "get" ]]; then
    echo "unexpected kubectl operation: $*" >&2
    exit 42
fi
exit 0
KUBECTL_STUB
chmod +x "${TEST_TMP_DIR}/setup/bin/kubectl"
export TEST_SUMMARY_FILE="${TEST_TMP_DIR}/output.sh"
export TEST_INSTALL_MARKER="${TEST_TMP_DIR}/setup/install-reached"

for row in \
    "ordinary-error|1|false|-y" \
    "explicit-override|0|true|-y --ignore-preflight-errors"; do
    IFS='|' read -r name expected_status should_reach_install args <<< "${row}"
    status=0
    # shellcheck disable=SC2086 # args is a test row containing separate CLI flags.
    env -i PATH="${TEST_TMP_DIR}/setup/bin:/usr/bin:/bin" \
        TEST_SUMMARY_FILE="${TEST_SUMMARY_FILE}" \
        TEST_INSTALL_MARKER="${TEST_INSTALL_MARKER}" \
        "${TEST_BASH}" "${TEST_TMP_DIR}/setup/setup-prefix.sh" ${args} \
        --skip-core --skip-rest --skip-dpf --skip-rms \
        >"${TEST_TMP_DIR}/setup/${name}.out" 2>&1 || status=$?
    if [[ "${status}" != "${expected_status}" ]]; then
        echo "FAIL setup ${name}: status ${status}, expected ${expected_status}" >&2
        failures=$((failures + 1))
    fi
    if [[ "${should_reach_install}" == "true" && ! -e "${TEST_INSTALL_MARKER}" ]]; then
        echo "FAIL setup ${name}: override did not pass the preflight source gate" >&2
        failures=$((failures + 1))
    elif [[ "${should_reach_install}" == "false" && -e "${TEST_INSTALL_MARKER}" ]]; then
        echo "FAIL setup ${name}: setup reached installation after preflight failure" >&2
        failures=$((failures + 1))
    fi
    [[ -e "${TEST_INSTALL_MARKER}" ]] && rm -f "${TEST_INSTALL_MARKER}"
done

# NICO_CORE_IMAGE_NAME: the default and a legacy override name the Core image
# preflight checks, and a full image path is an error. Runs the real default,
# validation, and image-check code with the registry probe stubbed.
core_name_default="$(grep -F 'NICO_CORE_IMAGE_NAME="${NICO_CORE_IMAGE_NAME:-' "${PREFLIGHT_SH}" || true)"
core_name_validation="$(sed -n '/^# NICO_CORE_IMAGE_NAME is one repository path component/,/^fi$/p' "${PREFLIGHT_SH}")"
core_image_check="$(awk '
    /^_validate_nico_core_image_access\(\) \{$/ { printing = 1 }
    printing { print }
    printing && /^}$/ { exit }
' "${PREFLIGHT_SH}")"
for row in \
    "default||registry.example.com/nico/nico:core-tag|0" \
    "legacy-override|nvmetal-carbide|registry.example.com/nico/nvmetal-carbide:core-tag|0" \
    "full-image-path|registry.example.com/nico:v1|registry.example.com/nico/registry.example.com/nico:v1:core-tag|1"; do
    IFS='|' read -r name image_name expected_ref expected_errors <<< "${row}"
    core_image_result="$(env -u NICO_CORE_IMAGE_NAME ${image_name:+NICO_CORE_IMAGE_NAME="${image_name}"} "${TEST_BASH}" -c '
        set -eu
        ERRORS=() SKIP_CORE=false _CORE_VALUES_CFG=/nonexistent
        NICO_IMAGE_REGISTRY=registry.example.com/nico NICO_CORE_IMAGE_TAG=core-tag
        _validate_image_manifest_access() { printf "%s" "$1"; }
        eval "$1"; eval "$2"; eval "$3"
        _validate_nico_core_image_access
        printf "|%s" "${#ERRORS[@]}"
    ' _ "${core_name_default}" "${core_name_validation}" "${core_image_check}" 2>&1)" || true
    if [[ "${core_image_result}" != "${expected_ref}|${expected_errors}" ]]; then
        echo "FAIL Core image name ${name}: got '${core_image_result}', expected '${expected_ref}|${expected_errors}'" >&2
        failures=$((failures + 1))
    fi
done

if (( failures > 0 )); then
    exit 1
fi
echo "preflight error handling tests passed"
