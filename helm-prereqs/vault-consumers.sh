#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# =============================================================================
# vault-consumers.sh — the Deployment probe that decides whether a workload
# still consumes Vault. Sourced by preflight.sh (and through it by setup.sh)
# and by health-check.sh, never executed directly.
# =============================================================================

# Prints one line per Deployment in namespace $1: "<name> <references>".
# <references> is empty for a Deployment that does not reference Vault, and
# otherwise says what makes it a Vault consumer:
#   VAULT_ADDR      a container or init container has an env entry of that name
#                   (the nico-api chart renders it only while
#                   nico-api.vault.enabled); reported alone, without the
#                   resources below
#   secret/<name> and configmap/<name>, sorted and comma-separated
#                   the pod references the nico-vault-token or
#                   nico-vault-approle-tokens Secret or the vault-cluster-info
#                   ConfigMap, which nico-prereqs renders into the NICo
#                   namespace only while vault.enabled: from a container or
#                   init container through env[].valueFrom (secretKeyRef,
#                   configMapKeyRef) or envFrom (secretRef, configMapRef), or
#                   from a secret, configMap, or projected volume
# Returns 1 when jq is not installed or the Deployments could not be listed
# (the reason goes to stderr), so a caller never reads a failure as "no
# Deployment references Vault".
_vault_deployment_references() {
    local _ns="$1"
    local _filter='
        def named($names; $kind): select(. as $n | $names | index($n)) | "\($kind)/\(.)";
        ["nico-vault-token", "nico-vault-approle-tokens"] as $secrets
        | ["vault-cluster-info"] as $configmaps
        | .items[]
        | .metadata.name as $name
        | (.spec.template.spec // {}) as $pod
        | [
            ( ($pod.containers // []) + ($pod.initContainers // []) | .[]
              | ( (.env // [])[]
                  | (select(.name == "VAULT_ADDR") | "VAULT_ADDR"),
                    (.valueFrom.secretKeyRef.name // empty | named($secrets; "secret")),
                    (.valueFrom.configMapKeyRef.name // empty | named($configmaps; "configmap")) ),
                ( (.envFrom // [])[]
                  | (.secretRef.name // empty | named($secrets; "secret")),
                    (.configMapRef.name // empty | named($configmaps; "configmap")) ) ),
            ( ($pod.volumes // [])[]
              | (.secret.secretName // empty | named($secrets; "secret")),
                (.configMap.name // empty | named($configmaps; "configmap")),
                ( (.projected.sources // [])[]
                  | (.secret.name // empty | named($secrets; "secret")),
                    (.configMap.name // empty | named($configmaps; "configmap")) ) )
          ]
        | unique
        | if index("VAULT_ADDR") then "VAULT_ADDR" else join(", ") end
        | "\($name) \(.)"'
    if ! command -v jq >/dev/null 2>&1; then
        echo "jq not found in PATH: install jq, which the Vault consumer probe needs to inspect the Deployments in ${_ns}" >&2
        return 1
    fi
    # pipefail is set only inside this subshell, so a failed kubectl is never
    # read as an empty list and the caller's shell options stay as they are.
    (
        set -o pipefail
        kubectl get deployments -n "${_ns}" -o json | jq -r "${_filter}"
    )
}
