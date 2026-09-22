#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Remove security-sensitive resources left by the retired PSM/NSM Vault-token
# Helm hook. PostgreSQL retains the manager databases and users, but the chart
# no longer manages their Kubernetes credential Secrets.
set -euo pipefail

NICO_SYSTEM_NS="${NICO_SYSTEM_NS:-nico-system}"
NICO_FLOW_NAMESPACE="${NICO_FLOW_NAMESPACE:-flow}"
VAULT_NS="${VAULT_NS:-vault}"

_namespaced_markers=""
_flow_markers=""
_cluster_markers=""
if ! _namespaced_markers="$(
    kubectl get job/flow-vault-tokens serviceaccount/flow-vault-tokens-sa \
        -n "${NICO_SYSTEM_NS}" --ignore-not-found -o name
)"; then
    echo "ERROR: unable to inspect legacy PSM/NSM hook resources" >&2
    exit 1
fi
if ! _flow_markers="$(
    kubectl get secret/psm-vault-token secret/nsm-vault-token \
        -n "${NICO_FLOW_NAMESPACE}" --ignore-not-found -o name
)"; then
    echo "ERROR: unable to inspect legacy PSM/NSM token Secrets" >&2
    exit 1
fi
if ! _cluster_markers="$(
    kubectl get clusterrole/flow-vault-tokens-writer \
        clusterrolebinding/flow-vault-tokens-writer \
        --ignore-not-found -o name
)"; then
    echo "ERROR: unable to inspect legacy PSM/NSM hook RBAC" >&2
    exit 1
fi
if [[ -z "${_namespaced_markers}${_flow_markers}${_cluster_markers}" ]]; then
    echo "No legacy PSM/NSM hook resources found; skipping cleanup"
    exit 0
fi

echo "Cleaning up legacy PSM/NSM deployment credentials and hook RBAC..."

# Stop token minting and remove its cluster-wide authorization before touching
# the credentials it produced.
kubectl delete job flow-vault-tokens -n "${NICO_SYSTEM_NS}" \
    --ignore-not-found
kubectl delete clusterrolebinding flow-vault-tokens-writer \
    --ignore-not-found

# Do not expose the Vault root token through shell tracing, argv, or the
# API-server audit log. Feed it to the Vault pod through stdin instead.
_restore_xtrace=false
if [[ $- == *x* ]]; then
    set +x
    _restore_xtrace=true
fi
_vault_token="$(kubectl -n "${VAULT_NS}" get secret vaultroottoken \
    -o jsonpath='{.data.token}' | base64 -d)"
if [[ -z "${_vault_token}" ]]; then
    echo "ERROR: ${VAULT_NS}/vaultroottoken contains no token" >&2
    exit 1
fi

# Revoke every matching accessor, including tokens orphaned by older hook runs
# that minted a replacement before deleting the previous Kubernetes Secret.
printf '%s\n' "${_vault_token}" | kubectl -n "${VAULT_NS}" exec -i vault-0 -- \
    sh -ceu '
        read -r VAULT_TOKEN
        export VAULT_TOKEN
        export VAULT_ADDR=https://127.0.0.1:8200
        export VAULT_SKIP_VERIFY=true

        accessors_json="$(vault list -format=json auth/token/accessors)"
        printf "%s\n" "$accessors_json" \
            | tr -d '\''[]"'\'' \
            | tr '\'',\'' '\''\n'\'' \
            | tr -d '\''[:space:]'\'' \
            | while IFS= read -r accessor; do
                [ -n "$accessor" ] || continue
                if ! token_json="$(vault token lookup -format=json \
                    -accessor "$accessor" 2>/dev/null)"; then
                    continue
                fi
                display_name="$(printf "%s\n" "$token_json" | sed -n \
                    '\''s/.*"display_name":[[:space:]]*"\([^\"]*\)".*/\1/p'\'')"
                case "$display_name" in
                    psm-vault-token|nsm-vault-token|token-psm-vault-token|token-nsm-vault-token)
                        vault token revoke -accessor "$accessor"
                        ;;
                esac
            done

        vault policy delete psm-vault-policy
        vault policy delete nsm-vault-policy
    '
unset _vault_token
if "${_restore_xtrace}"; then
    set -x
fi

kubectl delete secret psm-vault-token nsm-vault-token \
    -n "${NICO_FLOW_NAMESPACE}" --ignore-not-found
kubectl delete serviceaccount flow-vault-tokens-sa \
    -n "${NICO_SYSTEM_NS}" --ignore-not-found
kubectl delete clusterrole flow-vault-tokens-writer \
    --ignore-not-found

echo "Legacy PSM/NSM deployment credentials and hook RBAC removed"
