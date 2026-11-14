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

# verify-kind-bmc-proxy.sh — Exercise a Core-backed BMC lookup through the
# BMC proxy, beyond a rollout check.
#
# The proxy resolves a Redfish request's `Forwarded: host=<bmc-ip>` header by
# asking Core for the BMC's MAC address (find_mac_address_by_bmc_ip), then
# proxies to the resolved BMC. A successful 2xx response therefore proves the
# whole path: mTLS admission, Forwarded parsing, the Core lookup (against the
# core-grpc Service), and the upstream proxy hop. A rollout check alone does
# not test any of this.
#
# The probe uses a short-lived certificate with the SPIFFE identity
# nico-system/sa/kind-bmc-verify, which HELM_SET_BMC_PROXY's
# auth.apiPrincipal=kind-bmc-verify authorizes.
#
# Requires: Core deployed as core-grpc (KIND_CORE=true), BMC proxy deployed
# (KIND_BMC_PROXY=true), at least one registered MAT BMC Service
# (kind-verify-mat), and the Core image (contains curl) in the cluster.

set -euo pipefail

NAMESPACE="${NAMESPACE:-nico-system}"
PROXY_URL="${PROXY_URL:-https://nico-bmc-proxy.nico-system.svc.cluster.local:1079}"
IMAGE="${IMAGE:-localhost:5000/nico-rest-core:latest}"
CERT_NAME="kind-bmc-verify"
POD_NAME="kind-bmc-verify-probe"

# Remove this run's probe pod, Certificate, and TLS Secret on any exit.
cleanup() {
  kubectl delete pod "${POD_NAME}" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
  kubectl delete certificate "${CERT_NAME}" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
  kubectl delete secret "${CERT_NAME}-tls" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "=== BMC proxy Core-backed lookup verify ==="

# ---- Find a registered MAT BMC external IP ----
BMC_IP=$(kubectl -n "${NAMESPACE}" get svc -o json | jq -r \
  '.items[]
    | select(.metadata.labels["app.kubernetes.io/managed-by"] == "mat-k8s-controller")
    | select(.metadata.labels["nvidia-infra-controller/mat-machine-type"] | IN("host", "dpu", "pod-bmc"))
    | select((.spec.externalIPs // []) | length > 0)
    | .spec.externalIPs[0]' | head -1)
if [ -z "${BMC_IP}" ]; then
  echo "ERROR: no MAT BMC Service with externalIPs found; run kind-verify-mat first" >&2
  exit 1
fi
echo "Probing with Forwarded: host=${BMC_IP}"

# ---- Issue the short-lived probe certificate ----
kubectl apply -f - <<EOF
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: ${CERT_NAME}
  namespace: ${NAMESPACE}
spec:
  secretName: ${CERT_NAME}-tls
  duration: 1h
  issuerRef:
    name: nico-rest-ca-issuer
    kind: ClusterIssuer
    group: cert-manager.io
  privateKey:
    algorithm: ECDSA
    size: 256
  uris:
    - "spiffe://nico.local/${NAMESPACE}/sa/${CERT_NAME}"
  dnsNames:
    - ${CERT_NAME}.${NAMESPACE}.svc.cluster.local
EOF

echo "Waiting for certificate to be ready..."
kubectl wait certificate/"${CERT_NAME}" -n "${NAMESPACE}" \
  --for=condition=Ready --timeout=60s

# ---- Run the probe pod (the Core image ships curl) ----
# Delete a leftover probe Pod from an interrupted earlier run first: applying
# onto a completed Pod would keep its stale logs and skip the fresh probe.
kubectl delete pod "${POD_NAME}" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
kubectl wait "pod/${POD_NAME}" -n "${NAMESPACE}" --for=delete --timeout=30s 2>/dev/null || true
kubectl apply -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: ${POD_NAME}
  namespace: ${NAMESPACE}
spec:
  restartPolicy: Never
  volumes:
    - name: tls
      secret:
        secretName: ${CERT_NAME}-tls
  containers:
    - name: probe
      image: ${IMAGE}
      imagePullPolicy: Never
      command: ["/bin/sh", "-c"]
      args:
        - |
          code=\$(curl -s -o /dev/null -w '%{http_code}' \
            --cacert /var/run/secrets/spiffe.io/ca.crt \
            --cert /var/run/secrets/spiffe.io/tls.crt \
            --key /var/run/secrets/spiffe.io/tls.key \
            -H 'Forwarded: host=${BMC_IP}' \
            '${PROXY_URL}/redfish/v1/')
          echo "HTTP_CODE=\${code}"
      volumeMounts:
        - name: tls
          mountPath: /var/run/secrets/spiffe.io
          readOnly: true
EOF

echo "Waiting for probe pod to complete..."
PHASE="Unknown"
for attempt in $(seq 1 60); do
  PHASE=$(kubectl get pod "${POD_NAME}" -n "${NAMESPACE}" \
    -o jsonpath='{.status.phase}' 2>/dev/null || echo "Unknown")
  if [ "${PHASE}" = "Succeeded" ] || [ "${PHASE}" = "Failed" ]; then
    break
  fi
  sleep 2
done

LOGS=$(kubectl logs "${POD_NAME}" -n "${NAMESPACE}" 2>/dev/null || true)
echo "${LOGS}"

if [ "${PHASE}" != "Succeeded" ]; then
  echo "ERROR: probe pod did not succeed (phase=${PHASE})" >&2
  exit 1
fi

HTTP_CODE=$(echo "${LOGS}" | sed -n 's/^HTTP_CODE=\([0-9]*\)$/\1/p')
case "${HTTP_CODE}" in
  2*)
    echo "BMC proxy verify: OK — Core-backed lookup succeeded (HTTP ${HTTP_CODE})"
    ;;
  400)
    echo "ERROR: proxy could not resolve the BMC from the Forwarded header (HTTP 400); the Core lookup path failed" >&2
    exit 1
    ;;
  502)
    echo "ERROR: proxy's Core lookup errored (HTTP 502); check that apiServiceName points at core-grpc" >&2
    exit 1
    ;;
  *)
    echo "ERROR: unexpected HTTP status ${HTTP_CODE:-none} from the proxy" >&2
    exit 1
    ;;
esac
