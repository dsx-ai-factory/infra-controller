#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Unit-style checks for the helpers setup-machine-a-tron.sh uses to point
# nico-core at machine-a-tron: resolve_bmc_mock_svc and resolve_bmc_mock_port
# (which bmc-mock Service and port bmc_proxy and --rms-sim name) and
# patch_rms_endpoint (the managed [rms] block in the nico-api site config).
# They are extracted from the script and run against a fake helm and fixture
# ConfigMaps; no cluster is needed. A last check pins the phase order: with
# --rms-sim the block is written only after the deploy has been verified.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup-machine-a-tron.sh"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT
TEST_LOG="${TEST_TMP_DIR}/helm.log"
export TEST_LOG

fail() { echo "FAIL: $*" >&2; exit 1; }

# The script must parse. bash reads scripts in 4 KiB chunks and has misparsed
# a construct that landed on a chunk boundary before, so any edit is worth
# this check. macOS /bin/bash 3.2 cannot parse the script's larger
# here-documents inside $(...) at all, so only bash 4+ is checked.
if (( BASH_VERSINFO[0] >= 4 )); then
    "${BASH}" -n "${SETUP_SH}" || fail "setup-machine-a-tron.sh does not parse under bash ${BASH_VERSION}"
fi

extract() {
    local body
    body="$(sed -n "/^$1()/,/^}/p" "${SETUP_SH}")"
    [[ -n "${body}" ]] || fail "could not extract $1 from setup-machine-a-tron.sh"
    eval "${body}"
}
extract render_bmc_mock_services
extract resolve_bmc_mock_svc
extract resolve_bmc_mock_port
extract patch_rms_endpoint

# --- resolve_bmc_mock_svc -----------------------------------------------------
# A helm that renders the Services named in FAKE_HELM_SERVICES, each with the
# chart's redfish port entry (FAKE_HELM_PORT, default 1266), and records the
# values files it was given, so the test can see the pods.default stub.
mkdir -p "${TEST_TMP_DIR}/bin"
cat > "${TEST_TMP_DIR}/bin/helm" <<'FAKE_HELM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${TEST_LOG}"
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    if [[ "${args[i]}" == "-f" ]]; then
        echo "--- values ${args[i+1]}" >> "${TEST_LOG}"
        cat "${args[i+1]}" >> "${TEST_LOG}"
    fi
done
if [[ "${FAKE_HELM_FAIL:-0}" == "1" ]]; then
    echo "Error: execution error at (nico-machine-a-tron/templates/deployment.yaml:7:4): multi-pod machine-a-tron deployments require mat-k8s-controller.enabled=true" >&2
    exit 1
fi
for svc in ${FAKE_HELM_SERVICES:-}; do
    printf -- '---\napiVersion: v1\nkind: Service\nmetadata:\n  name: %s\n  namespace: nico-mat\nspec:\n  ports:\n    - port: %s\n      name: redfish\n      targetPort: redfish\n      protocol: TCP\n' "${svc}" "${FAKE_HELM_PORT:-1266}"
done
FAKE_HELM
chmod +x "${TEST_TMP_DIR}/bin/helm"
export PATH="${TEST_TMP_DIR}/bin:${PATH}"

RELEASE="nico-machine-a-tron"
CHART_DIR="${TEST_TMP_DIR}/chart"
MAT_NAMESPACE="nico-mat"
VALUES_FILE="${TEST_TMP_DIR}/values.yaml"
BMC_MOCK_SVC_OVERRIDE=""
BMC_MOCK_PORT_DEFAULT="1266"
mkdir -p "${CHART_DIR}"
echo 'pods: {}' > "${VALUES_FILE}"
unset MAT_MULTIPOD

# Without MAT_MULTIPOD the fleet is injected into pods.default, so that pod's
# Service is chosen even when the values file renders another pod too.
: > "${TEST_LOG}"
got="$(FAKE_HELM_SERVICES="nico-machine-a-tron-mat-0-bmc-mock nico-machine-a-tron-default-bmc-mock" resolve_bmc_mock_svc)"
[[ "${got}" == "nico-machine-a-tron-default-bmc-mock" ]] || fail "default pod expected, got '${got}'"
grep -q 'hostCount: 1' "${TEST_LOG}" || fail "the render did not include the pods.default stub"
grep -q -- '-s templates/service.yaml' "${TEST_LOG}" || fail "the render did not select templates/service.yaml"

# The name comes from the render, so a chart nameOverride is honoured.
got="$(FAKE_HELM_SERVICES="mat-x-default-bmc-mock" resolve_bmc_mock_svc)"
[[ "${got}" == "mat-x-default-bmc-mock" ]] || fail "nameOverride not honoured, got '${got}'"

# No default pod rendered where one is injected: nothing qualifies.
got="$(FAKE_HELM_SERVICES="nico-machine-a-tron-mat-0-bmc-mock" resolve_bmc_mock_svc)"
[[ -z "${got}" ]] || fail "expected no Service without a default pod, got '${got}'"

# MAT_MULTIPOD=1: the values file owns the pods map, the first pod in sorted
# order is the endpoint, and no pods.default is injected.
: > "${TEST_LOG}"
got="$(MAT_MULTIPOD=1 FAKE_HELM_SERVICES="nico-machine-a-tron-mat-2-bmc-mock nico-machine-a-tron-mat-0-bmc-mock nico-machine-a-tron-mat-1-bmc-mock" resolve_bmc_mock_svc)"
[[ "${got}" == "nico-machine-a-tron-mat-0-bmc-mock" ]] || fail "multipod: mat-0 expected, got '${got}'"
if grep -q 'hostCount: 1' "${TEST_LOG}"; then fail "multipod must not inject pods.default"; fi

# BMC_MOCK_SVC wins without rendering anything.
: > "${TEST_LOG}"
got="$(BMC_MOCK_SVC_OVERRIDE="custom-bmc-mock" resolve_bmc_mock_svc)"
[[ "${got}" == "custom-bmc-mock" ]] || fail "override not honoured, got '${got}'"
[[ ! -s "${TEST_LOG}" ]] || fail "override must not render the chart"

# A chart the values file cannot render fails with helm's own message.
if out="$(FAKE_HELM_FAIL=1 resolve_bmc_mock_svc 2>&1)"; then
    fail "a failed render must fail the resolution"
fi
[[ "${out}" == *"mat-k8s-controller.enabled"* ]] || fail "helm's error must be reported, got '${out}'"

echo "resolve_bmc_mock_svc: ok"

# --- resolve_bmc_mock_port ----------------------------------------------------
# The port is whatever the chart renders for the redfish port, so a values
# file that overrides service.bmcMock.port is honoured.
got="$(FAKE_HELM_SERVICES="nico-machine-a-tron-default-bmc-mock" resolve_bmc_mock_port)"
[[ "${got}" == "1266" ]] || fail "port: chart default expected, got '${got}'"
got="$(FAKE_HELM_SERVICES="nico-machine-a-tron-default-bmc-mock" FAKE_HELM_PORT=8443 resolve_bmc_mock_port)"
[[ "${got}" == "8443" ]] || fail "port: values override not honoured, got '${got}'"

# A render with no Service in it falls back to the chart default rather than
# leaving the URL without a port.
got="$(FAKE_HELM_SERVICES="" resolve_bmc_mock_port)"
[[ "${got}" == "1266" ]] || fail "port: fallback expected, got '${got}'"

# The port is a chart value, not a pod choice, so BMC_MOCK_SVC does not
# short-circuit it: the values file still decides.
got="$(BMC_MOCK_SVC_OVERRIDE="custom-bmc-mock" FAKE_HELM_SERVICES="x-default-bmc-mock" FAKE_HELM_PORT=9443 resolve_bmc_mock_port)"
[[ "${got}" == "9443" ]] || fail "port: override must not bypass the render, got '${got}'"

# A chart the values file cannot render fails here too.
if out="$(FAKE_HELM_FAIL=1 resolve_bmc_mock_port 2>&1)"; then
    fail "port: a failed render must fail the resolution"
fi
[[ "${out}" == *"mat-k8s-controller.enabled"* ]] || fail "port: helm's error must be reported, got '${out}'"

echo "resolve_bmc_mock_port: ok"

# --- patch_rms_endpoint -------------------------------------------------------
URL="https://nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local:1266"
CM="${TEST_TMP_DIR}/cm.json"
BEGIN="# --- BEGIN RMS simulator endpoint (managed by setup-machine-a-tron.sh --rms-sim) ---"
END="# --- END RMS simulator endpoint ---"
BASE=$'[site_explorer]\nconcurrent_explorations = 10\n\n[firmware_global]\nconcurrency_limit = 5\n'

cm_with() {
    jq -n --arg toml "$1" '{apiVersion: "v1", kind: "ConfigMap",
        metadata: {name: "nico-api-site-config-files", resourceVersion: "7", uid: "u-1"},
        data: {"nico-api-site-config.toml": $toml, "unrelated.txt": "keep"}}' > "${CM}"
}
toml_of() { jq -r '.data["nico-api-site-config.toml"]' "${CM}"; }
count_rms_tables() { toml_of | grep -c '^\[rms\]$' || true; }

# Adding writes one managed block, strips the server-side metadata, and is
# idempotent; a new URL replaces the block rather than adding a second one.
cm_with "${BASE}"
[[ "$(patch_rms_endpoint "${CM}" true "${URL}")" == "changed" ]] || fail "add: expected changed"
toml_of | grep -Fxq "api_url = \"${URL}\"" || fail "add: api_url missing"
toml_of | grep -Fxq "${BEGIN}" || fail "add: BEGIN marker missing"
toml_of | grep -Fxq "${END}" || fail "add: END marker missing"
[[ "$(count_rms_tables)" == "1" ]] || fail "add: expected exactly one [rms] table"
[[ "$(jq -r '.metadata.resourceVersion // "gone"' "${CM}")" == "gone" ]] || fail "add: resourceVersion not stripped"
[[ "$(jq -r '.data["unrelated.txt"]' "${CM}")" == "keep" ]] || fail "add: non-toml data changed"
[[ "$(patch_rms_endpoint "${CM}" true "${URL}")" == "nochange" ]] || fail "add twice: expected nochange"
[[ "$(patch_rms_endpoint "${CM}" true "https://other:1266")" == "changed" ]] || fail "new url: expected changed"
[[ "$(count_rms_tables)" == "1" ]] || fail "new url: expected exactly one [rms] table"
toml_of | grep -Fq "https://other:1266" || fail "new url: not written"
if toml_of | grep -Fq "${URL}"; then fail "new url: old url still present"; fi

# Removing restores the site config as it was.
[[ "$(patch_rms_endpoint "${CM}" false "${URL}")" == "changed" ]] || fail "remove: expected changed"
[[ "$(toml_of)" == "$(printf '%s' "${BASE}")" ]] || fail "remove: site config not restored: $(toml_of)"
[[ "$(patch_rms_endpoint "${CM}" false "${URL}")" == "nochange" ]] || fail "remove twice: expected nochange"

# Every TOML spelling that declares a root-level rms key is a conflict, and a
# conflict leaves the ConfigMap untouched.
conflicts=(
    $'[rms]\napi_url = "https://real:8801"\n'
    $'[rms]  # the real RMS\napi_url = "https://real:8801"\n'
    $'[ rms ]\napi_url = "https://real:8801"\n'
    $'["rms"]\napi_url = "https://real:8801"\n'
    $'[rms.tls]\nenforce = true\n'
    $'[[rms]]\napi_url = "https://real:8801"\n'
    $'rms.api_url = "https://real:8801"\n'
    $'rms = { api_url = "https://real:8801" }\n'
)
for extra in "${conflicts[@]}"; do
    case "${extra}" in
        rms*) cm_with "${extra}${BASE}" ;;   # root-level keys must precede any table
        *) cm_with "${BASE}${extra}" ;;
    esac
    before="$(cat "${CM}")"
    got="$(patch_rms_endpoint "${CM}" true "${URL}")"
    [[ "${got}" == "conflict" ]] || fail "expected conflict for $(printf '%q' "${extra}"), got '${got}'"
    [[ "$(cat "${CM}")" == "${before}" ]] || fail "conflict must leave the ConfigMap untouched"
done

# Look-alikes are not conflicts: a dotted rms key under another table, a
# commented-out header, and tables whose names merely contain "rms".
lookalikes=(
    $'[foo]\nrms.api_url = "https://real:8801"\n'
    $'# [rms]\n# api_url = "https://real:8801"\n'
    $'[rms_sim]\nversion_string = "x"\n'
    $'[rmsx]\nk = 1\n'
    $'[xrms]\nk = 1\n'
)
for extra in "${lookalikes[@]}"; do
    cm_with "${BASE}${extra}"
    got="$(patch_rms_endpoint "${CM}" true "${URL}")"
    [[ "${got}" == "changed" ]] || fail "expected changed for $(printf '%q' "${extra}"), got '${got}'"
    [[ "$(count_rms_tables)" == "1" ]] || fail "look-alike: expected exactly one [rms] table"
done

# A managed block that lost its END marker: only the managed lines go, the
# rest of the file stays, and the block is rewritten once.
cm_with "${BASE}"$'\n'"${BEGIN}"$'\n[rms]\napi_url = "https://old:1266"\n[other]\nk = 1\n'
got="$(patch_rms_endpoint "${CM}" true "${URL}" 2>"${TEST_TMP_DIR}/warn.txt")"
[[ "${got}" == "changed" ]] || fail "begin-only: expected changed, got '${got}'"
grep -q "no END marker" "${TEST_TMP_DIR}/warn.txt" || fail "begin-only: expected a warning"
toml_of | grep -Fxq '[other]' || fail "begin-only: the rest of the file was discarded"
toml_of | grep -Fxq 'k = 1' || fail "begin-only: the rest of the file was discarded"
if toml_of | grep -Fq 'https://old:1266'; then fail "begin-only: the stale api_url survived"; fi
[[ "$(count_rms_tables)" == "1" ]] || fail "begin-only: expected exactly one [rms] table"
toml_of | grep -Fxq "${END}" || fail "begin-only: the rewritten block has no END marker"
[[ "$(patch_rms_endpoint "${CM}" false "${URL}")" == "changed" ]] || fail "begin-only remove: expected changed"
toml_of | grep -Fxq '[other]' || fail "begin-only remove: the rest of the file was discarded"
[[ "$(count_rms_tables)" == "0" ]] || fail "begin-only remove: [rms] still present"

# Where python ships tomllib, a result that would not parse is reported and
# not written.
if python3 -c 'import tomllib' 2>/dev/null; then
    cm_with $'[site_explorer]\nbroken = \n'
    got="$(patch_rms_endpoint "${CM}" true "${URL}")"
    [[ "${got}" == invalid* ]] || fail "expected an invalid result, got '${got}'"
fi

echo "patch_rms_endpoint: ok"

# --- phase order --------------------------------------------------------------
# With --rms-sim the [rms] block must be written only after the deploy and the
# mount check, so a deploy that fails never leaves nico-api pointed at an
# endpoint nothing answers; the removal path (no flag) stays ahead of the
# deploy so a run without the flag restores the site whatever happens later.
# First line of the script holding the fixed string $1 ($2 "-x": whole line).
line_of() {
    local n
    n="$(grep -nF ${2:-} -- "$1" "${SETUP_SH}" | head -1 | cut -d: -f1)"
    [[ -n "${n}" ]] || fail "order: '$1' not found in setup-machine-a-tron.sh"
    echo "${n}"
}
rehearse="$(line_of 'apply_rms_endpoint true check')"
remove="$(line_of 'apply_rms_endpoint false')"
deploy="$(line_of 'helm upgrade --install "$RELEASE" "$CHART_DIR"')"
mount_check="$(line_of '"$_MAT_LOGS" == *"Mounting the RMS simulator"*')"
activate="$(line_of '        apply_rms_endpoint true' -x)"
(( rehearse < deploy )) || fail "order: the [rms] rehearsal must precede the deploy"
(( remove < deploy )) || fail "order: the [rms] removal must precede the deploy"
(( deploy < mount_check )) || fail "order: the mount check must follow the deploy"
(( mount_check < activate )) || fail "order: the [rms] block must be written after the mount check"
# The mount check is fatal: the line after the mount flag test is a die.
mount_gate="$(line_of '$_RMS_MOUNTED \')"
sed -n "$(( mount_gate + 1 ))p" "${SETUP_SH}" | grep -q '|| die' \
    || fail "order: a missing mount must be fatal under --rms-sim"

echo "phase order: ok"
echo "setup-machine-a-tron RMS simulator helper tests passed"
