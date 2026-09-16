#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# install-observability.sh — site-local metrics + logs + traces for a NICo cluster:
#   Loki (logs) + Tempo (traces) + OTEL collector agent (DaemonSet: pod logs -> Loki,
#   OTLP spans -> Tempo) + kube-prometheus-stack (Prometheus scraping the carbide_* /metrics
#   endpoints and hardware-health /telemetry via the NICo charts' ServiceMonitors + Grafana
#   with Prometheus/Loki/Tempo datasources and auto-loaded dashboards). Optional: an OTLP/mTLS
#   gateway for sites with real DPUs.
#
# Fully self-contained: everything runs and stays on the site; nothing leaves the cluster.
#
# Runs TWO ways, both idempotent (helm upgrade --install / server-side apply):
#   1. from setup.sh:        ./setup.sh --with-observability ...
#   2. standalone, any time: helm-prereqs/observability/install-observability.sh
#      (works on clusters installed long ago — no setup.sh state needed beyond the
#       local-path-persistent StorageClass and, for metrics, ServiceMonitors enabled in Core)
#
# Environment:
#   OTEL_SITE_NAME     Site label (forge_site) on every log line, metric, and trace.
#                      Default: siteName from helm-prereqs/values.yaml, else "nico-site".
#   GRAFANA_VIP        Optional MetalLB VIP for Grafana. Unset (default) = ClusterIP; reach
#                      Grafana with: kubectl -n monitoring port-forward svc/obs-grafana 3000:80
#   GRAFANA_ANONYMOUS  Default true: no login, anonymous Admin (internal-only sites). Set
#                      false to require the admin password (README "Securing Grafana").
#   PROMETHEUS_OPERATOR  Default true. Set false if the cluster already runs prometheus-operator.
#   WITH_TEMPO         Default true. false = skip Tempo and the agent's traces pipeline
#                      (metrics + logs only; the Grafana Tempo datasource will show as
#                      unavailable until Tempo is installed).
#   WITH_DPU           Default false. true = also deploy the OTLP/mTLS gateway for real DPUs
#                      (requires cert-manager + the site CA issuer + OTEL_RECEIVER_VIP).
#   OTEL_RECEIVER_VIP  MetalLB VIP for the DPU gateway (required when WITH_DPU=true).
#   OTEL_RECEIVER_DNS  SAN the DPUs dial. Default otel-receiver.forge — that exact name is
#                      what the DPU-side collector ships dialing (the .forge DPU-compat DNS
#                      zone); change it only together with the DPU-side configuration.
#   SITE_CA_ISSUER     ClusterIssuer for the gateway server cert — MUST be the ca-issuer backed
#                      by the CA the DPUs trust. Default site-issuer.
#   NICO_SERVICEMONITORS  Default hint when run standalone. setup.sh supplies true only after
#                      successfully installing Core from this tree in the same run. true applies
#                      the NICo Core metrics overlay. Set false to skip reconciliation.
#   NICO_RELEASE / NICO_NS   NICo Core release name/namespace. Default nico / nico-system.
#   LOKI_CHART_VER / TEMPO_CHART_VER / OTEL_CHART_VER / KPS_CHART_VER   Chart pin overrides.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREREQS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Chart pins. The otel IMAGE pin is load-bearing: the `loki` exporter was deprecated upstream
# in collector v0.107 and removed from the contrib distribution soon after — bumping requires
# moving logs to Loki-native OTLP (and Loki 3.x).
LOKI_CHART_VER="${LOKI_CHART_VER:-5.15.0}"     # Loki 2.8.4
TEMPO_CHART_VER="${TEMPO_CHART_VER:-2.2.3}"    # Tempo 2.10.x monolithic (grafana-community repo)
OTEL_CHART_VER="${OTEL_CHART_VER:-0.106.0}"    # image contrib:0.106.1
KPS_CHART_VER="${KPS_CHART_VER:-59.1.0}"

# siteName may be bare, single- or double-quoted in YAML; strip either style (same parse as setup.sh).
_VALUES_SITE="$(awk '/^siteName:/{v=$2; gsub(/["'"'"']/,"",v); print v}' "${PREREQS_DIR}/values.yaml" 2>/dev/null || true)"
OTEL_SITE_NAME="${OTEL_SITE_NAME:-${_VALUES_SITE:-nico-site}}"
GRAFANA_VIP="${GRAFANA_VIP:-}"
GRAFANA_ANONYMOUS="${GRAFANA_ANONYMOUS:-true}"
PROMETHEUS_OPERATOR="${PROMETHEUS_OPERATOR:-true}"
WITH_TEMPO="${WITH_TEMPO:-true}"
WITH_DPU="${WITH_DPU:-false}"
OTEL_RECEIVER_VIP="${OTEL_RECEIVER_VIP:-}"
OTEL_RECEIVER_DNS="${OTEL_RECEIVER_DNS:-otel-receiver.forge}"
SITE_CA_ISSUER="${SITE_CA_ISSUER:-site-issuer}"

command -v kubectl >/dev/null || { echo "ERROR: kubectl not found" >&2; exit 1; }
command -v helm    >/dev/null || { echo "ERROR: helm not found" >&2; exit 1; }
if [[ "${WITH_DPU}" == "true" && -z "${OTEL_RECEIVER_VIP}" ]]; then
    echo "ERROR: WITH_DPU=true requires OTEL_RECEIVER_VIP (a MetalLB VIP the DPUs can reach)" >&2
    exit 1
fi

echo "=== Observability: Loki + Tempo + OTEL + Prometheus/Grafana (site '${OTEL_SITE_NAME}') ==="

# --- preflight: StorageClass -------------------------------------------------------------
if ! kubectl get storageclass local-path-persistent >/dev/null 2>&1; then
    echo "ERROR: StorageClass 'local-path-persistent' not found — Loki/Tempo/Prometheus PVCs need it." >&2
    echo "       On a helm-prereqs cluster it comes from setup.sh Phase 2; standalone:" >&2
    echo "       kubectl apply -f ${PREREQS_DIR}/operators/storageclass-local-path-persistent.yaml" >&2
    exit 1
fi

echo "--- [1/8] helm repos"
# --force-update tolerates a pre-existing repo entry with a different URL; the update is
# scoped to these four repos so an unrelated broken repo on the machine cannot abort the run.
helm repo add --force-update grafana https://grafana.github.io/helm-charts >/dev/null
# The tempo chart moved off grafana/helm-charts; it now lives in grafana-community.
helm repo add --force-update grafana-community https://grafana-community.github.io/helm-charts >/dev/null
helm repo add --force-update open-telemetry https://open-telemetry.github.io/opentelemetry-helm-charts >/dev/null
helm repo add --force-update prometheus-community https://prometheus-community.github.io/helm-charts >/dev/null
helm repo update grafana grafana-community open-telemetry prometheus-community >/dev/null

echo "--- [2/8] namespaces (loki, tempo, otel, monitoring)"
OBS_NAMESPACES=(loki otel monitoring); [[ "${WITH_TEMPO}" == "true" ]] && OBS_NAMESPACES+=(tempo)
for ns in "${OBS_NAMESPACES[@]}"; do kubectl create ns "$ns" 2>/dev/null || true; done

echo "--- [3/8] Loki ${LOKI_CHART_VER} (single-binary, local-path 50Gi, 15d retention)"
helm upgrade --install loki grafana/loki --version "${LOKI_CHART_VER}" -n loki \
    -f "${SCRIPT_DIR}/values-loki.yaml" --wait --timeout 300s

if [[ "${WITH_TEMPO}" == "true" ]]; then
    echo "--- [4/8] Tempo ${TEMPO_CHART_VER} (monolithic, local-path, OTLP ingest)"
    helm upgrade --install tempo grafana-community/tempo --version "${TEMPO_CHART_VER}" -n tempo \
        -f "${SCRIPT_DIR}/values-tempo.yaml" --wait --timeout 300s
else
    echo "--- [4/8] Tempo skipped (WITH_TEMPO=false)"
fi

echo "--- [5/8] OTEL collector agent (DaemonSet: pod logs -> Loki, OTLP spans -> Tempo)"
# extraEnvs[0] is OTEL_SITE_NAME in the values file; the index-targeted override is the
# per-site knob so the values files never need editing.
AGENT_ARGS=( --set-string "extraEnvs[0].value=${OTEL_SITE_NAME}" )
if [[ "${WITH_TEMPO}" != "true" ]]; then
    # Without Tempo there is nowhere to send spans — drop the traces pipeline and its listener.
    AGENT_ARGS+=( --set "config.service.pipelines.traces=null" )
fi
helm upgrade --install otel-agent open-telemetry/opentelemetry-collector \
    --version "${OTEL_CHART_VER}" -n otel \
    -f "${SCRIPT_DIR}/values-otel-collector-agent.yaml" \
    "${AGENT_ARGS[@]}" \
    --wait --timeout 300s
# nico-dns's span exporter is always-on and defaults to
# opentelemetry-collector.otel.svc.cluster.local:4317 (crates/dns/src/config.rs; no standard
# deploy values override it) — without this alias its exports blackhole. The ExternalName
# CNAMEs that default name onto the agent Service.
kubectl apply -f - <<'ALIAS'
apiVersion: v1
kind: Service
metadata:
  name: opentelemetry-collector
  namespace: otel
spec:
  type: ExternalName
  externalName: otel-agent.otel.svc.cluster.local
ALIAS

echo "--- [6/8] prometheus-operator CRDs (only the MISSING ones, server-side)"
# The chart runs crds.enabled=false: a helm-prereqs cluster already has several of these CRDs
# (setup.sh Phase 2 applies operators/crds/), and other clusters may have a partial set owned
# by another manager. Server-side-applying only the missing ones never touches existing CRDs.
CRD_TMP="$(mktemp -d)"
trap 'rm -rf "${CRD_TMP}"' EXIT
# NOTE: on a helm-prereqs cluster the pre-existing CRDs (operators/crds/) may be NEWER than
# this chart's operator; that is fine — the operator uses stable fields and newer CRDs are
# forward-compatible. Only missing CRDs are added, at this chart's version.
helm pull prometheus-community/kube-prometheus-stack --version "${KPS_CHART_VER}" --untar -d "${CRD_TMP}" >/dev/null
for crd_file in "${CRD_TMP}/kube-prometheus-stack/charts/crds/crds/"*.yaml; do
    crd_name="$(awk '/^  name:/{print $2; exit}' "${crd_file}")"
    if kubectl get crd "${crd_name}" >/dev/null 2>&1; then
        echo "    exists : ${crd_name}"
    else
        echo "    install: ${crd_name}"
        kubectl apply --server-side -f "${crd_file}"
    fi
done
rm -rf "${CRD_TMP}"

echo "--- [7/8] kube-prometheus-stack ${KPS_CHART_VER} (Prometheus + Grafana)"
KPS_ARGS=(
    --set-string "prometheus.prometheusSpec.externalLabels.forge_site=${OTEL_SITE_NAME}"
    --set "prometheusOperator.enabled=${PROMETHEUS_OPERATOR}"
)
if [[ "${GRAFANA_ANONYMOUS}" != "true" ]]; then
    # Turn anonymous access off AND bring the login form back (the values file disables it),
    # otherwise nobody could log in at all. Credentials: admin / the chart's admin secret
    # (kubectl -n monitoring get secret obs-grafana -o jsonpath='{.data.admin-password}' | base64 -d)
    KPS_ARGS+=(
        --set "grafana.grafana\.ini.auth\.anonymous.enabled=false"
        --set "grafana.grafana\.ini.auth.disable_login_form=false"
    )
fi
if [[ -n "${GRAFANA_VIP}" ]]; then
    KPS_ARGS+=(
        --set-string "grafana.service.type=LoadBalancer"
        --set-string "grafana.service.annotations.metallb\.universe\.tf/loadBalancerIPs=${GRAFANA_VIP}"
    )
fi
helm upgrade --install obs prometheus-community/kube-prometheus-stack \
    --version "${KPS_CHART_VER}" -n monitoring \
    -f "${SCRIPT_DIR}/values-kube-prometheus-stack.yaml" \
    "${KPS_ARGS[@]}" --wait --timeout 600s

echo "--- [8/8] Grafana dashboards (ConfigMaps labelled grafana_dashboard; sidecar auto-loads)"
if compgen -G "${SCRIPT_DIR}/dashboards/*.json" >/dev/null; then
    for dash in "${SCRIPT_DIR}"/dashboards/*.json; do
        name="dash-$(basename "${dash}" .json)"
        kubectl create configmap "${name}" -n monitoring \
            --from-file="$(basename "${dash}")=${dash}" \
            --dry-run=client -o yaml \
          | kubectl label -f - --local --dry-run=client -o yaml grafana_dashboard=1 \
          | kubectl apply -f -
        echo "    dashboard: ${name}"
    done
else
    echo "    (no dashboards/*.json)"
fi

# --- optional: DPU OTLP/mTLS gateway ------------------------------------------------------
if [[ "${WITH_DPU}" == "true" ]]; then
    echo "--- [DPU 1/3] mTLS pre-flight: '${SITE_CA_ISSUER}' must be a ca-issuer the DPUs trust"
    # The DPU's /etc/otelcol-contrib/certs/ca.pem is the site root CA and its client cert is
    # signed by the same root — so the gateway's server cert must be a leaf signed DIRECTLY by
    # that root. A dev-CA issuer here produces "x509: certificate signed by unknown authority"
    # on the DPU side.
    kubectl get clusterissuer "${SITE_CA_ISSUER}" >/dev/null \
        || { echo "ERROR: ClusterIssuer ${SITE_CA_ISSUER} not found" >&2; exit 1; }

    echo "--- [DPU 2/3] gateway server certificate (${OTEL_RECEIVER_DNS})"
    sed -e "s/name: site-issuer/name: ${SITE_CA_ISSUER}/" \
        -e "s/- otel-receiver\.forge/- ${OTEL_RECEIVER_DNS}/" \
        "${SCRIPT_DIR}/otel-receiver-certificate.yaml" | kubectl apply -f -
    # If re-pointing the issuer of an EXISTING cert and cert-manager doesn't reissue:
    #   kubectl -n otel delete secret otel-receiver-tls
    kubectl wait --for=condition=Ready certificate/otel-receiver-tls -n otel --timeout=120s

    echo "--- [DPU 3/3] OTLP/mTLS gateway (${OTEL_RECEIVER_VIP}:443 -> Loki + Prometheus)"
    helm upgrade --install otel-collector-gateway open-telemetry/opentelemetry-collector \
        --version "${OTEL_CHART_VER}" -n otel \
        -f "${SCRIPT_DIR}/values-otel-collector-gateway.yaml" \
        --set-string "extraEnvs[0].value=${OTEL_SITE_NAME}" \
        --set-string "service.annotations.metallb\.universe\.tf/loadBalancerIPs=${OTEL_RECEIVER_VIP}" \
        --wait --timeout 300s
else
    echo "--- DPU gateway skipped (WITH_DPU=true to install; real-DPU sites only)"
fi

# --- post-install checks ------------------------------------------------------------------
echo ""
echo "--- verify"
kubectl -n loki get pods
[[ "${WITH_TEMPO}" == "true" ]] && kubectl -n tempo get pods
kubectl -n otel get pods
kubectl -n monitoring get pods | head -8

# --- NICo metrics --------------------------------------------------------------------------
# The NICo subcharts ship ServiceMonitor templates but default them OFF — nothing scrapes the
# carbide_* metrics until they exist. The bundled overlay enables all required metrics endpoints.
# NICO_SERVICEMONITORS: true = apply it via a release upgrade with THIS tree's chart (setup.sh
# passes true only when this run installed Core from the same tree); hint (standalone and
# setup.sh --skip-core default) = print the command instead, because upgrading with a checkout
# that differs from what is deployed could change more than metrics collection; false = skip
# silently. Reapply it unconditionally because existing monitors may not cover all current
# endpoints.
_inspect_nico_release() {
    local status_output
    local status_code

    if status_output="$(helm status "${NICO_RELEASE}" -n "${NICO_NS}" 2>&1)"; then
        _NICO_RELEASE_PRESENT=true
        return 0
    else
        status_code=$?
    fi

    # Helm returns this storage-driver sentinel only when the named release is absent.
    if [[ "${status_output}" == *"release: not found"* ]]; then
        _NICO_RELEASE_PRESENT=false
        return 0
    fi

    echo "ERROR: unable to inspect NICo release '${NICO_RELEASE}' in ${NICO_NS}:" >&2
    printf '%s\n' "${status_output}" >&2
    return "${status_code}"
}

NICO_SERVICEMONITORS="${NICO_SERVICEMONITORS:-hint}"
NICO_RELEASE="${NICO_RELEASE:-nico}"
NICO_NS="${NICO_NS:-nico-system}"
_NICO_RELEASE_PRESENT=false
if [[ "${NICO_SERVICEMONITORS}" != "false" ]]; then
    _inspect_nico_release || exit $?
fi

if [[ "${NICO_SERVICEMONITORS}" == "false" ]]; then
    echo "--- NICo direct metrics reconciliation skipped (NICO_SERVICEMONITORS=false)"
elif [[ "${_NICO_RELEASE_PRESENT}" != "true" ]]; then
    echo "--- NICo release '${NICO_RELEASE}' not found in ${NICO_NS} — metrics collection deferred."
    echo "    After installing NICo Core, re-run this script (idempotent) or:"
    echo "      helm upgrade ${NICO_RELEASE} <chart-ref> -n ${NICO_NS} --reuse-values \\"
    echo "        -f ${SCRIPT_DIR}/values-nico-servicemonitors.yaml"
elif [[ "${NICO_SERVICEMONITORS}" == "true" ]]; then
    echo "--- reconciling NICo direct metrics collection (ServiceMonitors + hardware-health sink)"
    helm upgrade "${NICO_RELEASE}" "${PREREQS_DIR}/../helm" -n "${NICO_NS}" --reuse-values \
        -f "${SCRIPT_DIR}/values-nico-servicemonitors.yaml"
else
    echo "--- NICo direct metrics collection was not reconciled (standalone safety default)."
    echo "    Apply the overlay with YOUR deployed chart ref:"
    echo "      helm upgrade ${NICO_RELEASE} <chart-ref> -n ${NICO_NS} --reuse-values \\"
    echo "        -f ${SCRIPT_DIR}/values-nico-servicemonitors.yaml"
    echo "    Or re-run with NICO_SERVICEMONITORS=true if this checkout matches the deployed chart."
fi

echo ""
echo "=== Observability install complete ==="
echo "  Grafana:    $([[ -n "${GRAFANA_VIP}" ]] && echo "http://${GRAFANA_VIP} (VIP)" || echo "kubectl -n monitoring port-forward svc/obs-grafana 3000:80  ->  http://localhost:3000")"
echo "  Loki:       loki.loki.svc.cluster.local:3100        (X-Scope-OrgID: forge)"
[[ "${WITH_TEMPO}" == "true" ]] && echo "  Tempo:      tempo.tempo.svc.cluster.local:4317 (OTLP ingest), :3200 (query API)"
echo "  Prometheus: obs-prometheus.monitoring.svc.cluster.local:9090"
[[ "${WITH_DPU}" == "true" ]] && echo "  DPU OTLP:   ${OTEL_RECEIVER_DNS} -> ${OTEL_RECEIVER_VIP}:443 (mTLS)"
echo "  Docs:       helm-prereqs/observability/README.md"
