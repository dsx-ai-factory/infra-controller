#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBSERVABILITY_DIR="${SCRIPT_DIR}/../observability"
chart_version="$(sed -n 's/^OTEL_CHART_VER=.*:-\([^}]*\).*/\1/p' "${OBSERVABILITY_DIR}/install-observability.sh")"
if [[ -z "${chart_version}" ]]; then
    echo "could not read the collector chart version from install-observability.sh" >&2
    exit 1
fi

assert_config() {
    local path="$1" expected="$2" actual
    actual="$(yq -r "${path}" <<< "${config}")"
    if [[ "${actual}" != "${expected}" ]]; then
        printf '%s %s: expected %s, got %s\n' "${role}" "${path}" "${expected}" "${actual}" >&2
        exit 1
    fi
}

pod_address='[${env:MY_POD_IP}]'
for role in agent gateway; do
    # These values extend an upstream chart, so inspect the merged configuration,
    # including health checks and telemetry that are not local chart templates.
    rendered="$(helm template "otel-${role}" opentelemetry-collector \
        --repo https://open-telemetry.github.io/opentelemetry-helm-charts \
        --version "${chart_version}" --namespace otel \
        --values "${OBSERVABILITY_DIR}/values-otel-collector-${role}.yaml")"
    config="$(yq -r 'select(.kind == "ConfigMap") | .data.relay' <<< "${rendered}")"
    pod="$(yq 'select(.kind == "DaemonSet" or .kind == "Deployment") |
        .spec.template.spec.containers[0]' <<< "${rendered}")"
    if ! yq -e '
        ([.env[] | select(.name == "MY_POD_IP")] | length == 1) and
        (.env[] | select(.name == "MY_POD_IP") | .valueFrom.fieldRef.fieldPath == "status.podIP") and
        (.livenessProbe.httpGet.port == 13133) and
        (.readinessProbe.httpGet.port == 13133)
        ' <<< "${pod}" > /dev/null; then
        echo "${role}: expected pod IP injection and health probes on port 13133" >&2
        exit 1
    fi

    assert_config '.extensions.health_check.endpoint' "${pod_address}:13133"
    assert_config '.service.telemetry.metrics.address' "${pod_address}:8888"
    assert_config '.receivers.otlp.protocols.grpc.endpoint' "${pod_address}:4317"
    assert_config '.receivers.jaeger' null
    assert_config '.receivers.zipkin' null
    assert_config '.receivers.prometheus' null

    case "${role}" in
        agent)
            assert_config '.receivers.otlp.protocols.http.endpoint' "${pod_address}:4318"
            expected_ports=$'4317\n4318'
            ;;
        gateway)
            assert_config '.receivers.otlp.protocols.http' null
            assert_config '.receivers.otlp.protocols.grpc.tls.cert_file' /etc/otel/tls/tls.crt
            assert_config '.receivers.otlp.protocols.grpc.tls.key_file' /etc/otel/tls/tls.key
            assert_config '.receivers.otlp.protocols.grpc.tls.client_ca_file' /etc/otel/tls/ca.crt
            # Metrics accept either pod address while OTLP retains its primary-address bind.
            assert_config '.exporters."prometheus/site".endpoint' ':9999'
            assert_config '.receivers."prometheus/local-telemetry".config.scrape_configs[0].static_configs[0].targets[]' "${pod_address}:8888"
            expected_ports=$'443\n9999'
            ;;
    esac
    actual_ports="$(yq -r 'select(.kind == "Service") | .spec.ports[].port' <<< "${rendered}" | sort -n)"
    if [[ "${actual_ports}" != "${expected_ports}" ]]; then
        printf '%s: expected Service ports %s, got %s\n' "${role}" "${expected_ports}" "${actual_ports}" >&2
        exit 1
    fi
done

echo "observability collector address tests passed"
