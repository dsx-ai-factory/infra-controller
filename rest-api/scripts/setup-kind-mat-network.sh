#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# setup-kind-mat-network.sh — Create the MAT-BMC-SERVICES network segment in Core.
#
# machine_dhcp_records VIEW inner-joins domains ON machine_interfaces.domain_id.
# A segment without a subdomain_id leaves domain_id NULL, causing DiscoverDhcp
# to return 404 forever.
#
# CreateDomain is blocked for all gRPC callers (empty internal-RBAC vec[]).
# We insert the forward DNS domain directly into the database, then create the
# MAT-BMC-SERVICES segment via nico-admin-cli with --subdomain-id.
#
# Idempotent: re-running on an existing cluster updates subdomain/domain_id
# in-place without deleting rows (FK constraints prevent deletion).
#
# Run after helm-deploy-site-agent and before helm-deploy-mat.
# Requires: Core in nico-system, nico-rest-ca-issuer ClusterIssuer,
#           /opt/carbide/nico-admin-cli in the Core image.
#
set -euo pipefail

NAMESPACE="${NAMESPACE:-nico-system}"
CORE_URL="${CORE_URL:-https://core-grpc.nico-system.svc.cluster.local:1079}"
IMAGE="${IMAGE:-localhost:5000/nico-rest-core:latest}"
CERT_NAME="kind-admin-cli"
POD_NAME="kind-create-mat-network"
DB_POD="${DB_POD:-postgres-0}"
DB_NS="${DB_NS:-postgres}"
DB_USER="${DB_USER:-nico}"
DB_NAME="${DB_NAME:-nico}"
DOMAIN_NAME="${DOMAIN_NAME:-mat.kind.local}"

# DOMAIN_NAME is interpolated into SQL below; reject anything that could
# escape the single-quoted literal.
if [[ ! "${DOMAIN_NAME}" =~ ^[a-zA-Z0-9._-]+$ ]]; then
  echo "ERROR: DOMAIN_NAME '${DOMAIN_NAME}' contains characters outside [a-zA-Z0-9._-]" >&2
  exit 1
fi

# Remove this run's pod, Certificate, and TLS Secret on any exit, including
# failures and interrupts; without this a failed wait leaves them behind.
cleanup() {
  kubectl delete pod "${POD_NAME}" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
  kubectl delete certificate "${CERT_NAME}" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
  kubectl delete secret "${CERT_NAME}-tls" -n "${NAMESPACE}" --ignore-not-found >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "=== MAT network segment setup ==="

# Cleanup any leftover resources from a previous run
kubectl delete pod "${POD_NAME}" -n "${NAMESPACE}" --ignore-not-found 2>/dev/null
kubectl delete certificate "${CERT_NAME}" -n "${NAMESPACE}" --ignore-not-found 2>/dev/null
kubectl delete secret "${CERT_NAME}-tls" -n "${NAMESPACE}" --ignore-not-found 2>/dev/null

# ---- Ensure a forward DNS domain exists (CreateDomain is blocked for all gRPC callers) ----
DOMAIN_ID=$(kubectl exec -n "${DB_NS}" "${DB_POD}" -- psql -U "${DB_USER}" -d "${DB_NAME}" -t -c \
  "SELECT id FROM domains WHERE name NOT LIKE '%.in-addr.arpa' AND name NOT LIKE '%.ip6.arpa' AND deleted IS NULL LIMIT 1" \
  | tr -d '[:space:]')

if [ -z "${DOMAIN_ID}" ]; then
  echo "Creating forward DNS domain '${DOMAIN_NAME}'..."
  SOA='{"primary_ns":"ns1.mat.kind.local","contact":"hostmaster.mat.kind.local","serial":1,"refresh":3600,"retry":3600,"expire":604800,"minimum":120,"ttl":3600}'
  DOMAIN_ID=$(kubectl exec -n "${DB_NS}" "${DB_POD}" -- psql -U "${DB_USER}" -d "${DB_NAME}" -t -A -c \
    "INSERT INTO domains (name, soa) VALUES ('${DOMAIN_NAME}', '${SOA}') RETURNING id" \
    | head -1 | tr -d '[:space:]')
  if [ -z "${DOMAIN_ID}" ]; then
    echo "ERROR: domain INSERT returned no id" >&2
    exit 1
  fi
  echo "Created domain: ${DOMAIN_ID}"
else
  echo "Using existing forward domain: ${DOMAIN_ID}"
fi

# ---- Check idempotency: if segment already exists, update in-place ----
EXISTING=$(kubectl exec -n "${DB_NS}" "${DB_POD}" -- psql -U "${DB_USER}" -d "${DB_NAME}" -t -c \
  "SELECT id FROM network_segments WHERE name='MAT-BMC-SERVICES' AND deleted IS NULL" \
  | tr -d '[:space:]')
if [ -n "${EXISTING}" ]; then
  echo "Segment ${EXISTING} exists — updating subdomain and prefix..."
  kubectl exec -n "${DB_NS}" "${DB_POD}" -- psql -U "${DB_USER}" -d "${DB_NAME}" -c \
    "UPDATE network_segments SET subdomain_id='${DOMAIN_ID}' WHERE id='${EXISTING}';
     UPDATE machine_interfaces SET domain_id='${DOMAIN_ID}' WHERE segment_id='${EXISTING}';
     INSERT INTO network_prefixes (segment_id, prefix, gateway, num_reserved, svi_ip)
       SELECT '${EXISTING}', '192.168.102.0/24'::cidr, '192.168.102.1'::inet, 5, '192.168.102.1'::inet
       WHERE NOT EXISTS (SELECT 1 FROM network_prefixes WHERE segment_id='${EXISTING}');"
  echo "Segment updated."
  echo "=== MAT network segment setup complete ==="
  exit 0
fi

# ---- Issue a site-agent certificate (elektra-site-agent SPIFFE identity) ----
# CreateNetworkSegment requires the SiteAgent internal-RBAC principal.
echo "Issuing site-agent certificate..."
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
    - "spiffe://nico.local/elektra-site-agent/sa/elektra-site-agent"
  dnsNames:
    - ${CERT_NAME}.${NAMESPACE}.svc.cluster.local
EOF

echo "Waiting for certificate to be ready..."
kubectl wait certificate/"${CERT_NAME}" -n "${NAMESPACE}" \
  --for=condition=Ready --timeout=60s

# ---- Run nico-admin-cli to create the segment ----
echo "Creating MAT-BMC-SERVICES segment (subdomain=${DOMAIN_ID})..."
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
    - name: ${POD_NAME}
      image: ${IMAGE}
      imagePullPolicy: Never
      command:
        - /opt/carbide/nico-admin-cli
        - "--api-url=${CORE_URL}"
        - "--cloud-unsafe-op=admin"
        - network-segment
        - create
        - "--name=MAT-BMC-SERVICES"
        - "--segment-type=underlay"
        - "--prefix=192.168.102.0/24"
        - "--gateway=192.168.102.1"
        - "--reserve-first=5"
        - "--subdomain-id=${DOMAIN_ID}"
      volumeMounts:
        - name: tls
          mountPath: /var/run/secrets/spiffe.io
          readOnly: true
EOF

echo "Waiting for pod to complete..."
for i in $(seq 1 60); do
  PHASE=$(kubectl get pod "${POD_NAME}" -n "${NAMESPACE}" \
    -o jsonpath='{.status.phase}' 2>/dev/null || echo "Unknown")
  if [ "${PHASE}" = "Succeeded" ]; then
    echo "Pod succeeded."
    break
  elif [ "${PHASE}" = "Failed" ]; then
    echo "ERROR: Pod failed" >&2
    kubectl logs "${POD_NAME}" -n "${NAMESPACE}" 2>/dev/null || true
    exit 1
  fi
  sleep 2
done

if [ "${PHASE}" != "Succeeded" ]; then
  echo "ERROR: Pod did not complete within timeout (phase=${PHASE})" >&2
  kubectl logs "${POD_NAME}" -n "${NAMESPACE}" 2>/dev/null || true
  exit 1
fi

kubectl logs "${POD_NAME}" -n "${NAMESPACE}" 2>/dev/null || true

# Cleanup happens via the EXIT trap.
echo "=== MAT network segment setup complete ==="
