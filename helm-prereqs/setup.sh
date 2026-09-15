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
#   helmfile, helm, kubectl, jq, ssh-keygen, envsubst (gettext)
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
#   NICO_SKIP_DPF          Skip the DPF (DOCA Platform Framework) DPU provisioning
#                          stack, which installs by DEFAULT. Default: false.
#                          Same as --skip-dpf. (NICO_INSTALL_DPF=false honored too.)
#   NICO_DPF_VERSION       doca-platform tag to clone/install. Default: v26.4.0
#   NICO_DPF_SRC_DIR       Where the doca-platform clone is cached.
#                          Default: helm-prereqs/.dpf-src
#   NICO_DPF_IMAGE_REPO    DPF operator image repository. Default the public NGC
#                          image nvcr.io/nvidia/doca/dpf-system. Point at your own
#                          registry (mirror or self-built) to match Core/REST.
#   NICO_DPF_IMAGE_TAG     DPF operator image tag. Default: NICO_DPF_VERSION.
#   NICO_DPF_IMAGE_PULL_SECRET
#                          Pull secret for the DPF/DOCA images. Unset by default —
#                          the GA nvidia/doca images are public and pull
#                          anonymously. Set for a private registry/mirror.
#   NICO_DPF_NGC_API_KEY   NGC API key for dpf-pull-secret + Argo helm repos.
#                          Default: $REGISTRY_PULL_SECRET
#   NICO_DPF_NICO_NGC_API_KEY
#                          NGC API key with access to NICo DPUService images
#                          (nico-pull-secret). Default: $NICO_DPF_NGC_API_KEY
#   NICO_DPF_K8S_API_VIP   Host-cluster API server IP reachable from DPUs.
#                          Default: derived from the kubernetes Endpoints.
#   NICO_DPF_K8S_API_PORT  Host-cluster API server port. Default: derived.
#   NICO_DPF_DPU_INTERFACE Controller interface for the Kamaji keepalived VIP.
#                          REQUIRED when DPF install is enabled.
#   NICO_DPF_DPU_CLUSTER_VIP
#                          VIP the DPUs use to reach their control plane.
#                          REQUIRED when DPF install is enabled.
#   NICO_DPF_METALLB_POOL  MetalLB address pool that advertises the DPU cluster
#                          VIP. Optional — skip when the VIP is already routable.
#   NICO_DPF_CP_LABEL_VALUE
#                          Value of the node-role.kubernetes.io/control-plane
#                          label on this cluster's control-plane nodes.
#                          Default: "" (the kubeadm convention); set to "true"
#                          on distributions that label with a value.
#   NICO_DPF_BMC_ROOT_PASSWORD
#                          Site-wide BMC root password. REQUIRED unless --skip-dpf.
#                          setup.sh deploys Core with DPF off, sets this via
#                          nico-admin-cli, then enables DPF and restarts carbide-api.
#   NICO_DPF_DPU_AGENT_CHART_VERSION
#                          Helm chart version for nico-dpu-agent. Defaults to the
#                          version baked into the carbide-api binary at build time
#                          (CARBIDE_BUILD_HELM_VERSION). Set this when testing a
#                          dev/PR image whose chart version was never published to
#                          the registry — point it at the latest published version
#                          (e.g. the most recent main build tag).
#   NICO_DPF_FMDS_CHART_VERSION
#                          Same override for the nico-fmds chart.
#   NICO_DPF_DHCP_SERVER_CHART_VERSION
#                          Same override for the nico-dhcp-server chart.
#   NICO_DPF_OTEL_CHART_VERSION
#                          Same override for the nico-otelcol chart.
#   NICO_SKIP_RMS          Skip the NVIDIA Rack Manager Service (rack-manager
#                          chart, phase 5c), which installs by DEFAULT.
#                          Same as --skip-rms. (NICO_INSTALL_RMS=false honored too.)
#   NICO_RMS_CHART         Local rack-manager chart path (directory or .tgz),
#                          e.g. your own nv-rms checkout's helm/ dir. Default:
#                          unset - the chart comes from the helm-prereqs/nv-rms
#                          git submodule, pinned by this repo (initialized
#                          automatically when git and network are available;
#                          on airgapped hosts, clone nv-rms yourself and set
#                          this variable).
#   NICO_RMS_IMAGE_REPO    RMS API server image repository. Default: the NGC
#                          rms-dev image, nvcr.io/0837451325059433/rms-dev/rms-api
#                          (still entitlement-gated; point at your mirror
#                          otherwise).
#   NICO_RMS_IMAGE_TAG     RMS API server image tag (git-describe style, e.g.
#                          v0.8.0). REQUIRED when RMS install is enabled — the
#                          chart hard-fails at render without a tag.
#   NICO_RMS_NGC_API_KEY   NGC API key for the rms-pull-secret. Required with
#                          the default (entitlement-gated) image repo.
#                          Default: $REGISTRY_PULL_SECRET
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
#                                       #   to skip Flow prerequisites (database / ESO) too
#   ./setup.sh --skip-rms               # skip the Rack Manager Service (installed by default otherwise)
#   ./setup.sh --skip-core --skip-rest  # fully non-interactive infra-only run
#   ./setup.sh --core-values /path/to/values.yaml      # use site-specific values for Phase 6
#   ./setup.sh --metallb-config /path/to/metallb.yaml  # use site-specific MetalLB config (file or kustomize dir)
#   ./setup.sh --site-overlay /path/to/kustomize-dir   # kubectl apply -k after Phase 6 (NTP services, etc.)
#   ./setup.sh --skip-dpf               # skip DPF DPU provisioning (installed by default otherwise)
#   ./setup.sh --with-observability     # also install the local monitoring stack (Loki, Tempo,
#                                       #   OTEL collector, Prometheus, Grafana) after Core —
#                                       #   see helm-prereqs/observability/README.md; can also be
#                                       #   run standalone/later: observability/install-observability.sh
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
# DPF (DOCA Platform Framework) DPU provisioning installs by DEFAULT. Opt out
# with --skip-dpf or NICO_SKIP_DPF=true (e.g. sites with no DPUs, or that still
# use the deprecated iPXE DPU path). NICO_INSTALL_DPF=false is honored too.
INSTALL_DPF="${NICO_INSTALL_DPF:-true}"
[[ "${NICO_SKIP_DPF:-false}" == "true" ]] && INSTALL_DPF=false
# RMS (Rack Manager Service) installs by DEFAULT, mirroring DPF. Opt out with
# --skip-rms or NICO_SKIP_RMS=true (e.g. sites without rack management, or an
# externally managed RMS). NICO_INSTALL_RMS=false is honored too.
INSTALL_RMS="${NICO_INSTALL_RMS:-true}"
[[ "${NICO_SKIP_RMS:-false}" == "true" ]] && INSTALL_RMS=false
WITH_OBSERVABILITY="${WITH_OBSERVABILITY:-false}"
CORE_VALUES=""
METALLB_CONFIG=""
SITE_OVERLAY=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -y)             AUTO_YES=true  ;;
        --skip-core)    SKIP_CORE=true ;;
        --skip-rest)    SKIP_REST=true ;;
        --skip-flow)    SKIP_FLOW=true ;;
        --install-dpf)  INSTALL_DPF=true ;;   # explicit; DPF is the default
        --skip-dpf)     INSTALL_DPF=false ;;
        --install-rms)  INSTALL_RMS=true ;;   # explicit; RMS is the default
        --skip-rms)     INSTALL_RMS=false ;;
        --with-observability) WITH_OBSERVABILITY=true ;;
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
        *) echo "Usage: $0 [-y] [--skip-core] [--skip-rest] [--skip-flow] [--skip-dpf] [--skip-rms] [--with-observability] [--core-values <file>] [--metallb-config <file-or-dir>] [--site-overlay <dir>] [--debug]"; exit 1 ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Pre-flight checks — env vars, tools, config files. Resolves NICO_REST_DIR
# (in-tree rest-api/) and NICO_REST_HELM_DIR (in-tree helm/rest/). Exits 1 if
# user declines to continue.
# ---------------------------------------------------------------------------
export AUTO_YES SKIP_CORE SKIP_REST SKIP_FLOW INSTALL_DPF INSTALL_RMS
# Validate INSTALL_DPF BEFORE sourcing preflight — preflight gates its DPF
# checks on INSTALL_DPF==true, so a garbage NICO_INSTALL_DPF would otherwise
# silently skip those checks before erroring here.
case "${INSTALL_DPF}" in
    true|false) ;;
    *) echo "Error: NICO_INSTALL_DPF must be true or false (got '${INSTALL_DPF}')"; exit 1 ;;
esac
case "${INSTALL_RMS}" in
    true|false) ;;
    *) echo "Error: NICO_INSTALL_RMS must be true or false (got '${INSTALL_RMS}')"; exit 1 ;;
esac

# The predecessor Flow chart bundled PSM and NSM in the Flow Deployment. This
# release does not provide an automatic migration for those workloads. Stop
# before preflight creates temporary check Pods or any installation phase can
# remove prerequisites the legacy managers still use.
_reject_bundled_flow_manager_upgrade() {
    local container_names
    local container_name
    local manager_containers
    local rollout_state
    local generation observed_generation desired_replicas updated_replicas
    local ready_replicas available_replicas total_replicas
    local pod_rows
    local pod_name pod_phase pod_containers pod_container
    local manager_pods

    if ! container_names="$(
        kubectl get deployment flow -n flow --ignore-not-found \
            -o jsonpath='{range .spec.template.spec.containers[*]}{.name}{"\n"}{end}'
    )"; then
        echo "ERROR: unable to inspect flow/flow for bundled manager containers; refusing to continue." >&2
        return 1
    fi

    manager_containers=""
    while IFS= read -r container_name; do
        case "${container_name}" in
            psm|nsm)
                manager_containers="${manager_containers:+${manager_containers}, }${container_name}"
                ;;
        esac
    done <<< "${container_names}"
    if [[ -n "${manager_containers}" ]]; then
        echo "ERROR: automatic upgrade from a Flow deployment that bundles ${manager_containers} is unsupported." >&2
        echo "Preserve the existing release, or handle its dependencies and upgrade only the flow Helm release to the Flow-only chart before rerunning setup.sh." >&2
        return 1
    fi

    # A failed manual Helm upgrade can update the Deployment template while an
    # old PSM/NSM Pod remains active. Require a completed rollout before shared
    # prerequisites or credentials can be changed.
    if [[ -n "${container_names}" ]]; then
        if ! rollout_state="$(
            kubectl get deployment flow -n flow \
                -o jsonpath='{.metadata.generation}{"|"}{.status.observedGeneration}{"|"}{.spec.replicas}{"|"}{.status.updatedReplicas}{"|"}{.status.readyReplicas}{"|"}{.status.availableReplicas}{"|"}{.status.replicas}'
        )"; then
            echo "ERROR: unable to inspect flow/flow rollout status; refusing to continue." >&2
            return 1
        fi
        IFS='|' read -r generation observed_generation desired_replicas \
            updated_replicas ready_replicas available_replicas total_replicas \
            <<< "${rollout_state}"
        updated_replicas="${updated_replicas:-0}"
        ready_replicas="${ready_replicas:-0}"
        available_replicas="${available_replicas:-0}"
        total_replicas="${total_replicas:-0}"
        if [[ -z "${generation}" || -z "${observed_generation}" || \
              -z "${desired_replicas}" || \
              "${observed_generation}" != "${generation}" || \
              "${updated_replicas}" != "${desired_replicas}" || \
              "${ready_replicas}" != "${desired_replicas}" || \
              "${available_replicas}" != "${desired_replicas}" || \
              "${total_replicas}" != "${desired_replicas}" ]]; then
            echo "ERROR: flow/flow rollout is incomplete; old manager Pods may still be running." >&2
            echo "Complete or roll back the Flow-only Helm upgrade before rerunning setup.sh." >&2
            return 1
        fi
    fi

    if ! pod_rows="$(
        kubectl get pods -n flow -l app=flow --ignore-not-found \
            -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.status.phase}{"\t"}{range .spec.containers[*]}{.name}{" "}{end}{"\n"}{end}'
    )"; then
        echo "ERROR: unable to inspect active Flow Pods for bundled manager containers; refusing to continue." >&2
        return 1
    fi
    manager_pods=""
    while IFS=$'\t' read -r pod_name pod_phase pod_containers; do
        [[ -z "${pod_name}" ]] && continue
        case "${pod_phase}" in
            Succeeded|Failed) continue ;;
        esac
        for pod_container in ${pod_containers}; do
            case "${pod_container}" in
                psm|nsm)
                    manager_pods="${manager_pods:+${manager_pods}, }${pod_name}/${pod_container}"
                    ;;
            esac
        done
    done <<< "${pod_rows}"
    if [[ -n "${manager_pods}" ]]; then
        echo "ERROR: active Flow Pods still contain bundled managers: ${manager_pods}." >&2
        echo "Complete or roll back the Flow-only Helm upgrade before rerunning setup.sh." >&2
        return 1
    fi
}

_reject_bundled_flow_manager_upgrade

# shellcheck source=preflight.sh
source "${SCRIPT_DIR}/preflight.sh"

VAULT_NS="${VAULT_NS:-vault}"
CERT_MANAGER_NS="${CERT_MANAGER_NS:-cert-manager}"
NICO_MANAGE_DEFAULT_STORAGE_CLASS="${NICO_MANAGE_DEFAULT_STORAGE_CLASS:-true}"
NICO_STORAGE_CLASS="${NICO_STORAGE_CLASS:-local-path-persistent}"
NICO_DPF_VERSION="${NICO_DPF_VERSION:-v26.4.0}"
NICO_DPF_SRC_DIR="${NICO_DPF_SRC_DIR:-${SCRIPT_DIR}/.dpf-src}"
NICO_DPF_NGC_API_KEY="${NICO_DPF_NGC_API_KEY:-${REGISTRY_PULL_SECRET:-}}"
NICO_DPF_NICO_NGC_API_KEY="${NICO_DPF_NICO_NGC_API_KEY:-${NICO_DPF_NGC_API_KEY}}"
# DPF operator image. Defaults to the public NGC image (anonymous pull). To use
# your own registry (mirror or self-built, e.g. matching NICO_IMAGE_REGISTRY),
# set NICO_DPF_IMAGE_REPO/_TAG and NICO_DPF_IMAGE_PULL_SECRET. The tag defaults
# to the chart version (NICO_DPF_VERSION) but can differ for a self-built image.
NICO_DPF_IMAGE_REPO="${NICO_DPF_IMAGE_REPO:-nvcr.io/nvidia/doca/dpf-system}"
NICO_DPF_IMAGE_TAG="${NICO_DPF_IMAGE_TAG:-${NICO_DPF_VERSION}}"
# RMS (rack-manager chart, phase 5c). The chart ships as the helm-prereqs/nv-rms
# git submodule, pinned to a reviewed nv-rms commit (bump the pin to move
# versions). Image tags are git-describe style and decoupled from the chart -
# NICO_RMS_IMAGE_TAG has no default and preflight requires it when RMS install
# is enabled.
NICO_RMS_CHART="${NICO_RMS_CHART:-}"
NICO_RMS_IMAGE_REPO="${NICO_RMS_IMAGE_REPO:-nvcr.io/0837451325059433/rms-dev/rms-api}"
NICO_RMS_IMAGE_TAG="${NICO_RMS_IMAGE_TAG:-}"
NICO_RMS_NGC_API_KEY="${NICO_RMS_NGC_API_KEY:-${REGISTRY_PULL_SECRET:-}}"
# The namespace is fixed: NICo Core's chart defaults dial
# rms-api-server.rack-manager.svc.cluster.local, and the ESO sync,
# health-check, and clean.sh all assume it. Not overridable by design.
_RMS_NS="rack-manager"
# Site-wide BMC root password. Optional: when provided, setup.sh calls
# nico-admin-cli (phase 6b) to store the credential via the API so DPU
# provisioning starts immediately. When omitted, carbide-api starts cleanly
# without it (fixed in #4167 — the DPF SDK init is now best-effort when a
# 60 s refresh interval is configured) and the operator must set the credential
# manually via `nico-admin-cli credential add-bmc --kind=site-wide-root`
# before DPU provisioning will work.
NICO_DPF_BMC_ROOT_PASSWORD="${NICO_DPF_BMC_ROOT_PASSWORD:-}"
# Optional chart-version overrides for NICo-owned DPF services. Useful when
# testing a dev/PR image whose baked-in version was never published to the
# chart registry — point at the latest published version instead.
NICO_DPF_DPU_AGENT_CHART_VERSION="${NICO_DPF_DPU_AGENT_CHART_VERSION:-}"
NICO_DPF_FMDS_CHART_VERSION="${NICO_DPF_FMDS_CHART_VERSION:-}"
NICO_DPF_DHCP_SERVER_CHART_VERSION="${NICO_DPF_DHCP_SERVER_CHART_VERSION:-}"
NICO_DPF_OTEL_CHART_VERSION="${NICO_DPF_OTEL_CHART_VERSION:-}"

# ---------------------------------------------------------------------------
# Failure handler — offer to run clean.sh if setup exits with an error.
# Registered AFTER preflight so preflight aborts don't trigger it.
# ---------------------------------------------------------------------------
_SETUP_PHASE="initializing"
# Set true only while Kamaji's DataStore webhook is relaxed to Ignore during the
# cold-start deadlock break, so the EXIT trap can restore Fail if we abort in
# between and never leave admission validation fail-open.
_KAMAJI_WH_RELAXED=false

_on_failure() {
    local _rc=$?
    local _cmd="${BASH_COMMAND}"
    # Always remove the DPF two-phase rendered-values tempfiles (they hold the
    # full site config) and the freshly issued admin client key/cert, regardless
    # of success or failure. Runs on every EXIT, so the private key is wiped even
    # when errexit aborts _dpf_set_bmc_root before its own explicit cleanup.
    rm -f "${_DPF_ON_VALUES:-}" "${_DPF_OFF_VALUES:-}" 2>/dev/null || true
    rm -rf "${_DPF_CERT_JSON:-}" "${_DPF_CERT_DIR:-}" 2>/dev/null || true
    # Drop the ephemeral BMC-root + admin-cert Secrets if a mid-run errexit
    # skipped _dpf_set_bmc_root's own cleanup — the plaintext site-wide BMC
    # password must never linger in the cluster after setup exits.
    if [[ "${INSTALL_DPF:-false}" == "true" ]]; then
        # Stop the credential Job first: a still-running pod holds the BMC
        # password in its environment, so it must be gone before (not after)
        # its source Secrets are removed.
        kubectl delete job dpf-set-bmc-root -n nico-system \
            --ignore-not-found --wait=true --timeout=60s >/dev/null 2>&1 || true
        kubectl delete secret dpf-bmc-root-pw dpf-admincli-cert -n nico-system \
            --ignore-not-found >/dev/null 2>&1 || true
    fi
    # Restore Kamaji's DataStore webhook to Fail if a mid-run errexit left it
    # relaxed to Ignore during the deadlock break — never exit fail-open.
    if [[ "${_KAMAJI_WH_RELAXED:-false}" == "true" ]]; then
        _kamaji_patch_failurepolicy Fail 2>/dev/null || true
        _KAMAJI_WH_RELAXED=false
    fi
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

# clean.sh sweeps orphaned host directories with a per-node pod that carries a
# snapshot of which PVs were live when it was created. One left Pending by an
# interrupted teardown (image pull, node offline) would still be holding that
# stale snapshot, and could start after the PVs below exist and delete their
# directories. Clear any before provisioning storage.
# This check gates data loss, so it fails closed: a suppressed error here would
# let storage be provisioned while such a pod is still able to delete it.
if ! _STALE_SWEEP="$(kubectl get pods -n kube-system \
    -l nico-lpp-sweep=true -o name 2>/dev/null)"; then
    echo "ERROR: could not check for clean.sh sweep pods — refusing to provision storage" >&2
    exit 1
fi

if [[ -n "${_STALE_SWEEP}" ]]; then
    echo "  Removing sweep pods left by an interrupted clean.sh..."
    kubectl delete pod -n kube-system -l nico-lpp-sweep=true \
        --ignore-not-found --wait --timeout=60s 2>/dev/null || true

    if ! _STALE_SWEEP="$(kubectl get pods -n kube-system \
        -l nico-lpp-sweep=true -o name 2>/dev/null)" || [[ -n "${_STALE_SWEEP}" ]]; then
        echo "ERROR: sweep pods from a previous clean.sh are still present in kube-system." >&2
        echo "       They can delete newly provisioned PV directories. Remove them first:" >&2
        echo "         kubectl delete pod -n kube-system -l nico-lpp-sweep=true" >&2
        exit 1
    fi
fi

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
#     CRDs are managed externally (not by the helm release) and applied before
#     and after helmfile sync. Site-specific config (IPAddressPool, BGPPeer,
#     BGPAdvertisement) is applied from --metallb-config <path> if provided,
#     otherwise from values/metallb-config.yaml.
# ---------------------------------------------------------------------------
_SETUP_PHASE="[1c] MetalLB"
echo "=== [1c] MetalLB ==="

# CRDs are applied directly (server-side), not helm-managed: MetalLB's cert
# rotator takes SSA ownership of the CRD conversion-webhook caBundle after
# install, so a helm-managed CRD upgrade conflicts on every re-sync (see
# operators/values/metallb.yaml crds.enabled=false). --force-conflicts keeps
# this idempotent against the rotator's field ownership.
#
# Upgrade path (2.0→2.1): prior to 2.1, crds.enabled defaulted to true, so CRDs
# were helm-managed template resources tracked in the release manifest. The 2.1
# upgrade sets crds.enabled=false; if helm still owns the CRDs it would delete
# them — and Kubernetes garbage-collects all IPAddressPool/BGPPeer/BGPAdvertisement
# instances along with the CRD schema, causing data loss and breaking site config.
#
# Strategy:
#   1. Strip helm ownership labels from any existing metallb CRDs so helm cannot
#      delete them during the upgrade (prevents both schema and instance deletion).
#   2. Apply CRDs directly before and after helmfile sync for idempotency.
#   3. On sync failure, attempt CRD restoration before returning the error.
#   4. Wait for CRDs to reach Established=True before applying site objects.
#
# Single source of truth for the chart version is the metallb release in
# helmfile.yaml — read it from there so this bootstrap and the helm release
# cannot drift when the version is bumped.
METALLB_CHART_VERSION="$(awk '/chart: metallb\/metallb/{found=1} found && /^[[:space:]]*version:/{gsub(/"/,"",$2); print $2; exit}' helmfile.yaml)"
if [[ -z "${METALLB_CHART_VERSION}" ]]; then
    echo "ERROR: could not read the metallb chart version from helmfile.yaml" >&2
    exit 1
fi

# Helper: render and server-side apply only the CRD documents from the chart.
# The awk filter emits only CustomResourceDefinition documents, splitting on
# '---' separator lines itself (POSIX awk/mawk/BusyBox treat a multi-character
# RS as its first character only, so RS="\n---\n" is not portable). helm's
# stderr is left attached so a repo/render failure says what actually broke
# instead of surfacing as a confusing kubectl parse error downstream.
_apply_metallb_crds() {
    helm template metallb metallb/metallb --version "${METALLB_CHART_VERSION}" -n metallb-system --include-crds \
        | awk '
            /^---[[:space:]]*$/ { if (doc ~ /kind: CustomResourceDefinition/) printf "%s---\n", doc; doc = ""; next }
            { doc = doc $0 "\n" }
            END { if (doc ~ /kind: CustomResourceDefinition/) printf "%s", doc }' \
        | kubectl apply --server-side --force-conflicts -f -
}

# Strip helm ownership labels/annotations from any existing metallb CRDs so that
# helmfile sync cannot delete them when transitioning from crds.enabled=true (2.0)
# to crds.enabled=false (2.1). Without this, helm deletes the CRD schema AND all
# stored IPAddressPool/BGPPeer/BGPAdvertisement instances — causing data loss that
# cannot be recovered by re-applying the schema alone.
echo "Removing helm ownership from any existing MetalLB CRDs (prevents instance deletion on upgrade)..."
while IFS= read -r crd; do
    [[ -z "${crd}" ]] && continue
    if kubectl get "${crd}" -o jsonpath='{.metadata.labels.app\.kubernetes\.io/managed-by}' 2>/dev/null \
            | grep -q 'Helm'; then
        kubectl annotate "${crd}" \
            meta.helm.sh/release-name- \
            meta.helm.sh/release-namespace- \
            --overwrite 2>/dev/null || true
        kubectl label "${crd}" \
            app.kubernetes.io/managed-by- \
            --overwrite 2>/dev/null || true
        echo "  Stripped helm ownership from ${crd}"
    fi
done < <(kubectl get crd -o name 2>/dev/null | grep '\.metallb\.io$' || true)

echo "Applying MetalLB CRDs (server-side)..."
_apply_metallb_crds

# Capture helmfile sync exit code so CRDs can be restored even on failure.
# With set -e active, the || construct is required to prevent immediate exit.
_metallb_sync_rc=0
helmfile sync -l name=metallb || _metallb_sync_rc=$?

# Re-apply CRDs after the helm sync regardless of success or failure.
# On success: idempotent safety net for any ownership-change edge cases.
# On failure: best-effort restoration so a failed upgrade does not leave the
#             cluster without CRDs (which breaks all LoadBalancer services).
echo "Re-applying MetalLB CRDs (post-sync)..."
_apply_metallb_crds || true

if [[ "${_metallb_sync_rc}" -ne 0 ]]; then
    echo "ERROR: helmfile sync for metallb failed (exit ${_metallb_sync_rc}); CRDs have been restored." >&2
    exit "${_metallb_sync_rc}"
fi

# Wait for CRDs to reach Established=True before applying site objects.
# kubectl apply on the CRDs returns before the API server has registered
# the new types; applying IPAddressPool immediately can fail with
# "no matches for kind".
echo "Waiting for MetalLB CRDs to be established..."
kubectl wait --for=condition=Established \
    crd/ipaddresspools.metallb.io \
    crd/bgppeers.metallb.io \
    crd/bgpadvertisements.metallb.io \
    --timeout=60s

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

# ---------------------------------------------------------------------------
# 5b. DPF (DOCA Platform Framework) — optional DPU provisioning stack.
#     Needs StorageClass (1), MetalLB (1c), cert-manager (2). Must complete
#     BEFORE NICo Core (6): carbide-api reads [dpf] config at startup only and
#     expects the dpu.nvidia.com CRDs, the operator, DPFOperatorConfig, and
#     DPUCluster to already exist; the core chart's nico-api-dpf Role also
#     targets the dpf-operator-system namespace created here.
#     See docs/manuals/dpf.md for the full background.
# ---------------------------------------------------------------------------
if "${INSTALL_DPF}"; then
    _SETUP_PHASE="[5b] DPF operator stack"
    echo "=== [5b] DPF (DOCA Platform Framework) ${NICO_DPF_VERSION} ==="

    # 5b.1 Prerequisite operators (Argo CD, Kamaji, maintenance-operator, NFD),
    #      pinned from doca-platform deploy/helmfiles/prereqs.yaml. All land in
    #      dpf-operator-system, same as upstream.
    helmfile sync -l name=argo-cd

    # Kamaji has a cold-start deadlock: its controller requires the 'default'
    # DataStore at boot (--datastore=default), but that DataStore's admission
    # webhook (vdatastore.kb.io, failurePolicy=Fail) is served by the
    # not-yet-running controller — so the DataStore can never be created and
    # the controller crashloops. Break the cycle: attempt the install (creates
    # etcd + controller + webhook, DataStore rejected), relax the datastore
    # webhook to Ignore, create the DataStore out-of-band so the controller can
    # boot, restore the webhook to Fail, then re-sync to reconcile the release.
    if ! helmfile sync -l name=kamaji; then
        echo "kamaji first sync failed (expected DataStore webhook deadlock) — breaking the cycle..."
        _kamaji_wh=kamaji-validating-webhook-configuration
        _kamaji_patch_failurepolicy() {  # $1 = Ignore|Fail
            local _idx
            # `|| true`: if the webhook config is absent (kamaji aborted before Helm
            # created it), pipefail+errexit would otherwise kill setup here, before
            # the empty-_idx tolerance guard below can handle it.
            _idx=$(kubectl get validatingwebhookconfiguration "${_kamaji_wh}" -o json 2>/dev/null \
                | jq -r '.webhooks | to_entries[] | select(.value.name=="vdatastore.kb.io") | .key' || true)
            [[ -z "${_idx}" ]] && return 0
            kubectl patch validatingwebhookconfiguration "${_kamaji_wh}" --type=json \
                -p="[{\"op\":\"replace\",\"path\":\"/webhooks/${_idx}/failurePolicy\",\"value\":\"$1\"}]"
        }
        _kamaji_patch_failurepolicy Ignore
        _KAMAJI_WH_RELAXED=true
        # Create the controller's bootstrap dependencies out-of-band: the
        # cert-manager Issuer + Certificate (so the webhook-server cert issues
        # and the controller container can mount it) and the default DataStore
        # (so the controller doesn't crashloop). The failed first sync aborts
        # at the DataStore CR and may not have created the Issuer (helm's CR
        # apply order is nondeterministic), so we can't rely on its partial
        # state — create all three explicitly, matching the chart.
        echo "Creating the Kamaji Issuer, webhook Certificate, and default DataStore out-of-band..."
        helm template kamaji oci://ghcr.io/nvidia/charts/kamaji --version 1.2.0 \
            -n dpf-operator-system -f operators/values/kamaji.yaml \
            --show-only templates/certmanager_issuer.yaml \
            --show-only templates/certmanager_certificate.yaml \
            --show-only charts/kamaji-etcd/templates/etcd_datastore.yaml | kubectl apply -f -
        echo "Waiting for the kamaji controller to come up (cert issued + DataStore present)..."
        kubectl rollout status deployment/kamaji -n dpf-operator-system --timeout=300s
        # Restore failurePolicy to Fail *before* the reconcile sync. helm's
        # server-side apply only conflicts when it would change a field another
        # manager owns to a different value — setting Fail (what the chart
        # renders) to the already-Fail value is co-ownership, not a conflict.
        _kamaji_patch_failurepolicy Fail
        _KAMAJI_WH_RELAXED=false
        # Reconcile the release to deployed now that the controller backs the
        # webhook and the DataStore exists.
        helmfile sync -l name=kamaji
    fi

    helmfile sync -l name=maintenance-operator
    helmfile sync -l name=node-feature-discovery
    echo "Waiting for DPF prerequisite controllers..."
    kubectl rollout status deployment -n dpf-operator-system \
        -l app.kubernetes.io/name=argocd-repo-server --timeout=300s
    kubectl rollout status statefulset -n dpf-operator-system \
        -l app.kubernetes.io/name=argocd-application-controller --timeout=300s

    # 5b.2 Pull + repo secrets. Idempotent (apply of a dry-run render).
    #      The dpu.nvidia.com/image-pull-secret label makes DPF propagate the
    #      Secret into DPUService image-pull secrets on the DPU cluster.
    if [[ -n "${NICO_DPF_NGC_API_KEY}" ]]; then
        echo "Creating DPF pull and Argo CD repository secrets..."
        # Build the docker-registry secrets off-argv: a kubectl `--docker-password`
        # argument is world-readable via ps / /proc on the host running setup.sh.
        # Feed the token through the builtin printf + a process-substitution file
        # so the NGC key never appears in an exec argument. Same resulting
        # kubernetes.io/dockerconfigjson secret as `kubectl create secret
        # docker-registry` would produce.
        _dpf_docker_secret() {  # secret-name, registry-server, token
            local _n="$1" _srv="$2" _tok="$3" _auth
            _auth="$(printf '%s' "\$oauthtoken:${_tok}" | base64 | tr -d '\n')"
            kubectl create secret generic "${_n}" \
                --namespace dpf-operator-system \
                --type=kubernetes.io/dockerconfigjson \
                --from-file=.dockerconfigjson=<(printf \
                    '{"auths":{"%s":{"username":"$oauthtoken","password":"%s","auth":"%s"}}}' \
                    "${_srv}" "${_tok}" "${_auth}") \
                --dry-run=client -o yaml | kubectl apply -f -
        }
        _dpf_docker_secret dpf-pull-secret  nvcr.io "${NICO_DPF_NGC_API_KEY}"
        _dpf_docker_secret nico-pull-secret nvcr.io "${NICO_DPF_NICO_NGC_API_KEY}"
        kubectl label secret dpf-pull-secret nico-pull-secret \
            -n dpf-operator-system dpu.nvidia.com/image-pull-secret="" --overwrite
        # Argo CD helm repository secrets (argocd.argoproj.io/secret-type label
        # is how Argo CD discovers them). URLs must not end with '/'.
        _dpf_argo_repo_secret() {  # name, repo-name, url, extra literals...
            local _name="$1" _repo="$2" _url="$3"; shift 3
            kubectl create secret generic "${_name}" \
                --namespace dpf-operator-system \
                --from-literal=name="${_repo}" \
                --from-literal=url="${_url}" \
                --from-literal=type=helm \
                --from-literal=username='$oauthtoken' \
                --from-file=password=<(printf '%s' "${NICO_DPF_NGC_API_KEY}") \
                "$@" \
                --dry-run=client -o yaml | kubectl apply -f -
            kubectl label secret "${_name}" -n dpf-operator-system \
                argocd.argoproj.io/secret-type=repository --overwrite
        }
        # These repo-secret URLs must EXACTLY match the helm_repo_url carbide-api
        # requests when deploying each DPUService (its [dpf.services.*] defaults,
        # crates/api-core/src/dpf_services.rs), or Argo CD can't match the repo
        # and the private chart pull fails. Override in lockstep with
        # [dpf.services.*] when you mirror the charts.
        _dpf_argo_repo_secret ngc-doca-oci-helm nvidia-doca-oci \
            "${NICO_DPF_HELM_REPO_OCI:-nvcr.io/nvidia/doca}" \
            --from-literal=enableOCI=true
        _dpf_argo_repo_secret ngc-doca-https-helm nvidia-doca-https \
            "${NICO_DPF_HELM_REPO_HTTPS:-https://helm.ngc.nvidia.com/nvidia/doca}"
        _dpf_argo_repo_secret ngc-carbide-https-helm nvidia-carbide-https \
            "${NICO_DPF_HELM_REPO_CARBIDE:-https://helm.ngc.nvidia.com/0837451325059433/carbide-dev}"
    else
        echo "NICO_DPF_NGC_API_KEY / REGISTRY_PULL_SECRET not set — skipping DPF pull"
        echo "and Argo CD repository secrets (air-gapped or pre-loaded registry)."
    fi
    # hbn-user-password: random local FRR credential for the HBN DPUService;
    # generate only when absent so re-runs don't rotate it.
    if ! kubectl get secret hbn-user-password -n dpf-operator-system &>/dev/null; then
        kubectl create secret generic hbn-user-password \
            --namespace dpf-operator-system \
            --from-file=password=<(LC_ALL=C tr -dc 'a-z0-9' < /dev/urandom | head -c 10)
    fi
    kubectl label secret hbn-user-password -n dpf-operator-system \
        dpu.nvidia.com/image-pull-secret="" --overwrite

    # 5b.3 cert-manager approver policy. Only needed (and only appliable) when
    #      the cluster runs approver-policy; our stock cert-manager does not
    #      (built-in approver auto-approves — operators/values/cert-manager.yaml).
    if kubectl get crd certificaterequestpolicies.policy.cert-manager.io &>/dev/null; then
        echo "approver-policy detected — applying DPF CertificateRequestPolicy..."
        kubectl apply -f operators/dpf/cert-manager-policy.yaml
    else
        echo "approver-policy not installed — built-in cert-manager approver auto-approves; skipping CertificateRequestPolicy."
    fi

    # 5b.4 Clone doca-platform at the pinned tag (cached, shallow, idempotent).
    if [[ -d "${NICO_DPF_SRC_DIR}/.git" ]]; then
        echo "Reusing doca-platform clone at ${NICO_DPF_SRC_DIR} (tag ${NICO_DPF_VERSION})..."
        git -C "${NICO_DPF_SRC_DIR}" fetch --depth 1 origin \
            "refs/tags/${NICO_DPF_VERSION}:refs/tags/${NICO_DPF_VERSION}" 2>/dev/null || true
        git -C "${NICO_DPF_SRC_DIR}" checkout -q -f "refs/tags/${NICO_DPF_VERSION}"
    else
        # A leftover non-git or partially-cloned directory (e.g. a clone killed
        # mid-run, or disk-full) would make `git clone` fail with "destination
        # path already exists and is not empty" on every retry — remove it first
        # so a re-run always recovers. Guard the rm -rf against a dangerous
        # NICO_DPF_SRC_DIR override (empty / root / $HOME) before deleting.
        case "${NICO_DPF_SRC_DIR}" in
            ""|"/"|"${HOME}"|"${HOME}/")
                echo "ERROR: refusing to remove NICO_DPF_SRC_DIR='${NICO_DPF_SRC_DIR}' — set it to a dedicated clone dir."
                exit 1 ;;
        esac
        if [[ -e "${NICO_DPF_SRC_DIR}" ]]; then
            echo "ERROR: '${NICO_DPF_SRC_DIR}' exists but is not a doca-platform Git clone."
            echo "  Remove it explicitly and re-run (refusing to auto-delete a non-clone path)."
            exit 1
        fi
        echo "Cloning doca-platform ${NICO_DPF_VERSION}..."
        git clone --depth 1 --branch "${NICO_DPF_VERSION}" \
            https://github.com/NVIDIA/doca-platform.git "${NICO_DPF_SRC_DIR}"
    fi

    # 5b.5 DPF operator chart from the clone. NICo overrides (docs/manuals/dpf.md
    #      §2): NodeFeatureRules off because NFD labels nodes via its own config
    #      (PCI class 0200). The in-repo source chart ships EMPTY
    #      controllerManager.image (CI stamps it when publishing to NGC), so we
    #      set it explicitly — matching the published nvidia/doca chart. Repo is
    #      overridable; the tag tracks NICO_DPF_VERSION.
    #      Image pull secret: the GA nvidia/doca images are PUBLIC, so by default
    #      the operator pulls them anonymously. Attaching a registry-scoped
    #      secret that lacks nvidia/doca entitlement makes nvcr.io 403 the pull
    #      (kubelet does not fall back to anonymous). Set NICO_DPF_IMAGE_PULL_SECRET
    #      only when the DPF/DOCA images live in a private registry/mirror.
    _dpf_op_pull_args=()
    if [[ -n "${NICO_DPF_IMAGE_PULL_SECRET:-}" ]]; then
        _dpf_op_pull_args+=(--set "imagePullSecrets[0].name=${NICO_DPF_IMAGE_PULL_SECRET}")
    fi
    helm upgrade --install dpf-operator \
        "${NICO_DPF_SRC_DIR}/deploy/charts/dpf-operator" \
        --namespace dpf-operator-system \
        --set "enableNodeFeatureRules=false" \
        ${_dpf_op_pull_args[@]+"${_dpf_op_pull_args[@]}"} \
        --set "controllerManager.image.repository=${NICO_DPF_IMAGE_REPO}" \
        --set "controllerManager.image.tag=${NICO_DPF_IMAGE_TAG}" \
        --wait --timeout 600s
    kubectl wait --for=condition=Available deployment/dpf-operator-controller-manager \
        -n dpf-operator-system --timeout=300s
    echo "DPF operator ready"

    # 5b.6 Operator-level CRs. carbide-api only reads/patches these — they must
    #      be created here (its SDK creates the provisioning source, DPUFlavor,
    #      and DPUDeployment itself at startup, but never DPFOperatorConfig or
    #      DPUCluster).
    if [[ -z "${NICO_DPF_K8S_API_VIP:-}" ]]; then
        NICO_DPF_K8S_API_VIP="$(kubectl get endpoints kubernetes -n default \
            -o jsonpath='{.subsets[0].addresses[0].ip}')"
        echo "NICO_DPF_K8S_API_VIP not set — derived ${NICO_DPF_K8S_API_VIP} from the kubernetes Endpoints."
        echo "  NOTE: this must be reachable FROM THE DPUs; override if the derived address is not."
    fi
    if [[ -z "${NICO_DPF_K8S_API_PORT:-}" ]]; then
        NICO_DPF_K8S_API_PORT="$(kubectl get endpoints kubernetes -n default \
            -o jsonpath='{.subsets[0].ports[0].port}')"
    fi
    export NICO_DPF_K8S_API_VIP NICO_DPF_K8S_API_PORT
    export NICO_DPF_DPU_INTERFACE NICO_DPF_DPU_CLUSTER_VIP
    export NICO_DPF_CP_LABEL_VALUE="${NICO_DPF_CP_LABEL_VALUE:-}"
    envsubst '${NICO_DPF_K8S_API_VIP} ${NICO_DPF_K8S_API_PORT}' \
        < operators/dpf/dpfoperatorconfig.yaml.tmpl | kubectl apply -f -
    envsubst '${NICO_DPF_DPU_INTERFACE} ${NICO_DPF_DPU_CLUSTER_VIP} ${NICO_DPF_CP_LABEL_VALUE}' \
        < operators/dpf/dpucluster.yaml.tmpl | kubectl apply -f -
    if [[ -n "${NICO_DPF_METALLB_POOL:-}" ]]; then
        export NICO_DPF_METALLB_POOL
        envsubst '${NICO_DPF_METALLB_POOL} ${NICO_DPF_DPU_CLUSTER_VIP}' \
            < operators/dpf/dpu-cluster-vip-service.yaml.tmpl | kubectl apply -f -
    else
        echo "NICO_DPF_METALLB_POOL not set — skipping the DPU cluster VIP LoadBalancer Service."
        echo "  Ensure ${NICO_DPF_DPU_CLUSTER_VIP} is routable from the DPUs by other means."
    fi

    # 5b.7 Readiness — WARN only. Kamaji TenantControlPlane bring-up can take
    #      minutes and depends on VIP routability that this script can't verify.
    echo "Waiting up to 300s for the DPU cluster control plane (non-fatal)..."
    _dpf_deadline=$(( $(date +%s) + 300 ))
    until [[ "$(kubectl get dpucluster carbide-dpf-cluster -n dpf-operator-system \
                -o jsonpath='{.status.phase}' 2>/dev/null)" == "Ready" ]]; do
        if (( $(date +%s) >= _dpf_deadline )); then
            echo "WARNING: DPUCluster carbide-dpf-cluster is not Ready yet. Continuing —"
            echo "  check it later with: kubectl get dpucluster,tenantcontrolplane -n dpf-operator-system"
            break
        fi
        sleep 10
    done

    # 5b.8 [dpf] is NOT enabled in the Core config here. The two-phase approach
    #      (Core DPF-off first, then DPF-on after phase 6b) lets setup.sh call
    #      nico-admin-cli to set the BMC root credential while carbide-api is
    #      already up. carbide-api can start with DPF enabled even when the
    #      credential is absent (#4167), but setting it before enabling DPF
    #      ensures DPU provisioning begins immediately without waiting for the
    #      first 60 s refresh tick.
    echo "DPF stack installed (carbide-api DPF enablement happens after Core in phase 6)"
else
    echo "Skipping DPF (--skip-dpf / NICO_SKIP_DPF=true). DPUs, if any, use the deprecated iPXE path."
fi

# ---------------------------------------------------------------------------
# Set the site-wide BMC root password via nico-admin-cli. Used by phase 6b.
# Issues a short-lived admin client cert from the nicoca PKI (per
# docs/provisioning/ingesting-hosts.md), then runs the CLI (bundled in the
# NICo image at /opt/carbide/nico-admin-cli) as an in-cluster Job that reaches
# carbide-api through its external LoadBalancer, verifying TLS with the
# issued CA and authenticating with the client cert.
# ---------------------------------------------------------------------------
_dpf_set_bmc_root() {
    local _hostname _lbip _vault_token
    # `|| true` so a no-match grep (nothing to resolve) doesn't trip `set -e`
    # via pipefail before the guard below can report a clean error.
    # Strip an inline YAML "# comment" before quotes/space so a value left with
    # the shipped trailing comment (hostname: "api.foo" # REQUIRED: ...) doesn't
    # bleed the comment text into the hostname (which would break API_URL and the
    # Job hostAliases). Matches preflight's _strip_comments.
    _hostname="$( { grep -E '^[[:space:]]+hostname:' "${_CORE_VALUES_FILE}" | head -1 \
        | sed -E "s/.*hostname:[[:space:]]*//; s/[[:space:]]+#.*$//; s/[\"']//g" | tr -d '[:space:]'; } || true)"
    _lbip="$(kubectl get svc nico-api-external -n nico-system \
        -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || true)"
    if [[ -z "${_hostname}" || -z "${_lbip}" ]]; then
        echo "ERROR: could not resolve nico-api hostname (${_hostname:-?}) or LB IP (${_lbip:-?})"
        return 1
    fi
    echo "Setting site-wide BMC root password (api ${_hostname} -> ${_lbip})..."

    # 1. Issue a short-lived admin client cert from the nicoca PKI.
    _vault_token="$(kubectl -n "${VAULT_NS}" get secret vaultroottoken \
        -o jsonpath='{.data.token}' | base64 -d)"
    # Script-global (not local) so the EXIT trap wipes the private key if we
    # fail before the explicit cleanup below — errexit skips the rest of the fn.
    _DPF_CERT_JSON="$(mktemp)"; _DPF_CERT_DIR="$(mktemp -d)"
    # Feed the Vault ROOT token via stdin, never as an exec argument: kubectl
    # encodes argv into the API-server audit log (requestURI) and it surfaces in
    # vault-0's process list, exposing the full-privilege root token. `read`
    # pulls it from stdin inside the pod; the CN (not secret) rides in as $1.
    printf '%s\n' "${_vault_token}" | kubectl -n "${VAULT_NS}" exec -i vault-0 -- \
        sh -c 'read -r VAULT_TOKEN; export VAULT_TOKEN VAULT_SKIP_VERIFY=true
               vault write -format=json nicoca/issue/nico-cluster \
                 common_name="$1" ttl=1h' _ "${_hostname}" > "${_DPF_CERT_JSON}"
    jq -r '.data.certificate' "${_DPF_CERT_JSON}" > "${_DPF_CERT_DIR}/client.crt"
    jq -r '.data.private_key' "${_DPF_CERT_JSON}" > "${_DPF_CERT_DIR}/client.key"
    jq -r '.data.issuing_ca'  "${_DPF_CERT_JSON}" > "${_DPF_CERT_DIR}/ca.crt"
    kubectl create secret generic dpf-admincli-cert -n nico-system \
        --from-file=client.crt="${_DPF_CERT_DIR}/client.crt" \
        --from-file=client.key="${_DPF_CERT_DIR}/client.key" \
        --from-file=ca.crt="${_DPF_CERT_DIR}/ca.crt" \
        --dry-run=client -o yaml | kubectl apply -f -
    kubectl create secret generic dpf-bmc-root-pw -n nico-system \
        --from-file=password=<(printf '%s' "${NICO_DPF_BMC_ROOT_PASSWORD}") \
        --dry-run=client -o yaml | kubectl apply -f -
    rm -rf "${_DPF_CERT_JSON}" "${_DPF_CERT_DIR}"; _DPF_CERT_JSON=""; _DPF_CERT_DIR=""

    # 2. Run nico-admin-cli as a Job against carbide-api's external endpoint.
    kubectl delete job dpf-set-bmc-root -n nico-system --ignore-not-found >/dev/null 2>&1
    kubectl apply -f - <<EOF
apiVersion: batch/v1
kind: Job
metadata:
  name: dpf-set-bmc-root
  namespace: nico-system
spec:
  backoffLimit: 3
  # Hard-bound the Job so a stuck/retrying pod can't keep the BMC password in
  # its environment past the poll deadline below.
  activeDeadlineSeconds: 180
  ttlSecondsAfterFinished: 600
  template:
    spec:
      restartPolicy: Never
      imagePullSecrets:
        - name: imagepullsecret
      hostAliases:
        - ip: "${_lbip}"
          hostnames: ["${_hostname}"]
      volumes:
        - name: cert
          secret:
            secretName: dpf-admincli-cert
      containers:
        - name: admincli
          image: "${NICO_IMAGE_REGISTRY}/nvmetal-carbide:${NICO_CORE_IMAGE_TAG}"
          command: ["/opt/carbide/nico-admin-cli"]
          args:
            - credential
            - add-bmc
            - --kind=site-wide-root
            - --username=admin
            - --password=\$(BMC_ROOT_PASSWORD)
          env:
            - name: API_URL
              value: "https://${_hostname}:443"
            - name: ROOT_CA_PATH
              value: /certs/ca.crt
            - name: CLIENT_CERT_PATH
              value: /certs/client.crt
            - name: CLIENT_KEY_PATH
              value: /certs/client.key
            - name: BMC_ROOT_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: dpf-bmc-root-pw
                  key: password
          volumeMounts:
            - name: cert
              mountPath: /certs
              readOnly: true
EOF
    # Poll for either terminal state so a failed Job (bad password, cert
    # rejected) is caught immediately instead of burning the full timeout.
    echo "Waiting for the BMC-root Job to complete..."
    local _job_ok=false _deadline _s _f
    _deadline=$(( $(date +%s) + 180 ))
    while (( $(date +%s) < _deadline )); do
        _s="$(kubectl get job dpf-set-bmc-root -n nico-system \
            -o jsonpath='{.status.succeeded}' 2>/dev/null || true)"
        _f="$(kubectl get job dpf-set-bmc-root -n nico-system \
            -o jsonpath='{.status.conditions[?(@.type=="Failed")].status}' 2>/dev/null || true)"
        [[ "${_s}" == "1" ]] && { _job_ok=true; break; }
        [[ "${_f}" == "True" ]] && break
        sleep 3
    done
    if [[ "${_job_ok}" != "true" ]]; then
        echo "ERROR: nico-admin-cli BMC-root Job did not complete. Recent logs:"
        kubectl logs job/dpf-set-bmc-root -n nico-system --tail=40 2>/dev/null || true
        # Delete (and wait for) the Job so no pod keeps the BMC password in its
        # environment before we remove the Secrets below.
        kubectl delete job dpf-set-bmc-root -n nico-system \
            --ignore-not-found --wait=true >/dev/null 2>&1 || true
    fi
    # Always remove the plaintext BMC password + client-cert secrets — on the
    # failure path too, so the site-wide BMC password never lingers in the
    # cluster after a failed enablement.
    kubectl delete secret dpf-bmc-root-pw dpf-admincli-cert -n nico-system \
        --ignore-not-found >/dev/null 2>&1
    [[ "${_job_ok}" == "true" ]] || return 1
    echo "Site-wide BMC root password set."
}

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
# 5c. RMS (NVIDIA Rack Manager Service) — optional rack-management backend.
#     nico-api's chart defaults already dial rms-api-server.rack-manager.svc
#     :8801 with component-manager backends set to rms; this phase provides
#     that server. Needs nico-pg-cluster + ESO (5: the rms database, user, and
#     rms-db-eso sync are created by nico-prereqs when NICO_INSTALL_RMS=true)
#     and the vault-nico-issuer ClusterIssuer (5) for the mTLS certificate.
#     Runs before Core (6) only so rack operations work as soon as Core is up;
#     Core's RMS client is lazy and tolerates RMS arriving later.
# ---------------------------------------------------------------------------
if "${INSTALL_RMS}"; then
    _SETUP_PHASE="[5c] Rack Manager Service"
    echo ""
    echo "=== [5c] RMS (Rack Manager Service) ==="

    # 5c.1 Namespace. Created here (not by the chart) so the pull secret and
    #      the ESO credential sync have somewhere to land before helm runs.
    #      The rms-db-eso ClusterExternalSecret selects it by metadata.name.
    kubectl create namespace "${_RMS_NS}" --dry-run=client -o yaml \
        | kubectl apply -f -

    # 5c.2 Image pull secret. Same off-argv construction as the DPF secrets:
    #      the NGC key must never appear in an exec argument.
    if [[ -n "${NICO_RMS_NGC_API_KEY}" ]]; then
        # Scope the credential to the registry the image actually pulls from
        # (a mirror override included), and honor REGISTRY_PULL_USERNAME -
        # kubelet matches pull secrets by registry host, so an nvcr.io-only
        # entry is useless for a mirror.
        _rms_registry="${NICO_RMS_IMAGE_REPO%%/*}"
        _rms_pull_user="${REGISTRY_PULL_USERNAME:-\$oauthtoken}"
        echo "Creating rms-pull-secret in ${_RMS_NS} (registry: ${_rms_registry})..."
        _rms_auth="$(printf '%s' "${_rms_pull_user}:${NICO_RMS_NGC_API_KEY}" | base64 | tr -d '\n')"
        kubectl create secret generic rms-pull-secret \
            --namespace "${_RMS_NS}" \
            --type=kubernetes.io/dockerconfigjson \
            --from-file=.dockerconfigjson=<(printf \
                '{"auths":{"%s":{"username":"%s","password":"%s","auth":"%s"}}}' \
                "${_rms_registry}" "${_rms_pull_user}" "${NICO_RMS_NGC_API_KEY}" "${_rms_auth}") \
            --dry-run=client -o yaml | kubectl apply -f -
        unset _rms_auth
    else
        echo "NICO_RMS_NGC_API_KEY not set - skipping rms-pull-secret (mirror or pre-loaded registry)."
    fi

    # 5c.3 Resolve the chart: an explicit NICO_RMS_CHART override, or the
    #      pinned helm-prereqs/nv-rms git submodule. The submodule is the
    #      supply-chain boundary: the commit is pinned and reviewed in this
    #      repo, and git verifies the hash on init - setup.sh never clones an
    #      arbitrary ref. Airgapped hosts clone nv-rms out-of-band and point
    #      NICO_RMS_CHART at it.
    if [[ -n "${NICO_RMS_CHART}" ]]; then
        _RMS_CHART="${NICO_RMS_CHART}"
        echo "Using local rack-manager chart: ${_RMS_CHART}"
    else
        _RMS_SUBMODULE="${SCRIPT_DIR}/nv-rms"
        # The pin is only a boundary if the working tree honors it: refuse a
        # dirty submodule, then align HEAD to the gitlink this repo records.
        if [[ -d "${_RMS_SUBMODULE}/.git" || -f "${_RMS_SUBMODULE}/.git" ]] && \
           [[ -n "$(git -C "${_RMS_SUBMODULE}" status --porcelain 2>/dev/null)" ]]; then
            echo "Error: helm-prereqs/nv-rms has local modifications."
            echo "  → Commit/stash them upstream, restore the submodule, or point"
            echo "    NICO_RMS_CHART at your modified chart explicitly."
            exit 1
        fi
        echo "Syncing the nv-rms submodule to the pinned commit..."
        if ! git -C "${SCRIPT_DIR}/.." submodule update --init --checkout -- helm-prereqs/nv-rms; then
            echo "Error: could not sync the nv-rms submodule to the pinned commit."
            echo "  → On an airgapped host, clone https://github.com/dsx-ai-factory/nv-rms"
            echo "    at the pinned commit (git -C ${SCRIPT_DIR}/.. submodule status) and"
            echo "    set NICO_RMS_CHART=<clone>/helm."
            exit 1
        fi
        _RMS_CHART="${_RMS_SUBMODULE}/helm"
        echo "rack-manager chart: ${_RMS_CHART} ($(git -C "${_RMS_SUBMODULE}" rev-parse --short HEAD 2>/dev/null || echo pinned))"
    fi

    # 5c.4 Wait for the ESO-synced DB credentials. The rms database/user are
    #      declared on nico-pg-cluster by nico-prereqs (phase 5); the Zalando
    #      operator mints the credentials Secret and rms-db-eso projects it here.
    echo "Waiting for RMS DB credentials..."
    _rms_creds_ok=false
    for _rms_i in $(seq 1 36); do
        if kubectl get secret rms.nico.nico-pg-cluster.credentials \
            -n "${_RMS_NS}" &>/dev/null; then
            _rms_creds_ok=true
            break
        fi
        echo "  credentials not yet synced (${_rms_i}/36) — retrying in 5s..."
        sleep 5
    done
    if [[ "${_rms_creds_ok}" == "false" ]]; then
        echo "Error: rms.nico.nico-pg-cluster.credentials never appeared in ${_RMS_NS}."
        echo "  → Check 'kubectl describe clusterexternalsecret rms-db-eso' and that the"
        echo "    Zalando operator created the rms user (kubectl get secret -n postgres | grep rms)."
        echo "  → Confirm nico-prereqs synced with NICO_INSTALL_RMS=true (phase 5 of this run)."
        exit 1
    fi
    echo "RMS DB credentials ready"

    # 5c.5 Install. No --wait: the pod cannot start until cert-manager writes
    #      the TLS Secret, so waiting is done explicitly below where each
    #      failure mode gets its own message instead of a generic helm timeout.
    NICO_RMS_CMD=(
        helm upgrade --install rack "${_RMS_CHART}"
        --namespace "${_RMS_NS}"
        -f "${SCRIPT_DIR}/values/rms.yaml"
        # The chart renders into .Values.namespace, not the -n flag; set both.
        --set "namespace=${_RMS_NS}"
        --set-string "apiServer.image.repository=${NICO_RMS_IMAGE_REPO}"
        --set-string "global.image.tag=${NICO_RMS_IMAGE_TAG}"
    )
    [[ -n "${NICO_RMS_NGC_API_KEY}" ]] && \
        NICO_RMS_CMD+=(--set "global.imagePullSecrets[0].name=rms-pull-secret")
    "${WITH_OBSERVABILITY}" && \
        NICO_RMS_CMD+=(--set "apiServer.serviceMonitor.enabled=true")
    echo "Installing rack-manager helm chart..."
    "${NICO_RMS_CMD[@]}"

    # 5c.6 Certificate first: issuance is async, and the deployment sits in
    #      ContainerCreating until the Secret exists.
    echo "Waiting for the RMS API server certificate..."
    kubectl wait --for=condition=Ready certificate/rms-api-server-certificate \
        -n "${_RMS_NS}" --timeout=180s

    # The RMS binary refuses to start unless ca.crt is present in the TLS
    # Secret (it is the client-CA for mTLS). cert-manager's Vault issuer does
    # not populate ca.crt under every role/chain configuration, so verify it
    # here with a targeted message rather than letting the pod crashloop.
    if [[ -z "$(kubectl get secret rms-api-server-certificate \
            -n "${_RMS_NS}" -o jsonpath='{.data.ca\.crt}' 2>/dev/null)" ]]; then
        echo "Error: the issued Secret rms-api-server-certificate has no ca.crt."
        echo "  → RMS requires ca.crt as the mTLS client CA and will not start without it."
        echo "  → Check the vault-nico-issuer CA chain configuration, or pre-create a"
        echo "    Secret with tls.crt/tls.key/ca.crt and set apiServer.tls.existingSecret."
        exit 1
    fi

    echo "Waiting for rms-api-server rollout..."
    kubectl rollout status deployment/rms-api-server \
        -n "${_RMS_NS}" --timeout=300s
    echo "RMS ready: rms-api-server.${_RMS_NS}.svc.cluster.local:8801"
else
    echo ""
    echo "=== [5c] RMS (Rack Manager Service) ==="
    echo "Skipped (RMS disabled via --skip-rms / NICO_SKIP_RMS / NICO_INSTALL_RMS=false)."
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

    if "${INSTALL_DPF}"; then
        # Two-phase DPF enablement: Core is deployed with DPF OFF first so that
        # carbide-api is running when setup.sh tries to set the site-wide BMC
        # root credential via nico-admin-cli (phase 6b). If the credential is
        # not provided here the password step is skipped and DPF-on is still
        # deployed — carbide-api tolerates a missing credential at startup
        # (#4167) and writes the K8s Secret on the next refresh tick once the
        # operator sets the credential manually. Build both value files.
        _DPF_ON_VALUES="$(mktemp -t nico-core-dpf-on.XXXXXX)"
        _DPF_OFF_VALUES="$(mktemp -t nico-core-dpf-off.XXXXXX)"
        if [[ -n "${CORE_VALUES}" ]]; then
            # --core-values is expected to carry a [dpf] block with enabled=true.
            cp "${_CORE_VALUES_FILE}" "${_DPF_ON_VALUES}"
        else
            # default file: the [dpf] block ships '#dpf# '-commented; uncomment it.
            sed -E 's/^([[:space:]]*)#dpf# ?/\1/' "${_CORE_VALUES_FILE}" > "${_DPF_ON_VALUES}"
        fi
        # DPF-OFF = DPF-ON with the [dpf] section's own `enabled` forced to false.
        # Only the direct [dpf] key is targeted: entering any other top-level
        # [section] clears the in-block state so we never flip a later
        # section's `enabled` when [dpf] has no inline `enabled =` of its own.
        awk '
            /^[[:space:]]*\[[^]]+\][[:space:]]*$/ { indpf = ($0 ~ /^[[:space:]]*\[dpf\][[:space:]]*$/) ? 1 : 0 }
            indpf==1 && /^[[:space:]]*enabled[[:space:]]*=/ { sub(/=[[:space:]]*true/, "= false"); indpf=0 }
            { print }
        ' "${_DPF_ON_VALUES}" > "${_DPF_OFF_VALUES}"

        # Inject per-service chart-version overrides into both value files.
        # These let operators (and QA) pin NICo-owned DPF service charts to a
        # published version when testing a dev/PR image whose baked-in version
        # does not exist in the registry.
        _dpf_inject_service_overrides() {
            local file="$1"
            # Build the extra TOML fragment. The [[ ]] && pattern is intentionally
            # avoided: with set -euo pipefail, [[ -n "" ]] returns 1 and the whole
            # compound expression exits non-zero, aborting the script even when no
            # override is needed. Use if-then to keep exit-code semantics clean.
            local _extra=""
            if [[ -n "${NICO_DPF_DPU_AGENT_CHART_VERSION}" ]]; then
                _extra+="$(printf '\n[dpf.services.dpu_agent]\nhelm_version = "%s"\n' "${NICO_DPF_DPU_AGENT_CHART_VERSION}")"
            fi
            if [[ -n "${NICO_DPF_FMDS_CHART_VERSION}" ]]; then
                _extra+="$(printf '\n[dpf.services.fmds]\nhelm_version = "%s"\n' "${NICO_DPF_FMDS_CHART_VERSION}")"
            fi
            if [[ -n "${NICO_DPF_DHCP_SERVER_CHART_VERSION}" ]]; then
                _extra+="$(printf '\n[dpf.services.dhcp_server]\nhelm_version = "%s"\n' "${NICO_DPF_DHCP_SERVER_CHART_VERSION}")"
            fi
            if [[ -n "${NICO_DPF_OTEL_CHART_VERSION}" ]]; then
                _extra+="$(printf '\n[dpf.services.otel]\nhelm_version = "%s"\n' "${NICO_DPF_OTEL_CHART_VERSION}")"
            fi
            [[ -z "${_extra}" ]] && return 0

            # The overrides are TOML that must live INSIDE the nicoApiSiteConfig
            # YAML literal block scalar, not appended at file level. Appending raw
            # TOML at the YAML file level produces invalid YAML (helm reports
            # "could not find expected ':'"). The nicoApiSiteConfig block uses
            # 6-space indentation; we insert the extra TOML lines with that same
            # indent immediately before the first line that drops below it (which
            # ends the YAML literal block scalar).
            # Write the indented TOML lines to a temp file — awk -v cannot
            # hold newlines so we pass a filename instead.
            local _inject_file
            _inject_file="$(mktemp)"
            printf '%s' "${_extra}" | sed 's/^/      /' > "${_inject_file}"
            awk -v inject_file="${_inject_file}" '
                /^    nicoApiSiteConfig:/ { in_block=1; print; next }
                # Stay in block on 6-space-indented content OR blank/whitespace-
                # only lines (YAML literal block scalars allow blank lines as
                # part of the content; only a non-blank line at lower indentation
                # terminates the block).
                in_block && (/^      / || /^[[:space:]]*$/) { print; next }
                in_block {
                    while ((getline line < inject_file) > 0) print line
                    close(inject_file)
                    in_block=0
                    print; next
                }
                { print }
                END {
                    if (in_block) {
                        while ((getline line < inject_file) > 0) print line
                        close(inject_file)
                    }
                }
            ' "${file}" > "${file}.tmp" && mv "${file}.tmp" "${file}"
            rm -f "${_inject_file}"
        }
        _dpf_inject_service_overrides "${_DPF_ON_VALUES}"
        _dpf_inject_service_overrides "${_DPF_OFF_VALUES}"

        # Guard against a silent no-op: if the ON values don't actually enable
        # [dpf] (e.g. --core-values with no/commented [dpf] block, or an inline
        # [dpf] table the toggle can't read), the two-phase flow would deploy
        # Core, "enable" nothing, restart, and falsely report success while
        # carbide-api runs DPF-off. Fail early with an actionable message.
        _dpf_site_enabled() {   # prints the [dpf] section's `enabled` value, or "absent"
            awk '
                /^[[:space:]]*\[[^]]+\][[:space:]]*$/ { indpf = ($0 ~ /^[[:space:]]*\[dpf\][[:space:]]*$/) ? 1 : 0 }
                indpf==1 && /^[[:space:]]*enabled[[:space:]]*=/ {
                    # Anchor to the FIRST "=" (the key/value separator); a greedy
                    # ".*=" would read a trailing comment like "# default=true".
                    v=$0; sub(/^[^=]*=[[:space:]]*/,"",v); sub(/[[:space:]].*/,"",v); print v; found=1; exit
                }
                END { if (!found) print "absent" }
            ' "$1"
        }
        if [[ "$(_dpf_site_enabled "${_DPF_ON_VALUES}")" != "true" ]]; then
            echo "Error: DPF is enabled (the default), but the site config has no '[dpf]' table with"
            echo "  'enabled = true' on its own line."
            if [[ -n "${CORE_VALUES}" ]]; then
                echo "  Add a [dpf] block (enabled = true, docker_image_pull_secret = \"nico-pull-secret\")"
                echo "  to ${CORE_VALUES}, or pass --skip-dpf. See docs/manuals/dpf.md §3.5."
            else
                echo "  The default values/nico-core.yaml [dpf] block appears to have been removed."
            fi
            exit 1
        fi
        if [[ "$(_dpf_site_enabled "${_DPF_OFF_VALUES}")" == "true" ]]; then
            echo "Error: could not disable [dpf] for the first-phase (DPF-off) Core deploy."
            echo "  Write [dpf] as a standard table header with 'enabled = true' on its own line."
            exit 1
        fi

        # Idempotent re-run: if the LIVE site config already has DPF enabled (a
        # prior run reached phase 6b), carbide-api is already up with the site-wide
        # BMC root set. Re-running the DPF-OFF phase would rewrite the ConfigMap to
        # enabled=false and — if phase 6b then failed — leave the site DPF-disabled
        # on the next pod restart ([dpf] is read only at startup). So detect that
        # state and deploy DPF-ON directly, skipping the down-cycle and BMC step.
        # The live ConfigMap's [dpf].enabled=true is only ever persisted by phase
        # 6b's DPF-ON upgrade, which runs strictly AFTER _dpf_set_bmc_root sets the
        # site-wide BMC root — so enabled=true alone implies BMC is set. Detect it
        # from the ConfigMap ONLY: gating on live pod health would false-negative
        # during a healthy in-progress rollout and wrongly re-run the destructive
        # down-cycle this check exists to prevent. (A fresh install has no such
        # ConfigMap; a prior run that failed before phase 6b left it enabled=false.)
        _dpf_already_on=false
        _live_site_toml="$(kubectl get configmap nico-api-site-config-files -n nico-system \
            -o jsonpath='{.data.nico-api-site-config\.toml}' 2>/dev/null || true)"
        if [[ -n "${_live_site_toml}" ]] \
           && [[ "$(_dpf_site_enabled <(printf '%s\n' "${_live_site_toml}"))" == "true" ]]; then
            _dpf_already_on=true
        fi
        if "${_dpf_already_on}"; then
            echo "DPF already enabled in the live site config — skipping the DPF-off down-cycle;"
            echo "the BMC-root credential is refreshed in phase 6b (idempotent re-run)."
            _CORE_VALUES_ARG="${_DPF_ON_VALUES}"
        else
            # Deploy the DPF-OFF version in this phase; phase 6b upgrades to DPF-ON.
            _CORE_VALUES_ARG="${_DPF_OFF_VALUES}"
        fi
    fi

    NICO_CORE_CMD=(
        helm upgrade --install nico ./helm
        --namespace nico-system
        -f "${_CORE_VALUES_ARG}"
        --set-string "global.image.repository=${NICO_IMAGE_REGISTRY}/nvmetal-carbide"
        --set-string "global.image.tag=${NICO_CORE_IMAGE_TAG}"
        --timeout 600s --wait
    )
    if "${INSTALL_DPF}"; then
        # Create the nico-api-dpf Role/RoleBinding in dpf-operator-system so
        # carbide-api can manage DPF CRs (chart template dpf-rbac.yaml).
        NICO_CORE_CMD+=(--set "nico-api.dpf.rbacCreate=true")
    fi
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
    echo "    global.image.repository   — ${NICO_IMAGE_REGISTRY}/nvmetal-carbide"
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

        # -------------------------------------------------------------------
        # 6b. DPF enablement in carbide-api. Core is up with DPF OFF; now set
        #     the site-wide BMC root password (required by the DPF SDK) via
        #     nico-admin-cli, then upgrade Core to DPF ON, which rolls
        #     carbide-api so it initializes the DPF SDK.
        # -------------------------------------------------------------------
        if "${INSTALL_DPF}" && "${_dpf_already_on:-false}"; then
            # Skip the destructive DPF-off down-cycle. If a BMC root password
            # was supplied (e.g. a rotation), reconcile it via nico-admin-cli;
            # otherwise carbide-api's 60 s refresh task will pick it up from
            # Vault automatically — no action needed here.
            _SETUP_PHASE="[6b] DPF already enabled — refreshing BMC-root credential"
            echo "=== [6b] DPF already enabled — refreshing BMC-root credential ==="
            kubectl rollout status deployment/nico-api -n nico-system --timeout=300s
            if [[ -n "${NICO_DPF_BMC_ROOT_PASSWORD}" ]]; then
                _dpf_set_bmc_root
            else
                echo "NICO_DPF_BMC_ROOT_PASSWORD not set — skipping BMC-root reconcile (carbide-api refresh task handles rotation automatically)."
            fi
        elif "${INSTALL_DPF}"; then
            _SETUP_PHASE="[6b] DPF enablement"
            echo "=== [6b] Enabling DPF in carbide-api ==="
            kubectl rollout status deployment/nico-api -n nico-system --timeout=300s
            if [[ -n "${NICO_DPF_BMC_ROOT_PASSWORD}" ]]; then
                _dpf_set_bmc_root
            else
                echo "NICO_DPF_BMC_ROOT_PASSWORD not set — skipping BMC-root credential setup."
                echo "Set the site-wide BMC root via: nico-admin-cli credential add-bmc --kind=site-wide-root --password='<password>'"
                echo "carbide-api will pick it up within 60 s of it being set."
            fi
            echo "Upgrading NICo Core to enable [dpf]..."
            (cd "${SCRIPT_DIR}/.." && helm upgrade --install nico ./helm \
                --namespace nico-system -f "${_DPF_ON_VALUES}" \
                --set-string "global.image.repository=${NICO_IMAGE_REGISTRY}/nvmetal-carbide" \
                --set-string "global.image.tag=${NICO_CORE_IMAGE_TAG}" \
                --set "nico-api.dpf.rbacCreate=true" \
                --timeout 600s --wait)
            # The helm upgrade only rewrites the site-config ConfigMap; the
            # nico-api pod template is unchanged, so it does NOT roll on its own
            # and carbide-api keeps its in-memory DPF-off config ([dpf] is read
            # at startup only). Force a restart so it re-reads [dpf].enabled=true
            # and creates the DPF initialization objects (provisioning source,
            # DPUFlavor, and DPUDeployment).
            echo "Restarting carbide-api so it reads the DPF-enabled config..."
            kubectl rollout restart deployment/nico-api -n nico-system
            kubectl rollout status deployment/nico-api -n nico-system --timeout=300s
            echo "DPF enabled in carbide-api"
        fi
    elif "${INSTALL_DPF}"; then
        # The DPF path deploys from a mktemp values file that the EXIT trap
        # deletes, and enablement is a two-phase flow (deploy DPF-off, set the
        # site-wide BMC root password, re-deploy DPF-on) that can't be reduced to
        # a single command — so point back at setup.sh rather than print a stale
        # helm command the operator can't actually run.
        echo "Skipped. Re-run setup.sh to deploy NICo Core (DPF enablement is a two-phase"
        echo "flow driven by this script), or pass --skip-dpf to deploy without DPF."
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
# Observability (optional) — local Loki + Tempo + OTEL collector +
# kube-prometheus-stack. Runs BEFORE the REST section so infra-only /
# --skip-rest installs still get monitoring. Self-contained and idempotent;
# can also be run standalone at any later time:
#   helm-prereqs/observability/install-observability.sh
# Docs: helm-prereqs/observability/README.md
# ---------------------------------------------------------------------------
_OBSERVABILITY_INSTALLED=false
if "${WITH_OBSERVABILITY}"; then
    echo ""
    _SETUP_PHASE="observability"
    echo "=== Observability (--with-observability) ==="
    # The stack is optional: a failure here must not abort the rest of the install.
    # NICO_SERVICEMONITORS=true is safe in this integrated path — Core was just installed
    # from this same tree, so the release upgrade the installer performs is a no-op apart
    # from adding the monitor objects.
    if NICO_SERVICEMONITORS="${NICO_SERVICEMONITORS:-true}" \
        "${SCRIPT_DIR}/observability/install-observability.sh"; then
        _OBSERVABILITY_INSTALLED=true
    else
        echo "WARNING: observability install failed (optional component) — continuing."
        echo "         Re-run it any time: ${SCRIPT_DIR}/observability/install-observability.sh"
    fi
else
    echo ""
    echo "=== Observability — skipped (pass --with-observability or run observability/install-observability.sh later) ==="
fi

# The initial guard proved that both the desired Deployment and active Flow
# Pods are manager-free. After preflight and the Core phase completes or is
# skipped, revoke predecessor hook credentials and RBAC before --skip-rest can
# exit. Fresh installs have no legacy markers, so this is a read-only no-op.
"${SCRIPT_DIR}/cleanup-legacy-flow-managers.sh"

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
#      7h. NICo Flow
#      7i. NICo REST site-agent
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
# Legacy standalone postgres StatefulSet with pre-initialised databases:
# nico (orphaned — no live component targets it), temporal, temporal_visibility,
# keycloak. Still the default target for both — see "Consolidating
# Temporal/Keycloak onto nico-pg-cluster" in README.md for the opt-in path.
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
_KC_ENABLED="$(_yaml_toplevel_value "${SCRIPT_DIR}/values/nico-rest.yaml" keycloak enabled)"
[[ "${_KC_ENABLED}" == "true" ]] || _KC_ENABLED="false"

if [[ "${_KC_ENABLED}" == "true" ]]; then
    echo "=== [7d/7] Keycloak ==="

    # helm-prereqs/values.yaml::keycloak.useHaPostgres — DB consolidation opt-in,
    # distinct from nico-rest.yaml's keycloak.enabled (deployed at all) above.
    _KC_DB_CONSOLIDATED="$(_yaml_toplevel_value "${SCRIPT_DIR}/values.yaml" keycloak useHaPostgres)"
    [[ "${_KC_DB_CONSOLIDATED}" == "true" ]] || _KC_DB_CONSOLIDATED="false"
    # keycloak.namespace — must match what eso-external-secrets.yaml's
    # nico-keycloak-db-eso targets, so KEYCLOAK_NS (read by keycloak/setup.sh)
    # agrees with it. An operator-supplied KEYCLOAK_NS env var still wins, same
    # as every other script in this feature (keycloak/setup.sh, clean.sh,
    # migrate-temporal-keycloak-db.sh) — don't clobber it with the values.yaml
    # default.
    if [[ -z "${KEYCLOAK_NS:-}" ]]; then
        export KEYCLOAK_NS="$(_yaml_toplevel_value "${SCRIPT_DIR}/values.yaml" keycloak namespace)"
        export KEYCLOAK_NS="${KEYCLOAK_NS:-nico-rest}"
    fi

    if [[ "${_KC_DB_CONSOLIDATED}" == "true" ]]; then
        echo "Waiting for Keycloak DB credentials to be synced by ESO (nico-keycloak-pg-creds in ${KEYCLOAK_NS})..."
        for _kc_i in $(seq 1 24); do
            if kubectl get secret nico-keycloak-pg-creds -n "${KEYCLOAK_NS}" &>/dev/null; then
                break
            fi
            if [[ "${_kc_i}" -eq 24 ]]; then
                echo "ERROR: nico-keycloak-pg-creds not synced after 120s." >&2
                echo "  Check: kubectl describe clusterexternalsecret nico-keycloak-db-eso" >&2
                echo "  Ensure keycloak.useHaPostgres=true in helm-prereqs/values.yaml." >&2
                exit 1
            fi
            echo "  nico-keycloak-pg-creds not yet synced (${_kc_i}/24) — retrying in 5s..."
            sleep 5
        done
        echo "Keycloak DB credentials ready — targeting nico-pg-cluster"
        # Reference the ESO-synced Secret directly (secretKeyRef in
        # deployment.yaml) rather than reading the password into this shell —
        # it never needs to touch a plaintext env value or process argument.
        export KEYCLOAK_DB_HOST="nico-pg-cluster.postgres.svc.cluster.local"
        export KEYCLOAK_DB_NAME="keycloak"
        export KEYCLOAK_DB_USER="keycloak.nico"
        # nico-pg-cluster's pg_hba.conf requires TLS (see the tls.enabled note
        # on the Temporal override below) — "require" encrypts the connection.
        # It does not verify the server certificate/hostname; that needs a
        # truststore wired to the operator's CA, not done here.
        export KEYCLOAK_DB_SSLMODE="require"
        export KEYCLOAK_DB_PASSWORD_SECRET_NAME="nico-keycloak-pg-creds"
        export KEYCLOAK_DB_PASSWORD_SECRET_KEY="password"
    fi

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
# helm-prereqs/values.yaml::temporal.useHaPostgres — see README's "Consolidating
# Temporal/Keycloak onto nico-pg-cluster" for the transition story and
# helm-prereqs/scripts/migrate-temporal-keycloak-db.sh for moving existing
# workflow history over.
_SETUP_PHASE="[7f/7] Temporal"
echo "=== [7f/7] Temporal ==="

_TEMPORAL_DB_CONSOLIDATED="$(_yaml_toplevel_value "${SCRIPT_DIR}/values.yaml" temporal useHaPostgres)"
[[ "${_TEMPORAL_DB_CONSOLIDATED}" == "true" ]] || _TEMPORAL_DB_CONSOLIDATED="false"

TEMPORAL_CMD=(
    helm upgrade --install temporal "${NICO_REST_DIR}/temporal-helm/temporal"
    --namespace temporal
    -f "${NICO_REST_DIR}/temporal-helm/temporal/values-kind.yaml"
    --timeout 300s --wait
)

if [[ "${_TEMPORAL_DB_CONSOLIDATED}" == "true" ]]; then
    echo "Waiting for Temporal DB credentials to be synced by ESO (nico-temporal-pg-creds in temporal)..."
    for _tp_i in $(seq 1 24); do
        if kubectl get secret nico-temporal-pg-creds -n temporal &>/dev/null; then
            break
        fi
        if [[ "${_tp_i}" -eq 24 ]]; then
            echo "ERROR: nico-temporal-pg-creds not synced after 120s." >&2
            echo "  Check: kubectl describe clusterexternalsecret nico-temporal-db-eso" >&2
            echo "  Ensure temporal.useHaPostgres=true in helm-prereqs/values.yaml." >&2
            exit 1
        fi
        echo "  nico-temporal-pg-creds not yet synced (${_tp_i}/24) — retrying in 5s..."
        sleep 5
    done
    echo "Temporal DB credentials ready — targeting nico-pg-cluster"

    # Mirror the nico-rest-workflow worker fix (#3284): override the chart's
    # postgres.postgres defaults at install time rather than editing
    # values-kind.yaml in place, so the legacy target keeps working unchanged
    # for sites that haven't opted in. nico-temporal-pg-creds (synced by ESO
    # above) already carries a "password" key, matching the chart's
    # persistence.secretKey default — no need to copy it into another Secret.
    #
    # tls.enabled: true is required — nico-pg-cluster's pg_hba.conf only
    # accepts encrypted connections (Zalando operator default), and any
    # unrecognized key under persistence.default/visibility.sql passes
    # straight through into the rendered SQL persistence config
    # (server-configmap.yaml), so this is the supported way to turn it on.
    # Without it, temporal-schema-update crash-loops on every attempt with
    # "pg_hba.conf rejects connection ... no encryption" (verified on dev6).
    _TEMPORAL_CREDS_FILE="$(mktemp)"
    cat > "${_TEMPORAL_CREDS_FILE}" <<EOF
server:
  config:
    persistence:
      secretName: "nico-temporal-pg-creds"
      default:
        sql:
          host: "nico-pg-cluster.postgres.svc.cluster.local"
          database: "temporal"
          user: "temporal.nico"
          existingSecret: "nico-temporal-pg-creds"
          tls:
            enabled: true
      visibility:
        sql:
          host: "nico-pg-cluster.postgres.svc.cluster.local"
          database: "temporal_visibility"
          user: "temporal.nico"
          existingSecret: "nico-temporal-pg-creds"
          tls:
            enabled: true
EOF
    TEMPORAL_CMD+=(-f "${_TEMPORAL_CREDS_FILE}")
fi

"${TEMPORAL_CMD[@]}"
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

    # Idempotency fast-path: skip creation when the namespace already exists.
    # Any describe failure (not-found or transient) falls through to create,
    # which propagates genuine errors with diagnostics below.
    if kubectl exec -n temporal deploy/temporal-admintools -- \
        sh -c "temporal operator namespace describe -n \"\$1\" --address ${_TEMPORAL_ADDR} ${_TEMPORAL_TLS}" \
        sh "${_namespace}" >/dev/null 2>&1; then
        echo "Temporal namespace ${_namespace} already exists"
        return
    fi

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
# Flow is the task, policy, and automation service. It runs in its own `flow`
# namespace and serves gRPC on port 50051.
#
# Runs BEFORE the site-agent (7i) so that flow.flow.svc.cluster.local:50051
# exists when the site-agent starts and attempts its Flow gRPC connection.
#
# Prerequisites already in place by this point:
#   - flow database on nico-pg-cluster (helm-prereqs postgresql.yaml)
#   - flow.nico DB credentials synced into the flow namespace by the
#     flow-db-eso ClusterExternalSecret
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
        ## Flow ships on the same image release line as NICo REST, so reuse
        ## NICO_REST_IMAGE_TAG rather than NICO_CORE_IMAGE_TAG.
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

    # Annotate/label the namespace itself so the Flow release can adopt the
    # namespace created before the main helm install.
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

    # Wait for the Flow DB credential ESO sync to land. Fail fast if the Secret
    # never appears instead of allowing the helm install to enter an opaque
    # FailedMount loop.
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

    echo "Waiting for Flow DB credentials..."
    _wait_for_secret "flow.nico.nico-pg-cluster.credentials" \
        "${NICO_FLOW_NAMESPACE}" \
        "Synced by the flow-db-eso ClusterExternalSecret in nico-prereqs. Check 'kubectl describe clusterexternalsecret flow-db-eso' and confirm helm-prereqs/values.yaml::flow.enabled=true."

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
if "${_OBSERVABILITY_INSTALLED}"; then
    echo "  Grafana (observability): kubectl -n monitoring port-forward svc/obs-grafana 3000:80"
fi
echo "========================================================================="

_SETUP_PHASE="complete"  # signals _on_failure trap: clean exit, no prompt needed
