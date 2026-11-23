#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
TEMPLATE="${SCRIPT_DIR}/../operators/dpf/dpfoperatorconfig.yaml.tmpl"
CRD="${SCRIPT_DIR}/../../crates/dpf/crds/apiextensions.k8s.io_v1_customresourcedefinition_dpfoperatorconfigs.operator.dpu.nvidia.com.yaml"
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_TMP_DIR}"' EXIT

eval "$(sed -n '/^_kea_nameservers()/,/^}/p' "${PREFLIGHT_SH}")"
declare -F _kea_nameservers >/dev/null || { echo "could not extract _kea_nameservers" >&2; exit 1; }

failures=0

# name | kea hookParameters block (empty means no nico-dhcp key) | expected
cases=(
    "single VIP|nameservers: \"10.0.0.53\"|10.0.0.53"
    "comma list|nameservers: \"10.0.0.53, 10.0.0.54\"|10.0.0.53 10.0.0.54"
    "empty value|nameservers: \"\"|"
    "no nico-dhcp||"
)
for case in "${cases[@]}"; do
    IFS='|' read -r name block expected <<< "${case}"
    values="${TEST_TMP_DIR}/values.yaml"
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

# Render with the whitelist setup.sh uses and check the result against the
# DPFOperatorConfig CRD that ships with the pinned DPF release.
whitelist="$(grep -B1 'operators/dpf/dpfoperatorconfig.yaml.tmpl' "${SETUP_SH}" \
    | grep -oE "envsubst '[^']+'" | sed -E "s/^envsubst '(.*)'$/\1/")"
NICO_DPF_K8S_API_VIP=192.0.2.10 NICO_DPF_K8S_API_PORT=6443 \
    NICO_DPF_COREDNS_UPSTREAM="10.0.0.53 10.0.0.54" \
    envsubst "${whitelist}" < "${TEMPLATE}" > "${TEST_TMP_DIR}/rendered.yaml"
if ! python3 - "${TEST_TMP_DIR}/rendered.yaml" "${CRD}" <<'PY'; then
import sys, yaml
cr = yaml.safe_load(open(sys.argv[1]))
crd = yaml.safe_load(open(sys.argv[2]))
schema = crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
props = schema["properties"]["coreDNS"]["properties"]
got = cr["spec"]["coreDNS"]
assert set(got) <= set(props), f"unknown coreDNS fields {set(got) - set(props)}"
assert props["upstreamNameservers"]["type"] == "string"
assert got["upstreamNameservers"] == "10.0.0.53 10.0.0.54", got
assert cr["spec"]["overrides"]["kubernetesAPIServerPort"] == 6443
PY
    echo "FAIL rendered DPFOperatorConfig"
    failures=$((failures + 1))
fi

if (( failures > 0 )); then
    echo "${failures} failure(s)"
    exit 1
fi
echo "PASS"
