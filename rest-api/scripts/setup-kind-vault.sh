#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# setup-kind-vault.sh — Initialize, unseal, and configure Vault for Kind e2e.
#
# Deploys a single-node production-mode Vault (no TLS, file storage) and
# configures Kubernetes auth so nico-api Core authenticates via its SA token.
#
# Run AFTER the Vault pod is Ready and BEFORE helm-deploy-core.
# Idempotent: safe to re-run on an already-configured Vault.
#
# Modelled on helm-prereqs/unseal_vault.sh from the production deployment.
#
# Required env:
#   VAULT_NAMESPACE      — Vault pod namespace (default: vault)
#   NICO_NAMESPACE       — Core namespace (default: nico-system)
#   CORE_SA_NAME         — Core service account name (default: core-grpc)
#   KV_MOUNT             — KV v2 mount path (default: secret)
#   PKI_MOUNT            — PKI mount path (default: pki)
#
set -euo pipefail

VAULT_NAMESPACE="${VAULT_NAMESPACE:-vault}"
NICO_NAMESPACE="${NICO_NAMESPACE:-nico-system}"
CORE_SA_NAME="${CORE_SA_NAME:-core-grpc}"
KV_MOUNT="${KV_MOUNT:-secret}"
PKI_MOUNT="${PKI_MOUNT:-pki}"
VAULT_ADDR="http://127.0.0.1:8200"
VAULT_POD="vault-0"
# Full init JSON stored as a K8s Secret; recreated on each kind-reset.
INIT_SECRET_NAME="vault-kind-keys"
STATUS_RETRIES=24
STATUS_SLEEP=5

# Poll vault status until parseable JSON is returned.
_vault_status_json() {
    local attempt output
    for attempt in $(seq 1 "${STATUS_RETRIES}"); do
        output=$(kubectl exec -n "${VAULT_NAMESPACE}" "${VAULT_POD}" -- \
            env VAULT_ADDR="${VAULT_ADDR}" vault status -format=json 2>/dev/null || true)
        if [[ -n "${output}" ]] && \
           echo "${output}" | jq -e '(.initialized | type == "boolean") and (.sealed | type == "boolean")' >/dev/null 2>&1; then
            printf '%s\n' "${output}"
            return 0
        fi
        echo "  Vault status not ready (${attempt}/${STATUS_RETRIES}); retrying in ${STATUS_SLEEP}s..." >&2
        sleep "${STATUS_SLEEP}"
    done
    echo "ERROR: Vault status not parseable after ${STATUS_RETRIES} attempts" >&2
    return 1
}

vault_exec() {
    kubectl exec -n "${VAULT_NAMESPACE}" "${VAULT_POD}" -- \
        env VAULT_ADDR="${VAULT_ADDR}" VAULT_TOKEN="${ROOT_TOKEN}" "$@"
}

# Like vault_exec but with stdin forwarded (required for heredoc policy writes).
vault_exec_stdin() {
    kubectl exec -i -n "${VAULT_NAMESPACE}" "${VAULT_POD}" -- \
        env VAULT_ADDR="${VAULT_ADDR}" VAULT_TOKEN="${ROOT_TOKEN}" "$@"
}

echo "=== Kind Vault setup ==="

# ---- Wait for pod ----
echo "Waiting for Vault pod ${VAULT_POD}..."
kubectl wait pod/"${VAULT_POD}" -n "${VAULT_NAMESPACE}" \
    --for=condition=Ready --timeout=180s

VAULT_STATUS=$(_vault_status_json)
INITIALIZED=$(echo "${VAULT_STATUS}" | jq -r '.initialized')

# ---- Init (idempotent) ----
if [[ "${INITIALIZED}" == "false" ]]; then
    echo "Initializing Vault (1 key share, threshold 1)..."
    # Write init JSON directly to a temp file — avoids capturing stderr in the
    # variable, which would corrupt jq parsing (see production unseal_vault.sh).
    INIT_TMP=$(mktemp)
    kubectl exec -n "${VAULT_NAMESPACE}" "${VAULT_POD}" -- \
        env VAULT_ADDR="${VAULT_ADDR}" \
        vault operator init -key-shares=1 -key-threshold=1 -format=json \
        > "${INIT_TMP}"
    kubectl -n "${VAULT_NAMESPACE}" create secret generic "${INIT_SECRET_NAME}" \
        --from-file=keys.json="${INIT_TMP}"
    rm -f "${INIT_TMP}"
    echo "Vault initialized; keys stored in ${VAULT_NAMESPACE}/${INIT_SECRET_NAME}."
else
    echo "Vault already initialized; reading stored keys..."
fi

# ---- Read keys from Secret ----
UNSEAL_KEY=$(kubectl -n "${VAULT_NAMESPACE}" get secret "${INIT_SECRET_NAME}" \
    -o jsonpath='{.data.keys\.json}' | base64 -d | jq -r '.unseal_keys_b64[0]')
ROOT_TOKEN=$(kubectl -n "${VAULT_NAMESPACE}" get secret "${INIT_SECRET_NAME}" \
    -o jsonpath='{.data.keys\.json}' | base64 -d | jq -r '.root_token')

# ---- Unseal (idempotent) ----
VAULT_STATUS=$(_vault_status_json)
SEALED=$(echo "${VAULT_STATUS}" | jq -r '.sealed')

if [[ "${SEALED}" == "true" ]]; then
    echo "Unsealing Vault..."
    kubectl exec -n "${VAULT_NAMESPACE}" "${VAULT_POD}" -- \
        env VAULT_ADDR="${VAULT_ADDR}" vault operator unseal "${UNSEAL_KEY}" \
        || echo "  unseal command failed; will verify status" >&2
    sleep 3
    VAULT_STATUS=$(_vault_status_json)
    SEALED=$(echo "${VAULT_STATUS}" | jq -r '.sealed')
    if [[ "${SEALED}" == "true" ]]; then
        echo "ERROR: Vault is still sealed after unseal attempt" >&2
        exit 1
    fi
    echo "Vault unsealed."
else
    echo "Vault already unsealed."
fi

# ---- Root token in nico-system for Core ----
echo "Creating nico-vault-token secret in ${NICO_NAMESPACE}..."
kubectl -n "${NICO_NAMESPACE}" create secret generic nico-vault-token \
    --from-literal=token="${ROOT_TOKEN}" \
    --dry-run=client -o yaml | kubectl apply -f -

# ---- Empty approle-tokens secret (chart keys are optional) ----
kubectl -n "${NICO_NAMESPACE}" create secret generic nico-vault-approle-tokens \
    --from-literal=VAULT_ROLE_ID="" \
    --from-literal=VAULT_SECRET_ID="" \
    --dry-run=client -o yaml | kubectl apply -f -

# ---- Enable KV v2 ----
echo "Enabling KV v2 at ${KV_MOUNT}/..."
vault_exec vault secrets enable -path="${KV_MOUNT}" kv-v2 2>/dev/null || true

# ---- Enable PKI ----
echo "Enabling PKI at ${PKI_MOUNT}/..."
vault_exec vault secrets enable -path="${PKI_MOUNT}" pki 2>/dev/null || true
vault_exec vault secrets tune -max-lease-ttl=87600h "${PKI_MOUNT}" 2>/dev/null || true

# ---- Write nico-vault-policy ----
# Use vault_exec_stdin (kubectl exec -i) so the heredoc is forwarded via stdin
# to vault inside the pod. Plain vault_exec (no -i) does not forward stdin.
echo "Writing nico-vault-policy..."
vault_exec_stdin vault policy write nico-vault-policy - << 'POLICY'
path "secret/data/*" {
  capabilities = ["read", "list"]
}
path "secret/metadata/*" {
  capabilities = ["list"]
}
path "secret/data/machines*" {
  capabilities = ["create", "read", "patch", "list", "update", "delete"]
}
path "secret/data/machines/*" {
  capabilities = ["create", "read", "patch", "list", "update", "delete"]
}
path "secret/metadata/machines/*" {
  capabilities = ["list", "delete"]
}
path "secret/destroy/machines/*" {
  capabilities = ["delete"]
}
path "secret/data/ufm/*" {
  capabilities = ["create", "read", "patch", "list", "update", "delete"]
}
path "secret/data/nmxm/*" {
  capabilities = ["create", "read", "patch", "list", "update", "delete"]
}
path "pki*" {
  capabilities = ["read", "list"]
}
path "pki/sign/nico-cluster" {
  capabilities = ["create", "update"]
}
path "pki/issue/nico-cluster" {
  capabilities = ["create", "update"]
}
POLICY

# ---- Enable Kubernetes auth ----
echo "Enabling Kubernetes auth method..."
vault_exec vault auth enable kubernetes 2>/dev/null || true

echo "Configuring Kubernetes auth..."
vault_exec vault write auth/kubernetes/config \
    kubernetes_host="https://kubernetes.default.svc"

# ---- Create K8s auth role for core-grpc SA ----
echo "Creating Kubernetes auth role for ${CORE_SA_NAME} SA in ${NICO_NAMESPACE}..."
vault_exec vault write "auth/kubernetes/role/${CORE_SA_NAME}" \
    bound_service_account_names="${CORE_SA_NAME}" \
    bound_service_account_namespaces="${NICO_NAMESPACE}" \
    policies=nico-vault-policy \
    ttl=1h

echo ""
echo "=== Kind Vault setup complete ==="
echo "  KV v2:    ${KV_MOUNT}/"
echo "  PKI:      ${PKI_MOUNT}/"
echo "  K8s role: ${CORE_SA_NAME} in ${NICO_NAMESPACE}"
