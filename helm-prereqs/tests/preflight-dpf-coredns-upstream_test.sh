#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
CRD="${SCRIPT_DIR}/../../crates/dpf/crds/apiextensions.k8s.io_v1_customresourcedefinition_dpfoperatorconfigs.operator.dpu.nvidia.com.yaml"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

# preflight.sh and the setup.sh step run with only these variables and with
# stubs for the cluster tools, so no cluster or registry is contacted.
mkdir -p "${TEST_TMP_DIR}/bin"
clean_env=(env -i "PATH=${TEST_TMP_DIR}/bin:${PATH}" "HOME=${HOME:-/}" "TEST_TMP_DIR=${TEST_TMP_DIR}")
"${clean_env[@]}" python3 -c 'import yaml' 2>/dev/null \
    || { echo "this test needs python3 with PyYAML" >&2; exit 1; }
command -v envsubst >/dev/null || { echo "this test needs envsubst (gettext)" >&2; exit 1; }

eval "$(sed -n '/^_kea_nameservers()/,/^}/p' "${PREFLIGHT_SH}")"
declare -F _kea_nameservers >/dev/null || { echo "could not extract _kea_nameservers" >&2; exit 1; }

failures=0
values="${TEST_TMP_DIR}/values.yaml"

# name | kea hookParameters block (empty means no nico-dhcp key) | expected
cases=(
    "single VIP|nameservers: \"10.0.0.53\"|10.0.0.53"
    "comma list|nameservers: \"10.0.0.53, 10.0.0.54\"|10.0.0.53, 10.0.0.54"
    "empty value|nameservers: \"\"|"
    "no nico-dhcp||"
)
for case in "${cases[@]}"; do
    IFS='|' read -r name block expected <<< "${case}"
    if [[ -n "${block}" ]]; then
        printf 'nico-dhcp:\n  config:\n    kea:\n      hookParameters:\n        %s\n' "${block}" > "${values}"
    else
        printf 'unbound:\n  enabled: true\n' > "${values}"
    fi
    got="$(_kea_nameservers "${values}")"
    if [[ "${got}" != "${expected}" ]]; then
        echo "FAIL ${name}: got '${got}', want '${expected}'"
        failures=$((failures + 1))
    fi
done

got="$(_kea_nameservers "${TEST_TMP_DIR}/missing.yaml")"
[[ -z "${got}" ]] || { echo "FAIL missing file: got '${got}'"; failures=$((failures + 1)); }

# The values file is UTF-8 whatever the locale of the calling shell.
printf '# caf\xc3\xa9\nnico-dhcp:\n  config:\n    kea:\n      hookParameters:\n        nameservers: "10.0.0.53"\n' > "${values}"
got="$(LC_ALL=C PYTHONUTF8=0 PYTHONCOERCECLOCALE=0 _kea_nameservers "${values}")"
[[ "${got}" == "10.0.0.53" ]] || { echo "FAIL non-ASCII values file in the C locale: got '${got}'"; failures=$((failures + 1)); }

# Records what setup.sh applies. Every other call fails, as without a cluster.
cat > "${TEST_TMP_DIR}/bin/kubectl" <<'FAKE_KUBECTL'
#!/usr/bin/env bash
[[ "$*" == "apply -f -" ]] || exit 1
cat >> "${TEST_TMP_DIR}/applied.yaml"
FAKE_KUBECTL
printf '#!/usr/bin/env bash\nexit 1\n' > "${TEST_TMP_DIR}/bin/helm"
cp "${TEST_TMP_DIR}/bin/helm" "${TEST_TMP_DIR}/bin/helmfile"
chmod +x "${TEST_TMP_DIR}/bin/kubectl" "${TEST_TMP_DIR}/bin/helm" "${TEST_TMP_DIR}/bin/helmfile"

# Stands in for setup.sh: sources preflight.sh, then runs the step that applies
# the DPFOperatorConfig. With PREVIOUS_NAMESERVERS set, preflight.sh first runs
# against a values file that holds that value, in the same shell.
cat > "${TEST_TMP_DIR}/setup-dpf-config.sh" <<'FAKE_SETUP'
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${PREFLIGHT_SH}")"
CORE_VALUES="${TEST_TMP_DIR}/core-values.yaml"
for nameservers in ${PREVIOUS_NAMESERVERS:+"${PREVIOUS_NAMESERVERS}"} "${NAMESERVERS}"; do
    printf 'nico-dhcp:\n  config:\n    kea:\n      hookParameters:\n        nameservers: "%s"\n' \
        "${nameservers}" > "${CORE_VALUES}"
    # shellcheck disable=SC1090 # The preflight.sh under test.
    source "${PREFLIGHT_SH}"
done
printf '%s\n' "${ERRORS[@]}" > "${TEST_TMP_DIR}/errors"
echo "${NICO_DPF_COREDNS_UPSTREAM-<unset>}" > "${TEST_TMP_DIR}/caller-value"
eval "${DPF_CONFIG_STEP}"
FAKE_SETUP

dpf_config_step="$(sed -n '/^    # 5b\.6 /,/^    # 5b\.7 /p' "${SETUP_SH}")"
[[ "${dpf_config_step}" == *dpfoperatorconfig.yaml.tmpl* ]] \
    || { echo "could not extract step 5b.6 from setup.sh" >&2; exit 1; }

# Prints spec.coreDNS.upstreamNameservers of the applied DPFOperatorConfig after
# checking the object against the checked-in DPF CRD (crates/dpf/crds).
applied_upstream() {
    python3 - "${TEST_TMP_DIR}/applied.yaml" "${CRD}" <<'PY'
import sys, yaml
docs = yaml.safe_load_all(open(sys.argv[1], encoding="utf-8"))
(cr,) = [d for d in docs if d and d["kind"] == "DPFOperatorConfig"]
crd = yaml.safe_load(open(sys.argv[2], encoding="utf-8"))
schema = crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
props = schema["properties"]["coreDNS"]["properties"]
got = cr["spec"]["coreDNS"]
assert set(got) <= set(props), f"unknown coreDNS fields {set(got) - set(props)}"
assert props["upstreamNameservers"]["type"] == "string"
assert isinstance(got["upstreamNameservers"], str), got
assert cr["spec"]["overrides"]["kubernetesAPIServerPort"] == 6443
print(got["upstreamNameservers"])
PY
}

# name | exported NICO_DPF_COREDNS_UPSTREAM ("-" means unset) | nameservers in
# the values file | --skip-core | nameservers at an earlier preflight run in the
# same shell (empty means no earlier run) | value in the applied DPFOperatorConfig
cases=(
    "file value is the default, commas become spaces|-|10.0.0.53, 10.0.0.54|false||10.0.0.53 10.0.0.54"
    "file value with --skip-core|-|10.0.0.53|true||10.0.0.53"
    "exported value wins, commas become spaces|10.0.0.60,10.0.0.61|10.0.0.53|false||10.0.0.60 10.0.0.61"
    "second run reads the changed file|-|10.0.0.60|false|10.0.0.53|10.0.0.60"
    "no value is an error|-||false||"
)
for case in "${cases[@]}"; do
    IFS='|' read -r name exported nameservers skip_core previous expected <<< "${case}"
    : > "${TEST_TMP_DIR}/applied.yaml"
    run=("${clean_env[@]}" "PREFLIGHT_SH=${PREFLIGHT_SH}" "DPF_CONFIG_STEP=${dpf_config_step}"
        "NAMESERVERS=${nameservers}" "PREVIOUS_NAMESERVERS=${previous}"
        AUTO_YES=true "SKIP_CORE=${skip_core}" SKIP_REST=true INSTALL_DPF=true INSTALL_RMS=false
        NICO_DPF_K8S_API_VIP=192.0.2.10 NICO_DPF_K8S_API_PORT=6443
        NICO_DPF_DPU_INTERFACE=eth0 NICO_DPF_DPU_CLUSTER_VIP=192.0.2.20)
    caller_value="<unset>"
    if [[ "${exported}" != "-" ]]; then
        run+=("NICO_DPF_COREDNS_UPSTREAM=${exported}")
        caller_value="${exported}"
    fi
    if ! "${run[@]}" "${BASH}" "${TEST_TMP_DIR}/setup-dpf-config.sh" > "${TEST_TMP_DIR}/run.out" 2>&1; then
        echo "FAIL ${name}: preflight.sh and the DPFOperatorConfig step did not complete"
        sed 's/^/    /' "${TEST_TMP_DIR}/run.out"
        failures=$((failures + 1))
        continue
    fi
    got="$(applied_upstream)" || got="<no valid DPFOperatorConfig>"
    if [[ "${got}" != "${expected}" ]]; then
        echo "FAIL ${name}: applied '${got}', want '${expected}'"
        failures=$((failures + 1))
    fi
    got="$(cat "${TEST_TMP_DIR}/caller-value")"
    if [[ "${got}" != "${caller_value}" ]]; then
        echo "FAIL ${name}: preflight.sh left NICO_DPF_COREDNS_UPSTREAM='${got}' in the calling shell, want '${caller_value}'"
        failures=$((failures + 1))
    fi
    error="$(grep -F NICO_DPF_COREDNS_UPSTREAM "${TEST_TMP_DIR}/errors" || true)"
    if [[ -n "${expected}" && -n "${error}" ]]; then
        echo "FAIL ${name}: unexpected preflight error: ${error}"
        failures=$((failures + 1))
    elif [[ -z "${expected}" && "${error}" != *"value was found in ${TEST_TMP_DIR}/core-values.yaml - set nameservers there"*"or export NICO_DPF_COREDNS_UPSTREAM"* ]]; then
        echo "FAIL ${name}: preflight error does not say what to do: '${error}'"
        failures=$((failures + 1))
    fi
done

if (( failures > 0 )); then
    echo "${failures} failure(s)"
    exit 1
fi
echo "PASS"
