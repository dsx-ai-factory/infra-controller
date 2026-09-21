#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Run the bundled CLI with a short-lived admin certificate, not Core's service identity.
set -euo pipefail
namespace="${LOCAL_DEV_NAMESPACE:-nico-system}"
pod="$(kubectl get pods -n "${namespace}" -l app.kubernetes.io/name=nico-api -o json |
  jq -er '[.items[] | select(.metadata.deletionTimestamp == null) |
    select(any(.status.conditions[]?; .type == "Ready" and .status == "True"))][0].metadata.name // empty')"

# shellcheck disable=SC2016 # Vault variables are expanded only inside the pod.
certificate="$(kubectl exec -n "${namespace}" "${pod}" -- sh -ec '
  : "${VAULT_TOKEN:?local Core admin access requires a Vault token}"
  curl --fail --silent --show-error --max-time 15 \
    -H "X-Vault-Token: ${VAULT_TOKEN}" \
    -H "X-Vault-Namespace: ${VAULT_NAMESPACE:-}" \
    -H "Content-Type: application/json" \
    --data '\''{"common_name":"devspace-admin","ttl":"1h"}'\'' \
    "${VAULT_ADDR}/v1/${VAULT_PKI_MOUNT_LOCATION}/issue/${VAULT_PKI_ROLE_NAME}"
' | jq -er '.data | select(.certificate != null and .private_key != null) |
  .private_key + "\n" + .certificate')"

# The certificate and private key stay off command lines and are removed on exit.
# The TLS key reader expects the private key to be the first PEM block.
# shellcheck disable=SC2016 # Arguments and the cleanup trap run inside the pod.
printf '%s\n' "${certificate}" | kubectl exec -i -n "${namespace}" "${pod}" -- sh -ec '
  umask 077
  directory=$(mktemp -d /tmp/nico-devspace-admin.XXXXXX)
  trap '\''rm -f -- "${directory}/identity.pem"; rmdir -- "${directory}"'\'' EXIT
  trap "exit 130" INT
  trap "exit 143" TERM
  cat > "${directory}/identity.pem"
  namespace=$1
  shift
  unset DISABLE_TLS_ENFORCEMENT
  /opt/carbide/nico-admin-cli \
    --api-url "https://nico-api.${namespace}.svc.cluster.local:1079" \
    --root-ca-path /var/run/secrets/spiffe.io/ca.crt \
    --client-cert-path "${directory}/identity.pem" \
    --client-key-path "${directory}/identity.pem" "$@"
' _ "${namespace}" "$@"
