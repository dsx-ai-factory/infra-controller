#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBSERVABILITY_DIR="${SCRIPT_DIR}/../observability"
INSTALLER="${OBSERVABILITY_DIR}/install-observability.sh"
# Run the DPU configuration, install and summary sections without a cluster.
# Printed IPv6 addresses need brackets; the MetalLB annotation must stay bare.
dpu_config="$(sed -n '/^OTEL_RECEIVER_VIP=/,/^echo "=== Observability:/p' "${INSTALLER}")"
dpu_install="$(sed -n '/^# --- optional: DPU OTLP/,/^# --- post-install checks/p' "${INSTALLER}")"
summary="$(sed -n '/^echo "=== Observability install complete ==="/,/^echo "  Docs:/p' "${INSTALLER}")"
if [[ -z "${dpu_config}" || -z "${dpu_install}" || -z "${summary}" ]]; then
    echo "could not extract the DPU gateway configuration, installation and summary" >&2
    exit 1
fi

helm() {
    printf 'helm argument: %s\n' "$@"
}

kubectl() {
    case "$*" in
        'get clusterissuer site-issuer' | 'wait --for=condition=Ready certificate/otel-receiver-tls -n otel --timeout=120s') ;;
        'apply -f -') cat > /dev/null ;;
        *) printf 'unexpected kubectl command: %s\n' "$*" >&2; return 1 ;;
    esac
}
export -f helm kubectl

while IFS='|' read -r vip expected_vip expected_address; do
    output="$(
        WITH_DPU=true WITH_TEMPO=false GRAFANA_VIP='' \
        OTEL_RECEIVER_VIP="${vip}" OTEL_RECEIVER_DNS=otel-receiver.forge \
        SITE_CA_ISSUER=site-issuer OTEL_CHART_VER=test OTEL_SITE_NAME=test \
        SCRIPT_DIR="${OBSERVABILITY_DIR}" \
            bash -euo pipefail -c "${dpu_config}
${dpu_install}
${summary}"
    )"
    expected_lines=(
        "--- [DPU 3/3] OTLP/mTLS gateway (${expected_address} -> Loki + Prometheus)"
        "  DPU OTLP:   otel-receiver.forge -> ${expected_address} (mTLS)"
        "helm argument: service.annotations.metallb\\.universe\\.tf/loadBalancerIPs=${expected_vip}"
    )
    for expected in "${expected_lines[@]}"; do
        if ! grep -Fxq -- "${expected}" <<< "${output}"; then
            printf 'VIP %s: expected line %s, got:\n%s\n' "${vip}" "${expected}" "${output}" >&2
            exit 1
        fi
    done
done <<'CASES'
2001:db8::10|2001:db8::10|[2001:db8::10]:443
[2001:db8::10]|2001:db8::10|[2001:db8::10]:443
192.0.2.10|192.0.2.10|192.0.2.10:443
CASES

echo "install-observability DPU output tests passed"
