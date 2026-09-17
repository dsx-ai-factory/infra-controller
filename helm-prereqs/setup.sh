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

# =============================================================================
# setup.sh — install the NICo prerequisite stack
#
# Tool requirements:
#   helmfile, helm, kubectl, jq, ssh-keygen
#
# Required environment:
#   KUBECONFIG            Optional only if the current kubectl context already
#                         points at the target cluster.
#   NICO_IMAGE_REGISTRY    Required unless both --skip-core and --skip-rest are
#                         used. Registry/repository prefix for NICo images,
#                         without http(s)://. Example: registry.example.com/nico
#   NICO_CORE_IMAGE_TAG    Required unless --skip-core is used.
#                         NICo Core tag. Example: v2025.12.30
#   NICO_REST_IMAGE_TAG    Required unless --skip-rest is used.
#                         NICo REST tag. Example: v1.0.4
#
# Optional environment:
#   NICO_CORE_IMAGE_NAME   NICo Core image name under NICO_IMAGE_REGISTRY.
#                         Default: nico. Use nvmetal-carbide for legacy images.
#   REGISTRY_PULL_SECRET   Registry password/API key. If unset, setup does not
#                          create image pull secrets; images must be public,
#                          preloaded, or use existing imagePullSecrets.
#   REGISTRY_PULL_USERNAME Username for generated pull secrets.
#                          Default: $oauthtoken
#   NICO_SITE_UUID          REST site UUID. Used only when REST is deployed.
#                          If unset, setup resolves it: prior install ConfigMap, existing site by name, else mints and seeds the site record.
#   NICO_MANAGE_DEFAULT_STORAGE_CLASS
#                          Whether setup annotates local-path as the default
#                          StorageClass. Default: true.
#   NICO_STORAGE_CLASS     StorageClass for Postgres and Vault data and audit PVCs.
#                          Default: local-path-persistent.
#   VAULT_NS               Vault namespace. Default: vault
#   CERT_MANAGER_NS        cert-manager namespace. Default: cert-manager
#   PREFLIGHT_CHECK_IMAGE  Image for preflight per-node checks.
#                          Default: busybox:1.36
#
# Usage:
#   export KUBECONFIG=/path/to/kubeconfig
#   export NICO_IMAGE_REGISTRY=<registry>    # unless using --skip-core --skip-rest
#   export NICO_CORE_IMAGE_TAG=<tag>       # unless using --skip-core
#   export NICO_REST_IMAGE_TAG=<tag>       # unless using --skip-rest
#   export REGISTRY_PULL_SECRET=<secret>  # optional
#   ./setup.sh                          # prompts before deploying NICo Core and NICo REST
#   ./setup.sh -y                       # skip all prompts, deploy everything automatically
#   ./setup.sh --skip-core              # skip Phase 6 NICo Core (print command, deploy manually)
#   ./setup.sh --skip-rest              # skip Phase 7 NICo REST entirely (no repo needed)
#   ./setup.sh --skip-flow              # skip Phase 7h NICo Flow (REST still installs)
#                                       #   pair with helm-prereqs/values.yaml::flow.enabled=false
#                                       #   to skip Flow prereqs (DBs / ESO / vault tokens) too
#   ./setup.sh --skip-core --skip-rest  # fully non-interactive infra-only run
#   ./setup.sh --core-values /path/to/values.yaml      # use site-specific values for Phase 6
#   ./setup.sh --metallb-config /path/to/metallb.yaml  # use site-specific MetalLB config (file or kustomize dir)
#   ./setup.sh --site-overlay /path/to/kustomize-dir   # kubectl apply -k after Phase 6 (NTP services, etc.)
#   ./setup.sh --debug                  # enable bash -x trace (or run: bash -x ./setup.sh)
#
# Notes:
#   - --core-values supplies site-specific NICo Core Helm values.
#   - --metallb-config supplies site-specific MetalLB resources.
#   - --debug enables shell tracing and may print secrets; avoid it when
#     REGISTRY_PULL_SECRET is set unless logs are protected.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

AUTO_YES=false
SKIP_CORE=false
SKIP_REST=false
SKIP_FLOW=false
CORE_VALUES=""
METALLB_CONFIG=""
SITE_OVERLAY=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -y)             AUTO_YES=true  ;;
        --skip-core)    SKIP_CORE=true ;;
        --skip-rest)    SKIP_REST=true ;;
        --skip-flow)    SKIP_FLOW=true ;;
        --debug)        set -x         ;;
        --core-values)
            [[ -z "${2:-}" ]] && { echo "Error: --core-values requires a file path"; exit 1; }
            CORE_VALUES="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
            [[ ! -f "${CORE_VALUES}" ]] && { echo "Error: --core-values file not found: $2"; exit 1; }
            shift ;;
        --metallb-config)
            [[ -z "${2:-}" ]] && { echo "Error: --metallb-config requires a file or directory path"; exit 1; }
            METALLB_CONFIG="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
            [[ ! -e "${METALLB_CONFIG}" ]] && { echo "Error: --metallb-config path not found: $2"; exit 1; }
            shift ;;
        --site-overlay)
            [[ -z "${2:-}" ]] && { echo "Error: --site-overlay requires a kustomize directory path"; exit 1; }
            SITE_OVERLAY="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
            [[ ! -d "${SITE_OVERLAY}" ]] && { echo "Error: --site-overlay directory not found: $2"; exit 1; }
            shift ;;
        *) echo "Usage: $0 [-y] [--skip-core] [--skip-rest] [--skip-flow] [--core-values <file>] [--metallb-config <file-or-dir>] [--site-overlay <dir>] [--debug]"; exit 1 ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Pre-flight checks — env vars, tools, config files. Resolves NICO_REST_DIR
# (in-tree rest-api/) and NICO_REST_HELM_DIR (in-tree helm/rest/). Exits 1 if
# user declines to continue.
# ---------------------------------------------------------------------------
export AUTO_YES SKIP_CORE SKIP_REST SKIP_FLOW
# shellcheck source=preflight.sh
source "${SCRIPT_DIR}/preflight.sh"

VAULT_NS="${VAULT_NS:-vault}"
CERT_MANAGER_NS="${CERT_MANAGER_NS:-cert-manager}"
NICO_MANAGE_DEFAULT_STORAGE_CLASS="${NICO_MANAGE_DEFAULT_STORAGE_CLASS:-true}"
NICO_STORAGE_CLASS="${NICO_STORAGE_CLASS:-local-path-persistent}"

# ---------------------------------------------------------------------------
# Failure handler — offer to run clean.sh if setup exits with an error.
# Registered AFTER preflight so preflight aborts don't trigger it.
# ---------------------------------------------------------------------------
_SETUP_PHASE="initializing"

_on_failure() {
    local _rc=$?
    local _cmd="${BASH_COMMAND}"
    [[ ${_rc} -eq 0 ]] && return              # clean exit — nothing to do
    [[ "${_SETUP_PHASE}" == "complete" ]] && return  # finished successfully

    echo ""
    echo "========================================================================="
    echo "  SETUP FAILED"
    echo "  Phase   : ${_SETUP_PHASE}"
    echo "  Command : ${_cmd}"
    echo "  Code    : ${_rc}"
    echo "========================================================================="
    echo ""
    echo "  The cluster may be in a partially installed state."
    echo "  clean.sh will remove all resources installed by this run and"
    echo "  return the cluster to a clean state."
    echo ""
    # Prompt only when this process can actually read from the controlling TTY.
    if ! { exec 3</dev/tty; } 2>/dev/null; then
        echo "  No interactive TTY — skipping cleanup prompt. To clean up manually:"
        echo "    ${SCRIPT_DIR}/clean.sh"
        return
    fi
    if ! read -r -p "  ➤  Run clean.sh to revert the cluster now? [y/N] " _clean_reply <&3; then
        exec 3<&-
        echo ""
        echo "  No interactive response — skipping cleanup prompt. To clean up manually:"
        echo "    ${SCRIPT_DIR}/clean.sh"
        return
    fi
    exec 3<&-
    echo ""
    if [[ "${_clean_reply:-N}" =~ ^[Yy]$ ]]; then
        echo "  Running clean.sh..."
        "${SCRIPT_DIR}/clean.sh" || true
        echo ""
        echo "  Cleanup complete. Fix the issue above and re-run setup.sh."
    else
        echo "  Skipped. To clean up manually:"
        echo "    ${SCRIPT_DIR}/clean.sh"
    fi
}
trap '_on_failure' EXIT

# ---------------------------------------------------------------------------
# Ensure helmfile is installed
# ---------------------------------------------------------------------------
if ! command -v helmfile &>/dev/null; then
    echo "helmfile not found — installing..."
    if command -v brew &>/dev/null; then
        brew install helmfile
    else
        # Download the latest release binary for Linux
        HELMFILE_VERSION="$(curl -fsSL https://api.github.com/repos/helmfile/helmfile/releases/latest \
            | grep '"tag_name"' | sed 's/.*"tag_name": *"v\([^"]*\)".*/\1/')"
        ARCH="$(uname -m)"
        [[ "${ARCH}" == "x86_64" ]] && ARCH="amd64"
        [[ "${ARCH}" == "aarch64" ]] && ARCH="arm64"
        curl -fsSL "https://github.com/helmfile/helmfile/releases/download/v${HELMFILE_VERSION}/helmfile_${HELMFILE_VERSION}_linux_${ARCH}.tar.gz" \
            | tar -xz -C /usr/local/bin helmfile
        chmod +x /usr/local/bin/helmfile
    fi
    echo "helmfile $(helmfile --version) installed"
fi

# ---------------------------------------------------------------------------
# DNS check — verify cluster DNS is working before proceeding.
#
# Two supported setups:
#   Kubespray clusters: NodeLocal DNSCache DaemonSet (nodelocaldns) in kube-system.
#                       The ConfigMap and ServiceAccount are created by Kubespray;
#                       this script deploys the DaemonSet if it is missing.
#   kubeadm / other:   CoreDNS Deployment in kube-system. NodeLocal DNSCache is
#                       not used — we just verify CoreDNS pods are ready.
#
# We detect which setup is present by checking for the Kubespray-created
# ConfigMap (nodelocaldns). If absent, we skip the nodelocaldns DaemonSet
# entirely and check CoreDNS instead.
# ---------------------------------------------------------------------------
_SETUP_PHASE="cluster DNS check"
echo "=== Checking cluster DNS ==="

if kubectl get configmap nodelocaldns -n kube-system &>/dev/null; then
    # Kubespray cluster — NodeLocal DNSCache is expected
    NODEDNS_READY="$(kubectl get daemonset nodelocaldns -n kube-system \
        -o jsonpath='{.status.numberReady}' 2>/dev/null || echo "0")"
    NODEDNS_DESIRED="$(kubectl get daemonset nodelocaldns -n kube-system \
        -o jsonpath='{.status.desiredNumberScheduled}' 2>/dev/null || echo "-1")"

    if [[ "${NODEDNS_READY}" == "${NODEDNS_DESIRED}" && \
          "${NODEDNS_DESIRED}" != "0" && "${NODEDNS_DESIRED}" != "-1" ]]; then
        echo "DNS OK — nodelocaldns ${NODEDNS_READY}/${NODEDNS_DESIRED} ready"
    else
        echo "NodeLocal DNSCache not ready (${NODEDNS_READY}/${NODEDNS_DESIRED}) — deploying DaemonSet..."
        # apply may fail with "selector immutable" if DaemonSet already exists
        kubectl apply -f operators/nodelocaldns-daemonset.yaml 2>/dev/null || true
        kubectl rollout status daemonset/nodelocaldns -n kube-system --timeout=120s
        echo "NodeLocal DNSCache ready — waiting 10s for iptables to converge..."
        sleep 10
    fi
else
    # kubeadm or other cluster — check CoreDNS instead
    COREDNS_READY="$(kubectl get deployment coredns -n kube-system \
        -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")"
    COREDNS_DESIRED="$(kubectl get deployment coredns -n kube-system \
        -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "0")"

    if [[ "${COREDNS_READY}" -ge 1 ]]; then
        echo "DNS OK — CoreDNS ${COREDNS_READY}/${COREDNS_DESIRED} ready (nodelocaldns not present, skipping)"
    else
        echo "WARNING: CoreDNS is not ready (${COREDNS_READY}/${COREDNS_DESIRED}) — DNS resolution may fail"
        echo "  Check CoreDNS pods: kubectl get pods -n kube-system -l k8s-app=kube-dns"
        echo "  Continuing — some later steps may fail if DNS is broken"
    fi
fi

# ---------------------------------------------------------------------------
# 1. local-path-provisioner (no Helm chart — raw manifest)
# ---------------------------------------------------------------------------
_SETUP_PHASE="[1/6] local-path-provisioner"
echo "=== [1/6] local-path-provisioner ==="
kubectl apply -f operators/local-path-provisioner.yaml
# StorageClass provisioner is immutable — delete before apply so a stale
# provisioner from a previous install doesn't block the update.
kubectl delete -f operators/storageclass-local-path-persistent.yaml \
    --ignore-not-found 2>/dev/null || true
kubectl apply -f operators/storageclass-local-path-persistent.yaml
kubectl rollout status deployment/local-path-provisioner -n local-path-storage --timeout=120s
if [[ "${NICO_MANAGE_DEFAULT_STORAGE_CLASS}" == "true" ]]; then
    # Mark local-path as the cluster default StorageClass so workloads that
    # don't specify one (e.g. NICo REST postgres, Temporal) get a valid
    # provisioner.
    kubectl annotate storageclass local-path \
        storageclass.kubernetes.io/is-default-class=true --overwrite
else
    echo "NICO_MANAGE_DEFAULT_STORAGE_CLASS=${NICO_MANAGE_DEFAULT_STORAGE_CLASS}; leaving default StorageClass unchanged"
fi

# ---------------------------------------------------------------------------
# 1b. postgres-operator — Zalando operator must be up (CRD registered) before
#     the NICo prereqs chart creates the postgresql resource in Phase 5.
#     No TLS dependency — install early.
# ---------------------------------------------------------------------------
_SETUP_PHASE="[1b] postgres-operator"
echo "=== [1b] postgres-operator ==="
helmfile sync -l name=postgres-operator

# ---------------------------------------------------------------------------
# 1c. MetalLB — LoadBalancer service provider (BGP or L2 mode).
#     No TLS/PKI dependency — installed early so it is ready before NICo Core
#     deploys LoadBalancer services (NICo Core API, dhcp, dns, pxe, ssh-console-rs).
#
#     After the helm release installs the CRDs, site-specific config is applied
#     from --metallb-config <path> (file or kustomize dir) if provided, otherwise
#     from values/metallb-config.yaml. Fill in that file or pass --metallb-config.
# ---------------------------------------------------------------------------
_SETUP_PHASE="[1c] MetalLB"
echo "=== [1c] MetalLB ==="

helmfile sync -l name=metallb

echo "Waiting for MetalLB controller to be ready..."
kubectl wait --for=condition=Available deployment/metallb-controller \
    -n metallb-system --timeout=120s

echo "Applying MetalLB site config (IPAddressPool, BGPPeer, BGPAdvertisement)..."
if [[ -n "${METALLB_CONFIG}" ]]; then
    if [[ -d "${METALLB_CONFIG}" ]]; then
        kubectl apply -k "${METALLB_CONFIG}"
    else
        kubectl apply -f "${METALLB_CONFIG}"
    fi
else
    kubectl apply -f "${SCRIPT_DIR}/values/metallb-config.yaml"
fi
echo "MetalLB ready"

# ---------------------------------------------------------------------------
# 2. cert-manager + Prometheus CRDs + Vault TLS bootstrap
#    cert-manager must be up before we can issue certs for vault.
#    Vault pods need TLS secrets (nicoca-vault-client, vault-raft-tls)
#    BEFORE vault starts — so bootstrap them here via cert-manager.
# ---------------------------------------------------------------------------
_SETUP_PHASE="[2/6] cert-manager + Vault TLS bootstrap"
echo "=== [2/6] cert-manager + Vault TLS bootstrap ==="
helmfile sync -l name=cert-manager

kubectl apply --server-side -f operators/crds/ \
    --field-manager=helmfile --force-conflicts

kubectl create namespace "${VAULT_NS}" 2>/dev/null || true
helm template nico-prereqs . \
    --namespace nico-system \
    --set imagePullSecrets.ngcNicoPull="${REGISTRY_PULL_SECRET:-}" \
    --show-only templates/site-root-certificate.yaml \
    --show-only templates/vault-tls-certs.yaml \
    | kubectl apply --server-side --field-manager=helm -f -

kubectl wait --for=condition=Ready certificate/site-root \
    -n "${CERT_MANAGER_NS}" --timeout=120s
kubectl wait --for=condition=Ready certificate/nicoca-vault-client \
    -n "${VAULT_NS}" --timeout=120s
kubectl wait --for=condition=Ready certificate/vault-raft-tls \
    -n "${VAULT_NS}" --timeout=120s
echo "Vault TLS bootstrap complete"

# ---------------------------------------------------------------------------
# 3. vault — TLS secrets exist, pods can start
# ---------------------------------------------------------------------------
_SETUP_PHASE="[3/6] vault install"
echo "=== [3/6] vault ==="
helmfile sync -l name=vault \
    --set server.dataStorage.storageClass="${NICO_STORAGE_CLASS}" \
    --set server.auditStorage.storageClass="${NICO_STORAGE_CLASS}"

# ---------------------------------------------------------------------------
# 4. Initialize + unseal vault
#    Also sets up nico-system namespace (Helm labels + ssh-host-key)
#    so the NICo prereqs helm install can adopt it.
# ---------------------------------------------------------------------------
_SETUP_PHASE="[4/6] vault init + unseal"
echo "=== [4/6] unseal vault ==="
./unseal_vault.sh
./bootstrap_ssh_host_key.sh

# ---------------------------------------------------------------------------
# 5. external-secrets + NICo prereqs
# ---------------------------------------------------------------------------
_SETUP_PHASE="[5/6] external-secrets + NICo prereqs"
echo "=== [5/6] external-secrets + NICo prereqs ==="
helmfile sync -l name=external-secrets
helmfile sync -l name=nico-prereqs

# ---------------------------------------------------------------------------
# Wait for postgres-operator to provision the cluster and ESO to sync creds
# before NICo Core starts (the NICo Core API needs the DB credentials Secret).
# ---------------------------------------------------------------------------
echo "Waiting for nico-pg-cluster to reach Running state..."
until kubectl get postgresql nico-pg-cluster -n postgres \
    -o jsonpath='{.status.PostgresClusterStatus}' 2>/dev/null | grep -q "Running"; do
    STATUS="$(kubectl get postgresql nico-pg-cluster -n postgres \
        -o jsonpath='{.status.PostgresClusterStatus}' 2>/dev/null || echo 'unknown')"
    echo "  nico-pg-cluster status: ${STATUS} — retrying in 10s..."
    sleep 10
done
echo "nico-pg-cluster is Running"

# Install pg_trgm on nico_rest (needed by the nico-rest-db GIN index migration).
# Zalando's preparedDatabases conflicts with the databases section, so we install
# the extension directly after the cluster is ready. Idempotent: IF NOT EXISTS.
# Wait up to 120s for the Zalando operator to create the nico_rest database.
_pg_trgm_installed=false
for _pg_i in $(seq 1 24); do
    _PG_PRIMARY="$(kubectl get pods -n postgres -l application=spilo \
        -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.labels.spilo-role}{"\n"}{end}' \
        2>/dev/null | awk '$2=="master"{print $1}' | head -1)"
    if [[ -z "${_PG_PRIMARY}" ]]; then
        echo "  pg_trgm: no Patroni primary yet (${_pg_i}/24) — retrying in 5s..."
        sleep 5
        continue
    fi
    if kubectl exec -n postgres "${_PG_PRIMARY}" -- \
        su postgres -c "psql -d nico_rest -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;'" \
        2>/dev/null; then
        echo "pg_trgm ready"
        _pg_trgm_installed=true
        break
    fi
    echo "  pg_trgm: nico_rest not yet created by operator (${_pg_i}/24) — retrying in 5s..."
    sleep 5
done
if [[ "${_pg_trgm_installed}" == "false" ]]; then
    echo "  pg_trgm: nico_rest unavailable after 120s."
    echo "    → If rest.enabled=false in nico-prereqs, the nico_rest database is not created — this is expected."
    echo "    → If rest.enabled=true, the nico-rest-db migration will fail on the GIN index step."
fi

echo "Waiting for DB credentials to be synced by ESO..."
until kubectl get secret nico-system.nico.nico-pg-cluster.credentials \
    -n nico-system &>/dev/null; do
    echo "  credentials not yet synced — retrying in 5s..."
    sleep 5
done
echo "DB credentials ready"

echo "Waiting for Vault AppRole credentials to be synced by ESO..."
until ROLE_ID_B64="$(kubectl get secret nico-vault-approle-tokens \
        -n nico-system -o jsonpath='{.data.VAULT_ROLE_ID}' 2>/dev/null)" && \
      SECRET_ID_B64="$(kubectl get secret nico-vault-approle-tokens \
        -n nico-system -o jsonpath='{.data.VAULT_SECRET_ID}' 2>/dev/null)" && \
      [[ -n "${ROLE_ID_B64}" && -n "${SECRET_ID_B64}" ]]; do
    echo "  AppRole credentials not yet synced — retrying in 5s..."
    sleep 5
done
echo "Vault AppRole credentials ready"

if ! "${SKIP_CORE}"; then
    # Create imagepullsecret in nico-system so the API migrate hook can pull its
    # image. The hook runs before chart resources are created, so this must exist
    # before helm install — not as a post-install manual step.
    # Skipped when REGISTRY_PULL_SECRET is unset (air-gapped / pre-loaded registry).
    if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
        _registry_server="${NICO_IMAGE_REGISTRY%%/*}"
        echo "Creating imagepullsecret in nico-system (server: ${_registry_server})..."
        kubectl create secret docker-registry imagepullsecret \
            --namespace nico-system \
            --docker-server="${_registry_server}" \
            --docker-username="${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
            --docker-password="${REGISTRY_PULL_SECRET}" \
            --dry-run=client -o yaml | kubectl apply -f -
    else
        echo "REGISTRY_PULL_SECRET not set — skipping imagepullsecret creation (air-gapped or pre-loaded registry)."
    fi
fi

# ---------------------------------------------------------------------------
# NICo Core
# ---------------------------------------------------------------------------
if "${SKIP_CORE}"; then
    echo "=== [6/6] NICo Core ==="
    echo "Skipped (--skip-core flag set)."
else
    _CORE_VALUES_FILE="${CORE_VALUES:-${SCRIPT_DIR}/values/nico-core.yaml}"
    _CORE_VALUES_ARG="${CORE_VALUES:-helm-prereqs/values/nico-core.yaml}"

    NICO_CORE_CMD=(
        helm upgrade --install nico ./helm
        --namespace nico-system
        -f "${_CORE_VALUES_ARG}"
        --set-string "global.image.repository=${NICO_IMAGE_REGISTRY}/${NICO_CORE_IMAGE_NAME}"
        --set-string "global.image.tag=${NICO_CORE_IMAGE_TAG}"
        --timeout 600s --wait
    )
    _NICO_CORE_CMD_DISPLAY=""
    for _arg in "${NICO_CORE_CMD[@]}"; do
        printf -v _quoted_arg '%q' "${_arg}"
        _NICO_CORE_CMD_DISPLAY="${_NICO_CORE_CMD_DISPLAY}${_NICO_CORE_CMD_DISPLAY:+ }${_quoted_arg}"
    done

    # Warn if nico-core.yaml still contains example placeholder values.
    if [[ -z "${CORE_VALUES}" ]] && \
       grep -q "api-examplesite.example.com\|sitename = \"examplesite\"\|examplesite.example.com" \
            "${SCRIPT_DIR}/values/nico-core.yaml" 2>/dev/null; then
        echo "WARNING: values/nico-core.yaml still contains example placeholder values."
        echo "  Update nico-api.hostname, sitename, initial_domain_name, dhcp_servers,"
        echo "  site_fabric_prefixes, deny_prefixes, pools, and networks for your site."
        echo "  Or use --core-values /path/to/your-site-values.yaml to skip nico-core.yaml."
        echo ""
    fi

    # Warn if the DPU compatibility .forge zone isn't being served. Existing
    # DPU agent binaries are hardcoded to resolve carbide-pxe.forge,
    # carbide-ntp.forge, etc. Either the built-in unbound chart serves them
    # (enabled + localData populated with the .forge hostnames) or external
    # DNS has to. See helm-prereqs/README.md → "DPU compatibility DNS
    # (.forge zone)".
    if [[ -z "${CORE_VALUES}" ]] && \
       ! grep -qE "^[[:space:]]*-[[:space:]]*name:[[:space:]]*[a-z-]+\.forge" \
            "${SCRIPT_DIR}/values/nico-core.yaml" 2>/dev/null; then
        echo "WARNING: no DPU compatibility .forge zone configured in values/nico-core.yaml."
        echo "  DPU agents will fail to resolve carbide-pxe.forge / carbide-ntp.forge /"
        echo "  carbide-api.forge unless your external DNS already serves those names."
        echo "  To use the built-in unbound chart instead, enable unbound and uncomment"
        echo "  the localData example in values/nico-core.yaml (under the unbound block)."
        echo "  See helm-prereqs/README.md → \"DPU compatibility DNS (.forge zone)\"."
        echo ""
    fi

    echo ""
    echo "========================================================================="
    echo "  ACTION REQUIRED: Before deploying NICo Core, confirm you have updated:"
    echo "    ${_CORE_VALUES_FILE}"
    echo ""
    echo "  Key fields:"
    echo "    global.image.repository   — ${NICO_IMAGE_REGISTRY}/${NICO_CORE_IMAGE_NAME}"
    echo "    global.image.tag          — ${NICO_CORE_IMAGE_TAG}"
    echo "    nico-api.hostname      — your site hostname"
    echo "    nico-api.siteConfig    — site-specific network/pool/IB config"
    echo "========================================================================="
    echo ""
    if "${AUTO_YES}"; then
        _reply="Y"
    else
        read -r -p "  ➤  Deploy NICo Core now? [Y/n] " _reply
        echo ""
    fi
    if [[ "${_reply:-Y}" =~ ^[Yy]$ ]]; then
        _SETUP_PHASE="[6/6] NICo Core"
        echo "=== [6/6] NICo Core ==="
        (cd "${SCRIPT_DIR}/.." && "${NICO_CORE_CMD[@]}")
    else
        echo "Skipped. To deploy manually, run from $(dirname "${SCRIPT_DIR}"):"
        echo "  ${_NICO_CORE_CMD_DISPLAY}"
    fi
fi

# ---------------------------------------------------------------------------
# Site kustomize overlay — applies site-specific resources that are not
# managed by the NICo Helm chart (e.g. per-pod LoadBalancer Services,
# additional StatefulSets, or supplemental MetalLB config). Idempotent.
# ---------------------------------------------------------------------------
if [[ -n "${SITE_OVERLAY}" ]]; then
    _SETUP_PHASE="site overlay"
    echo "=== Site overlay: $(basename "${SITE_OVERLAY}") ==="
    kubectl apply -k "${SITE_OVERLAY}"
    echo "Site overlay applied"
fi

# ---------------------------------------------------------------------------
# 7. NICo REST full stack
#    Order of operations:
#      7a. Resolve NICo REST repo + CA signing secret
#      7b. NICo REST CA issuer ClusterIssuer (cert-manager.io)
#      7c. NICo REST postgres (simple StatefulSet — temporal + forge DBs)
#      7d. Keycloak (dev IdP)
#      7e. Temporal namespace + TLS certs (issued by the NICo REST CA issuer)
#      7f. Temporal helm chart
#      7g. NICo REST helm chart (API, cert-manager, workflow, site-manager)
# ---------------------------------------------------------------------------
echo ""
_SETUP_PHASE="[7/7] NICo REST"
echo "=== [7/7] NICo REST ==="

if "${SKIP_REST}"; then
    echo "Skipped (--skip-rest flag set)."
    echo ""
    echo "=== Setup complete (NICo REST skipped) ==="
    _SETUP_PHASE="complete"
    exit 0
fi

# --- 7a. NICo REST source tree and Helm charts (in-tree) -------------------------
# preflight.sh resolves and validates rest-api/ into NICO_REST_DIR and
# helm/rest/ into NICO_REST_HELM_DIR.
# If it didn't, preflight already errored out — guard the consumer side too in
# case someone sources setup.sh without going through preflight.
if [[ -z "${NICO_REST_DIR:-}" ]]; then
    echo "ERROR: NICO_REST_DIR is unset — preflight didn't resolve rest-api/. Make sure your checkout contains rest-api/."
    exit 1
fi
if [[ -z "${NICO_REST_HELM_DIR:-}" ]]; then
    echo "ERROR: NICO_REST_HELM_DIR is unset — preflight didn't resolve helm/rest/. Make sure your checkout contains helm/rest/nico-rest and helm/rest/nico-rest-site-agent."
    exit 1
fi
echo "NICo REST source: ${NICO_REST_DIR}"
echo "NICo REST charts: ${NICO_REST_HELM_DIR}"

# Create NICo REST namespace
kubectl create namespace nico-rest 2>/dev/null || true

# CA signing secret — needed by the NICo REST cert-manager component (internal PKI)
# and the cert-manager.io ClusterIssuer. gen-site-ca.sh creates it in
# both the NICo REST and cert-manager namespaces in one shot.
if kubectl get secret ca-signing-secret -n nico-rest &>/dev/null; then
    echo "ca-signing-secret already present — skipping CA generation"
else
    echo "Generating NICo REST CA signing secret..."
    (cd "${NICO_REST_DIR}" && ./scripts/gen-site-ca.sh)
fi

# --- 7b. ClusterIssuer -------------------------------------------------------
_SETUP_PHASE="[7b/7] NICo REST CA issuer ClusterIssuer"
echo "=== [7b/7] NICo REST CA issuer ClusterIssuer ==="
(cd "${NICO_REST_DIR}" && kubectl apply -k deploy/kustomize/base/cert-manager-io)

# --- 7c. NICo REST postgres --------------------------------------------------------
# Simple postgres StatefulSet with all NICo databases pre-initialised:
# forge, temporal, temporal_visibility, keycloak.
# Lives alongside nico-pg-cluster in the postgres namespace — different
# service name ("postgres") so Temporal and NICo values work without changes.
_SETUP_PHASE="[7c/7] NICo REST postgres"
echo "=== [7c/7] NICo REST postgres ==="
(cd "${NICO_REST_DIR}" && kubectl apply -k deploy/kustomize/base/postgres)
kubectl rollout status statefulset/postgres -n postgres --timeout=180s
echo "NICo REST postgres ready"

# --- 7d. Keycloak (conditional) -----------------------------------------------
# Only deploy Keycloak if nico-rest.yaml has keycloak.enabled: true.
# If using external OAuth2/OIDC (Option B in nico-rest.yaml), skip this step.
# Dev OIDC IdP, pre-loaded with the configured NICo development realm + test users.
# nico-rest-api talks to it at http://keycloak.nico-rest:8082
_SETUP_PHASE="[7d/7] Keycloak"
_KC_ENABLED="$(grep -A5 'keycloak:' "${SCRIPT_DIR}/values/nico-rest.yaml" \
    | grep 'enabled:' | head -1 | awk '{print $2}' || echo "false")"

if [[ "${_KC_ENABLED}" == "true" ]]; then
    echo "=== [7d/7] Keycloak ==="
    "${SCRIPT_DIR}/keycloak/setup.sh"
    echo "Keycloak ready"
else
    echo "=== [7d/7] Keycloak — skipped (keycloak.enabled is not true in nico-rest.yaml) ==="
fi

# --- 7e. Temporal namespace + TLS certs + db-creds --------------------------
_SETUP_PHASE="[7e/7] Temporal TLS bootstrap"
echo "=== [7e/7] Temporal TLS bootstrap ==="
(cd "${NICO_REST_DIR}" && kubectl apply -f deploy/kustomize/base/temporal-helm/namespace.yaml)
(cd "${NICO_REST_DIR}" && kubectl apply -f deploy/kustomize/base/temporal-helm/db-creds.yaml)
(cd "${NICO_REST_DIR}" && kubectl apply -f deploy/kustomize/base/temporal-helm/certificates.yaml)

echo "Waiting for temporal TLS certificates to be issued..."
kubectl wait --for=condition=Ready certificate/server-interservice-cert \
    -n temporal --timeout=120s
kubectl wait --for=condition=Ready certificate/server-cloud-cert \
    -n temporal --timeout=120s
kubectl wait --for=condition=Ready certificate/server-site-cert \
    -n temporal --timeout=120s
echo "Temporal TLS certs ready"

# --- 7f. Temporal ------------------------------------------------------------
_SETUP_PHASE="[7f/7] Temporal"
echo "=== [7f/7] Temporal ==="
helm upgrade --install temporal "${NICO_REST_DIR}/temporal-helm/temporal" \
    --namespace temporal \
    -f "${NICO_REST_DIR}/temporal-helm/temporal/values-kind.yaml" \
    --timeout 300s --wait
echo "Temporal ready"

# Create the Temporal namespaces required by NICo REST workers (requires mTLS)
echo "Creating Temporal cloud and site namespaces..."
_TEMPORAL_ADDR="temporal-frontend.temporal:7233"
_TEMPORAL_TLS="--tls-cert-path /var/secrets/temporal/certs/server-interservice/tls.crt \
    --tls-key-path /var/secrets/temporal/certs/server-interservice/tls.key \
    --tls-ca-path /var/secrets/temporal/certs/server-interservice/ca.crt \
    --tls-server-name interservice.server.temporal.local"
_wait_for_temporal() {
    local _output=""

    echo "Waiting for Temporal frontend and admin tools..."
    kubectl rollout status deploy/temporal-frontend -n temporal --timeout=120s
    kubectl rollout status deploy/temporal-admintools -n temporal --timeout=120s

    for _i in $(seq 1 24); do
        if _output="$(kubectl exec -n temporal deploy/temporal-admintools -- \
            sh -c "temporal operator namespace list --address ${_TEMPORAL_ADDR} ${_TEMPORAL_TLS}" 2>&1)"; then
            echo "Temporal frontend ready"
            return
        fi
        echo "  Waiting for Temporal API (${_i}/24)..."
        sleep 5
    done

    echo "ERROR: Temporal frontend is not ready for namespace operations" >&2
    echo "${_output}" >&2
    exit 1
}

_create_temporal_namespace() {
    local _namespace="$1"
    local _output

    if _output="$(kubectl exec -n temporal deploy/temporal-admintools -- \
        sh -c "temporal operator namespace create -n \"\$1\" --retention 72h --address ${_TEMPORAL_ADDR} ${_TEMPORAL_TLS}" \
        sh "${_namespace}" 2>&1)"; then
        echo "Temporal namespace ${_namespace} ready"
        return
    fi

    if printf "%s" "${_output}" | grep -qi "already exists"; then
        echo "Temporal namespace ${_namespace} already exists"
        return
    fi

    echo "ERROR: failed to create Temporal namespace ${_namespace}" >&2
    echo "${_output}" >&2
    exit 1
}

_verify_temporal_namespaces() {
    local _output
    local _missing=()
    local _namespace

    if ! _output="$(kubectl exec -n temporal deploy/temporal-admintools -- \
        sh -c "temporal operator namespace list --address ${_TEMPORAL_ADDR} ${_TEMPORAL_TLS}" 2>&1)"; then
        echo "ERROR: failed to list Temporal namespaces" >&2
        echo "${_output}" >&2
        exit 1
    fi

    for _namespace in "$@"; do
        if ! printf "%s" "${_output}" | grep -Eq "(^|[^[:alnum:]_-])${_namespace}([^[:alnum:]_-]|$)"; then
            _missing+=("${_namespace}")
        fi
    done

    if [[ ${#_missing[@]} -gt 0 ]]; then
        echo "ERROR: missing Temporal namespace(s): ${_missing[*]}" >&2
        echo "${_output}" >&2
        exit 1
    fi

    echo "Verified Temporal namespaces: $*"
}

_wait_for_temporal
_create_temporal_namespace cloud
_create_temporal_namespace site
# flow Temporal namespace — required by NICo Flow workers; pod panics on startup if absent.
_create_temporal_namespace flow
_verify_temporal_namespaces cloud site flow
echo "Temporal namespaces ready"

_SETUP_PHASE="[7g/7] NICo REST helm chart"
# --- 7g. NICo REST helm chart -------------------------------------------------
# Wait for ESO to sync the Zalando-generated REST DB credentials into nico-rest.
# nico-rest-db-eso ClusterExternalSecret creates nico-rest-pg-creds once the
# nico-rest namespace (created in 7a) is visible to ESO. The nico-rest-db
# pre-install hook will fail immediately if the secret is missing.
echo "Waiting for REST DB credentials to be synced by ESO (nico-rest-pg-creds in nico-rest)..."
for _rdc_i in $(seq 1 24); do
    if kubectl get secret nico-rest-pg-creds -n nico-rest &>/dev/null; then
        break
    fi
    if [[ "${_rdc_i}" -eq 24 ]]; then
        echo "ERROR: nico-rest-pg-creds not synced after 120s." >&2
        echo "  Check: kubectl describe clusterexternalsecret nico-rest-db-eso" >&2
        echo "  Ensure the nico-rest namespace exists and rest.enabled=true in nico-prereqs." >&2
        exit 1
    fi
    echo "  nico-rest-pg-creds not yet synced (${_rdc_i}/24) — retrying in 5s..."
    sleep 5
done
echo "REST DB credentials ready"

# Write credentials to a temp file rather than --set so they are not visible
# in process arguments and are not subject to Helm's --set special-char escaping.
_NICO_REST_CREDS_FILE="$(mktemp)"
chmod 600 "${_NICO_REST_CREDS_FILE}"
printf 'nico-rest-common:\n  secrets:\n    dbCreds:\n      username: "%s"\n      password: "%s"\n' \
    "$(kubectl get secret nico-rest-pg-creds -n nico-rest -o jsonpath='{.data.username}' | base64 -d)" \
    "$(kubectl get secret nico-rest-pg-creds -n nico-rest -o jsonpath='{.data.password}' | base64 -d)" \
    > "${_NICO_REST_CREDS_FILE}"
# The workflow workers missed the nico-pg-cluster consolidation (#3081): the
# subchart defaults still point at the legacy postgres.postgres/nico database
# (zero tables), so every DB activity fails with SQLSTATE 42P01 (relation
# "site" does not exist) and no site can ever leave Pending. Align the worker
# DB target with nico-rest-api at install time (password comes from the
# db-creds Secret the nico-rest-common hook creates from the values above).
printf 'nico-rest-workflow:\n  secrets:\n    dbCreds: "db-creds"\n  config:\n    db:\n      host: "nico-pg-cluster.postgres.svc.cluster.local"\n      name: "nico_rest"\n      user: "nico-rest.nico"\n' \
    >> "${_NICO_REST_CREDS_FILE}"

NICO_HELM_CHART="${NICO_REST_HELM_DIR}/nico-rest"
NICO_REST_CMD=(
    helm upgrade --install nico-rest "${NICO_HELM_CHART}"
    --namespace nico-rest
    -f "${SCRIPT_DIR}/values/nico-rest.yaml"
    -f "${_NICO_REST_CREDS_FILE}"
    --set global.image.repository="${NICO_IMAGE_REGISTRY}"
    --set global.image.tag="${NICO_REST_IMAGE_TAG}"
    --timeout 600s --wait
)

if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
    # Build dockerconfigjson for the image-pull-secret that the NICo REST common
    # chart creates. The registry host is derived from NICO_IMAGE_REGISTRY so this
    # works for nvcr.io and private non-NGC registries.
    _nico_registry_server="${NICO_IMAGE_REGISTRY%%/*}"
    _nico_docker_cfg="$(printf '{"auths":{"%s":{"username":"%s","password":"%s"}}}' \
        "${_nico_registry_server}" \
        "${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
        "${REGISTRY_PULL_SECRET}" | base64 | tr -d '\n')"
    NICO_REST_CMD+=(
        --set "nico-rest-common.secrets.imagePullSecret.dockerconfigjson=${_nico_docker_cfg}"
    )
else
    echo "REGISTRY_PULL_SECRET not set — omitting NICo REST image pull secret override."
    echo "NICo REST images must be public, preloaded, or configured with existing imagePullSecrets in values."
fi

echo ""
echo "========================================================================="
echo "  NICo REST"
echo "    Image:  ${NICO_IMAGE_REGISTRY}  tag: ${NICO_REST_IMAGE_TAG}"
echo "    Values: ${SCRIPT_DIR}/values/nico-rest.yaml"
echo "    Auth:   Keycloak dev instance (step 7d) — update nico-rest.yaml for production IdP"
echo "========================================================================="
echo ""
if "${AUTO_YES}"; then
    _nico_reply="Y"
else
    read -r -p "  ➤  Deploy NICo REST now? [Y/n] " _nico_reply
    echo ""
fi
if [[ "${_nico_reply:-Y}" =~ ^[Yy]$ ]]; then
    "${NICO_REST_CMD[@]}"
    rm -f "${_NICO_REST_CREDS_FILE}"
else
    rm -f "${_NICO_REST_CREDS_FILE}"
    echo "Skipped NICo REST. Re-run with -y or answer Y to deploy."
    echo ""
    echo "=== Setup complete (NICo REST skipped) ==="
    exit 0
fi

# --- 7h. NICo Flow ------------------------------------------------------------
# Flow is the rack lifecycle orchestrator (formerly RLA). Single pod with three
# containers — flow (50051), psm (50052), nsm (50053).  Runs in its own `flow`
# namespace.
#
# Runs BEFORE the site-agent (7i) so that flow.flow.svc.cluster.local:50051
# exists when the site-agent starts and attempts its Flow gRPC connection.
#
# Prerequisites already in place by this point:
#   - flow/psm/nsm databases on nico-pg-cluster (helm-prereqs postgresql.yaml)
#   - flow.nico/psm.nico/nsm.nico DB credentials synced via ESO into the flow
#     namespace by the flow-db-eso / psm-db-eso / nsm-db-eso ClusterExternalSecrets
#   - psm-vault-token and nsm-vault-token Secrets in the flow namespace
#     (provisioned by the flow-vault-tokens post-install hook)
#   - Temporal `flow` namespace (created in phase 7f above)
#   - nico-rest-ca-issuer ClusterIssuer (installed by phase 7b — issues the
#     temporal-client-certs)
#   - vault-nico-issuer ClusterIssuer (issues the SPIFFE cert)
#
# Same pre-apply-cert dance as the site-agent: render the Certificate(s) ahead
# of the helm install so cert-manager has time to issue them and the pod doesn't
# hit a FailedMount race on the spiffe / temporal-client-certs secrets.
if "${SKIP_FLOW}"; then
    echo "=== [7h/7] NICo Flow — skipped (--skip-flow) ==="
else
    _SETUP_PHASE="[7h/7] NICo Flow"
    echo "=== [7h/7] NICo Flow ==="

    NICO_FLOW_CHART="${SCRIPT_DIR}/../helm/charts/nico-flow"
    NICO_FLOW_NAMESPACE="flow"

    NICO_FLOW_ARGS=(
        --namespace "${NICO_FLOW_NAMESPACE}"
        --create-namespace
        --set "global.image.repository=${NICO_IMAGE_REGISTRY}"
        ## Flow (nico-flow / nico-psm / nico-nsm) ships on the same image release
        ## line as NICo REST — they're built and tagged together — so reuse
        ## NICO_REST_IMAGE_TAG, not NICO_CORE_IMAGE_TAG (which is carbide-api).
        --set "global.image.tag=${NICO_REST_IMAGE_TAG}"
    )

    # Render the dockerconfigjson for the chart-managed image-pull-secret. Same
    # pattern as the NICo REST common chart — keep the registry credential on
    # the helm command line so the chart template can install it as a
    # pre-install hook (pod can't pull from nvcr.io otherwise).
    if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
        _flow_registry_server="${NICO_IMAGE_REGISTRY%%/*}"
        _flow_docker_cfg="$(printf '{"auths":{"%s":{"username":"%s","password":"%s"}}}' \
            "${_flow_registry_server}" \
            "${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
            "${REGISTRY_PULL_SECRET}" | base64 | tr -d '\n')"
        NICO_FLOW_ARGS+=(
            --set "global.imagePullSecrets[0].name=image-pull-secret"
            --set "imagePullSecret.dockerconfigjson=${_flow_docker_cfg}"
        )
    fi

    # Pre-apply Certificates so cert-manager can issue secrets before the pod schedules.
    echo "Pre-applying flow Certificates (SPIFFE + Temporal client)..."
    helm template flow "${NICO_FLOW_CHART}" \
        "${NICO_FLOW_ARGS[@]}" \
        --show-only templates/namespace.yaml | kubectl apply -f -
    helm template flow "${NICO_FLOW_CHART}" \
        "${NICO_FLOW_ARGS[@]}" \
        --show-only templates/certificate.yaml | kubectl apply -f -
    kubectl annotate certificate/flow-certificate -n "${NICO_FLOW_NAMESPACE}" \
        "meta.helm.sh/release-name=flow" \
        "meta.helm.sh/release-namespace=${NICO_FLOW_NAMESPACE}" --overwrite
    kubectl annotate certificate/temporal-client-certs -n "${NICO_FLOW_NAMESPACE}" \
        "meta.helm.sh/release-name=flow" \
        "meta.helm.sh/release-namespace=${NICO_FLOW_NAMESPACE}" --overwrite
    kubectl label certificate/flow-certificate -n "${NICO_FLOW_NAMESPACE}" \
        "app.kubernetes.io/managed-by=Helm" --overwrite
    kubectl label certificate/temporal-client-certs -n "${NICO_FLOW_NAMESPACE}" \
        "app.kubernetes.io/managed-by=Helm" --overwrite

    # Annotate/label the namespace itself — the flow-vault-tokens-job (nico-prereqs
    # helm hook) creates this namespace ahead of the flow release. Without Helm
    # ownership metadata, helm install refuses to adopt it.
    kubectl annotate namespace "${NICO_FLOW_NAMESPACE}" \
        "meta.helm.sh/release-name=flow" \
        "meta.helm.sh/release-namespace=${NICO_FLOW_NAMESPACE}" --overwrite
    kubectl label namespace "${NICO_FLOW_NAMESPACE}" \
        "app.kubernetes.io/managed-by=Helm" --overwrite

    echo "Waiting for cert-manager to issue flow-certificate..."
    kubectl wait --for=condition=Ready certificate/flow-certificate \
        -n "${NICO_FLOW_NAMESPACE}" --timeout=120s
    echo "Waiting for cert-manager to issue temporal-client-certs..."
    kubectl wait --for=condition=Ready certificate/temporal-client-certs \
        -n "${NICO_FLOW_NAMESPACE}" --timeout=120s

    # Wait for the psm/nsm vault tokens and DB credential ESO syncs to land
    # (provisioned by helm-prereqs hooks; may still be in flight if nico-prereqs
    # was re-installed just before this phase). Fail-fast if any secret never
    # shows up — the alternative (silently falling through to helm install) is
    # 5 minutes of FailedMount-loop before helm gives up with an opaque message.
    _wait_for_secret() {
        local _name="$1"
        local _ns="$2"
        local _hint="$3"
        for _i in $(seq 1 24); do
            if kubectl get secret "${_name}" -n "${_ns}" >/dev/null 2>&1; then
                echo "  ${_name} ready"
                return 0
            fi
            echo "  Waiting for ${_name} (${_i}/24)..."
            sleep 5
        done
        echo "ERROR: Secret ${_name} did not appear in namespace ${_ns} within 120s."
        echo "  ${_hint}"
        return 1
    }

    echo "Waiting for psm/nsm Vault tokens..."
    for _s in psm-vault-token nsm-vault-token; do
        _wait_for_secret "${_s}" "${NICO_FLOW_NAMESPACE}" \
            "Provisioned by the flow-vault-tokens helm hook in nico-prereqs. Check 'kubectl logs -n nico-system job/flow-vault-tokens' and confirm helm-prereqs/values.yaml::flow.enabled=true."
    done

    echo "Waiting for flow/psm/nsm DB credentials..."
    for _s in flow.nico.nico-pg-cluster.credentials \
             psm.nico.nico-pg-cluster.credentials \
             nsm.nico.nico-pg-cluster.credentials; do
        _wait_for_secret "${_s}" "${NICO_FLOW_NAMESPACE}" \
            "Synced by the flow-db-eso/psm-db-eso/nsm-db-eso ClusterExternalSecrets in nico-prereqs. Check 'kubectl describe clusterexternalsecret -A | grep flow' and confirm helm-prereqs/values.yaml::flow.enabled=true."
    done

    echo "Installing flow helm chart..."
    helm upgrade --install flow "${NICO_FLOW_CHART}" \
        "${NICO_FLOW_ARGS[@]}" \
        --timeout 300s --wait
    echo "NICo Flow deployed"
fi

# --- 7i. NICo REST site-agent -------------------------------------------------
# The site-agent is a separate chart from the main NICo REST umbrella.
#
# Runs AFTER NICo Flow (7h) so that flow.flow.svc.cluster.local:50051 is
# reachable when the site-agent starts its Flow gRPC connection.
#
# Bootstrap order:
#   1. Create the per-site Temporal namespace BEFORE helm install so the
#      site-agent never starts without it (starting without it causes an
#      immediate nil-pointer panic in RegisterCron).
#   2. Install the chart with bootstrap.enabled=true — a pre-install Helm hook
#      Job (alpine/k8s) runs entirely inside the cluster:
#        a. Calls POST nico-rest-site-manager:8100/v1/site to register the site.
#        b. Waits for the Site CR OTP (populated by site-manager operator).
#        c. Creates site-registration secret with real UUID + OTP.
#      The StatefulSet pod is only created AFTER the hook completes, so there is
#      no FailedMount window. Do NOT pre-create the secret — that would trigger
#      the Job's idempotency check and skip the real bootstrap.
#
# The site-agent binary also needs DB credentials for its local elektratest DB.
# All of this is wired via --set flags so nico-rest.yaml stays registry-agnostic.
NICO_SITE_AGENT_CHART="${NICO_REST_HELM_DIR}/nico-rest-site-agent"

# ---------------------------------------------------------------------------
# Resolve the site UUID — the site-agent must only ever be bound to a UUID
# that a site record in the REST database backs, or its inventory is dropped
# and `nicocli site list` stays empty (the CR+OTP that the bootstrap Job
# creates via POST /v1/site is necessary but NOT sufficient).
# Resolution order:
#   1. explicit NICO_SITE_UUID           — bind to a pre-existing site
#   2. CLUSTER_ID of a prior install     — stable across reruns
#   3. existing REST site row by name    — adopt (idempotent reprovision)
#   4. mint a new UUID                   — the seed below creates its site row
# Then seed the REST DB directly (same record shape as forged's per-env
# envs/*/carbide-rest/site.sql: a 'default' infrastructure_provider for the
# org plus a Pending site row). DB row first, CR second — the bootstrap Job's
# existing POST /v1/site then creates the Site CR + OTP for the same UUID.
# IdP-agnostic: no API token needed.
# ---------------------------------------------------------------------------
NICO_ORG="${NICO_ORG:-ncx}"
# siteName may be bare, single- or double-quoted in YAML; strip either style.
NICO_SITE_NAME="${NICO_SITE_NAME:-$(awk '/^siteName:/{v=$2; gsub(/["'"'"']/,"",v); print v}' "${SCRIPT_DIR}/values.yaml" 2>/dev/null || true)}"
if [[ -z "${NICO_SITE_NAME}" ]]; then
    echo "ERROR: could not resolve the site name (set NICO_SITE_NAME or siteName in values.yaml)" >&2
    exit 1
fi
# These values are interpolated into SQL inside a double-quoted shell string —
# restrict them to a safe charset instead of attempting to escape.
if ! [[ "${NICO_SITE_NAME}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "${NICO_ORG}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    echo "ERROR: NICO_SITE_NAME/NICO_ORG must match [A-Za-z0-9][A-Za-z0-9._-]* (got '${NICO_SITE_NAME}' / '${NICO_ORG}')" >&2
    exit 1
fi

# || true: under set -euo pipefail a kubectl failure here would kill the
# script before the emptiness check that makes seeding optional.
_REST_PG_PRIMARY="$(kubectl get pods -n postgres -l application=spilo \
    -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.labels.spilo-role}{"\n"}{end}' \
    2>/dev/null | awk '$2=="master"{print $1}' | head -1 || true)"
_rest_sql() {   # runs SQL against the nico_rest DB on the Patroni primary
    kubectl exec -n postgres "${_REST_PG_PRIMARY}" -- \
        su postgres -c "psql -d nico_rest -v ON_ERROR_STOP=1 -tAc \"$1\"" 2>/dev/null
}

# CLUSTER_ID reaches the agent via envFrom -> the nico-rest-site-agent-config
# ConfigMap; it never appears as an inline env entry in the StatefulSet spec,
# so it must be read from the ConfigMap. Fetched once; reused by the
# stale-secret guard below.
_PRIOR_CLUSTER_ID="$(kubectl get configmap nico-rest-site-agent-config -n nico-rest \
    -o jsonpath='{.data.CLUSTER_ID}' 2>/dev/null || true)"

if [[ -z "${NICO_SITE_UUID:-}" ]]; then
    # 2. prior install's CLUSTER_ID (stable reruns)
    NICO_SITE_UUID="${_PRIOR_CLUSTER_ID}"
fi
if [[ -z "${NICO_SITE_UUID:-}" && -n "${_REST_PG_PRIMARY}" ]]; then
    # 3. adopt an existing site row with our name
    NICO_SITE_UUID="$(_rest_sql "SELECT id FROM site WHERE name='${NICO_SITE_NAME}' AND org='${NICO_ORG}' AND deleted IS NULL LIMIT 1;" || true)"
    [[ -n "${NICO_SITE_UUID}" ]] && echo "Adopting existing REST site '${NICO_SITE_NAME}' (${NICO_SITE_UUID})"
fi
if [[ -z "${NICO_SITE_UUID:-}" ]]; then
    # 4. mint — the seed below registers it
    if ! command -v python3 &>/dev/null; then
        echo "ERROR: NICO_SITE_UUID is unset and python3 is not available" >&2
        exit 1
    fi
    NICO_SITE_UUID="$(python3 -c 'import uuid; print(uuid.uuid4())')"
fi
# Validate before interpolating into SQL / --set (path 1 accepts arbitrary env).
if ! [[ "${NICO_SITE_UUID}" =~ ^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$ ]]; then
    echo "ERROR: resolved NICO_SITE_UUID is not a valid UUID: '${NICO_SITE_UUID}'" >&2
    exit 1
fi

# Seed the site record (idempotent): provider row per org, site row keyed by
# our UUID. Waits briefly for the REST migrations to have created the tables.
if [[ -n "${_REST_PG_PRIMARY}" ]]; then
    for _s_i in $(seq 1 24); do
        _rest_sql "SELECT 1 FROM site LIMIT 1;" >/dev/null 2>&1 && break
        [[ "${_s_i}" -eq 24 ]] && { echo "ERROR: REST 'site' table not present after 120s — did the nico-rest-db migrations run?" >&2; exit 1; }
        echo "  waiting for REST DB migrations (site table) (${_s_i}/24)..."
        sleep 5
    done
    _rest_sql "INSERT INTO infrastructure_provider (id, name, display_name, org, created, updated, created_by)
        SELECT gen_random_uuid(), 'default', 'default', '${NICO_ORG}', now(), now(), '${NICO_SITE_UUID}'
        WHERE NOT EXISTS (SELECT 1 FROM infrastructure_provider WHERE org='${NICO_ORG}' AND name='default' AND deleted IS NULL);" >/dev/null \
        || { echo "ERROR: failed to seed infrastructure_provider" >&2; exit 1; }
    _rest_sql "INSERT INTO site (id, name, display_name, org, infrastructure_provider_id, registration_token,
            registration_token_expiration, is_infinity_enabled, is_serial_console_enabled, status, created, updated, created_by, config)
        SELECT '${NICO_SITE_UUID}', '${NICO_SITE_NAME}', '${NICO_SITE_NAME}', '${NICO_ORG}',
            (SELECT id FROM infrastructure_provider WHERE org='${NICO_ORG}' AND name='default' AND deleted IS NULL LIMIT 1),
            gen_random_uuid(), now() + interval '7 days', false, false, 'Pending', now(), now(), '${NICO_SITE_UUID}',
            '{\\\"native_networking\\\": true, \\\"network_security_group\\\": true, \\\"flow\\\": true}'
        WHERE NOT EXISTS (SELECT 1 FROM site WHERE id='${NICO_SITE_UUID}');" >/dev/null \
        || { echo "ERROR: failed to seed the site record" >&2; exit 1; }
    # Parity with the REST create handler: it defaults native_networking and
    # network_security_group to true ("v2 networking posture", site.go) and
    # writes a status_detail row in the same transaction; endpoints surfacing
    # status details would otherwise return an empty array for a seeded site.
    _rest_sql "INSERT INTO status_detail (id, entity_id, status, message, count, created, updated)
        SELECT gen_random_uuid(), '${NICO_SITE_UUID}', 'Pending', 'received site creation request, pending pairing', 1, now(), now()
        WHERE NOT EXISTS (SELECT 1 FROM status_detail WHERE entity_id='${NICO_SITE_UUID}');" >/dev/null \
        || echo "WARNING: could not seed the site status_detail row (non-fatal)" >&2
    echo "REST site record ready: '${NICO_SITE_NAME}' (${NICO_SITE_UUID}, org ${NICO_ORG})"
else
    echo "WARNING: no Patroni primary found — skipping REST site seeding (site-agent inventory will be dropped until the site exists)" >&2
fi

# If a previous bootstrap bound the site-registration secret to a DIFFERENT
# UUID, delete it so the bootstrap Job re-registers under the resolved one
# instead of silently keeping the stale identity.
if kubectl get secret site-registration -n nico-rest &>/dev/null \
   && [[ -n "${_PRIOR_CLUSTER_ID}" && "${_PRIOR_CLUSTER_ID}" != "${NICO_SITE_UUID}" ]]; then
    echo "site-registration secret is bound to stale UUID ${_PRIOR_CLUSTER_ID} — deleting for re-bootstrap"
    kubectl delete secret site-registration -n nico-rest >/dev/null
fi

NICO_SITE_AGENT_ARGS=(
    --namespace nico-rest
    -f "${SCRIPT_DIR}/values/nico-site-agent.yaml"
    --set global.image.repository="${NICO_IMAGE_REGISTRY}"
    --set global.image.tag="${NICO_REST_IMAGE_TAG}"
)
if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
    NICO_SITE_AGENT_ARGS+=(
        --set "global.imagePullSecrets[0].name=image-pull-secret"
    )
fi

_SETUP_PHASE="[7i/7] NICo REST site-agent"
echo "=== [7i/7] NICo REST site-agent (site UUID: ${NICO_SITE_UUID}) ==="

# Pre-apply the Certificate resource so cert-manager issues the NICo gRPC client
# cert BEFORE the StatefulSet pod starts. Without this, there is a race: helm creates
# both the Certificate and the StatefulSet simultaneously, and the pod's
# GetInitialCertMD5() call fails because the secret hasn't been projected yet.
echo "Pre-applying NICo gRPC client certificate..."
# Issue the cert from vault-nico-issuer (same CA as the NICo Core API) so that:
#   1. the NICo Core API trusts the site-agent's client cert (Vault PKI CA)
#   2. the ca.crt in the secret is the Vault PKI CA, which the site-agent uses
#      as ServerCAPath to verify the NICo Core API server cert (also Vault-signed)
# Use the same values file as the install step so the rendered Certificate is
# byte-for-byte identical — preventing cert-manager from re-issuing the cert.
helm template nico-rest-site-agent "${NICO_SITE_AGENT_CHART}" \
    "${NICO_SITE_AGENT_ARGS[@]}" \
    --show-only templates/certificate.yaml | kubectl apply -f -
# Add Helm ownership annotations so the subsequent helm install can adopt this resource
# instead of failing with "exists and cannot be imported into the current release".
kubectl annotate certificate/core-grpc-client-site-agent-certs -n nico-rest \
    "meta.helm.sh/release-name=nico-rest-site-agent" \
    "meta.helm.sh/release-namespace=nico-rest" --overwrite
kubectl label certificate/core-grpc-client-site-agent-certs -n nico-rest \
    "app.kubernetes.io/managed-by=Helm" --overwrite
echo "Waiting for cert-manager to issue core-grpc-client-site-agent-certs..."
kubectl wait --for=condition=Ready certificate/core-grpc-client-site-agent-certs \
    -n nico-rest --timeout=120s
echo "NICo gRPC client cert ready"

# Create per-site Temporal namespace BEFORE deploying site-agent.
# The site-agent panics immediately on startup if this namespace doesn't exist.
echo "Creating Temporal namespace for site ${NICO_SITE_UUID}..."
_TEMPORAL_ADDR="temporal-frontend.temporal:7233"
_TEMPORAL_TLS="--tls-cert-path /var/secrets/temporal/certs/server-interservice/tls.crt \
    --tls-key-path /var/secrets/temporal/certs/server-interservice/tls.key \
    --tls-ca-path /var/secrets/temporal/certs/server-interservice/ca.crt \
    --tls-server-name interservice.server.temporal.local"
_create_temporal_namespace "${NICO_SITE_UUID}"
_verify_temporal_namespaces "${NICO_SITE_UUID}"
echo "Temporal namespace ready"

# FLOW_GRPC_ENABLED toggles the site-agent's Flow gRPC client (see
# carbide-rest/site-agent/pkg/components/config/config_manager.go —
# strings.ToLower(env)=="true"). Without it, site-agent never opens a
# connection to the Flow pod deployed in phase 7h. We default it ON when
# Flow itself is being deployed; users can flip it back via --set when
# pairing --skip-flow.
_FLOW_GRPC_ENABLED="true"
if "${SKIP_FLOW}"; then
    _FLOW_GRPC_ENABLED="false"
fi

helm upgrade --install nico-rest-site-agent "${NICO_SITE_AGENT_CHART}" \
    "${NICO_SITE_AGENT_ARGS[@]}" \
    --set "envConfig.CLUSTER_ID=${NICO_SITE_UUID}" \
    --set "envConfig.TEMPORAL_SUBSCRIBE_NAMESPACE=${NICO_SITE_UUID}" \
    --set "envConfig.TEMPORAL_SUBSCRIBE_QUEUE=site" \
    --set "envConfig.FLOW_GRPC_ENABLED=${_FLOW_GRPC_ENABLED}" \
    --timeout 300s --wait
echo "NICo REST site-agent deployed and bootstrap complete (FLOW_GRPC_ENABLED=${_FLOW_GRPC_ENABLED})"

# Verify the site-agent's gRPC connection to NICo Core succeeded. The site-agent attempts
# the connection exactly once at startup with a 5-second deadline; if it
# fails for any transient reason the NicoClient stays nil permanently and
# all inventory activities panic.  Detect failure and restart the pod so it
# gets a fresh attempt with the same correct config.
echo "Verifying site-agent NICo Core gRPC connection..."
_CONNECTED=false
for _i in $(seq 1 24); do
    _POD="$(kubectl get pods -n nico-rest \
        -l "app.kubernetes.io/name=nico-rest-site-agent" \
        -o name 2>/dev/null | head -1 || true)"
    if [ -n "${_POD}" ] && \
       kubectl logs -n nico-rest "${_POD}" --since=5m 2>/dev/null \
           | grep -q "NicoClient: successfully connected to server"; then
        _CONNECTED=true
        echo "Site-agent successfully connected to NICo Core gRPC"
        break
    fi
    echo "  Waiting for gRPC connection (${_i}/24)..."
    sleep 5
done

if [ "${_CONNECTED}" = "false" ]; then
    echo "WARNING: site-agent did not confirm gRPC connection — restarting pod for retry..."
    kubectl rollout restart statefulset/nico-rest-site-agent -n nico-rest
    kubectl rollout status statefulset/nico-rest-site-agent -n nico-rest --timeout=120s
    echo "Site-agent pod restarted — gRPC connection will be retried"
fi

echo ""
echo "========================================================================="
echo "  Setup complete"
echo "========================================================================="
echo ""
echo "  Quick health checks:"
echo "    kubectl get clusterissuer"
echo "    kubectl get secret nico-roots -n nico-system"
echo "    kubectl get pods -n nico-system"
echo "    kubectl get pods -n nico-rest"
echo "    kubectl get pods -n temporal"
echo ""
echo "  Next steps — see helm-prereqs/README.md, section 8:"
if [[ "${_KC_ENABLED:-false}" == "true" ]]; then
    echo "    • Acquiring a Keycloak access token     (helper: ${SCRIPT_DIR}/keycloak/get-token.sh)"
else
    echo "    • Acquiring an access token             (Keycloak disabled — use your own IdP)"
fi
echo "    • Setting up the NICo CLI against this cluster"
echo "    • Bootstrap the org and create your first site"
echo "    • Next: IP blocks and downstream resources"
echo ""
echo "  Keycloak deep-dive (realm, clients, roles): helm-prereqs/keycloak/README.md"
echo "========================================================================="

_SETUP_PHASE="complete"  # signals _on_failure trap: clean exit, no prompt needed
