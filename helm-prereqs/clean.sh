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
# clean.sh — full teardown, inverse of setup.sh
#
# Destroys in reverse order:
#   0. NCX stack           (nico-rest helm, temporal, keycloak, ncx postgres)
#   1. nico core        (separate helm release, if installed)
#   1b. DPF stack          (DPF CRs, dpf-operator helm release — if installed)
#   2. helmfile releases   (nico-prereqs, external-secrets, vault, cert-manager,
#                           postgres-operator, DPF prereqs when installed)
#   3. cluster-scoped hook resources (ClusterIssuers, ClusterSecretStore, etc.)
#   4. vault init secrets  (vault-cluster-keys, vaultunsealkeys, vaultroottoken)
#   5. namespaces          (nico-system, cert-manager, vault, external-secrets,
#                           postgres, dpf-operator-system)
#   6. local-path-persistent PVs owned by this stack (Retain policy — not deleted with namespace;
#                           flipped to Delete first so the provisioner reclaims the host directory)
#   7. local-path-provisioner + StorageClass (applied via kubectl, not helm-managed)
#   8. orphaned /opt/local-path-provisioner directories on every node (per-node sweep pod)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

# ---------------------------------------------------------------------------
# 0. NCX stack — uninstall before nico since it depends on nico's
#    cert-manager and ClusterIssuers.
# ---------------------------------------------------------------------------
echo "=== [0/8] Uninstalling NICo REST stack ==="
# Flow goes first — it talks to Temporal + nico-api and depends on credentials
# from both nico-prereqs (DB creds, vault tokens) and the REST stack.
helm uninstall flow                 -n flow                            2>/dev/null || true
kubectl delete ns flow --wait=false --ignore-not-found                 2>/dev/null || true
# --no-hooks: the rack-manager chart ships a pre-delete drop-database hook
# (disabled in values/rms.yaml, but a hand-installed release may have it on)
# that pulls postgres:15-alpine from Docker Hub — a mirror-only site would
# hang here, and clean.sh drops the whole postgres namespace anyway.
helm uninstall rack --no-hooks      -n rack-manager                    2>/dev/null || true
kubectl delete ns rack-manager --wait=false --ignore-not-found         2>/dev/null || true
helm uninstall nico-rest-site-agent -n nico-rest                       2>/dev/null || true
helm uninstall nico-rest            -n nico-rest                       2>/dev/null || true
helm uninstall temporal                -n temporal     2>/dev/null || true

if kubectl get deploy keycloak -n nico-rest &>/dev/null; then
    echo "  Cleaning up Keycloak..."
    "${SCRIPT_DIR}/keycloak/clean.sh" 2>/dev/null || true
else
    echo "  Keycloak not deployed — skipping cleanup"
fi

kubectl delete clusterissuer nico-rest-ca-issuer --ignore-not-found 2>/dev/null || true
kubectl delete ns nico-rest temporal flow \
    --wait=false --ignore-not-found 2>/dev/null || true
echo "Waiting for nico-rest, temporal, and flow namespaces to terminate..."
kubectl wait --for=delete ns/nico-rest ns/temporal ns/flow \
    --timeout=120s 2>/dev/null || true

# ---------------------------------------------------------------------------
# 0b. Observability stack (optional component; harmless no-ops if absent)
# ---------------------------------------------------------------------------
echo "=== [0b/8] Uninstalling observability stack (if present) ==="
helm uninstall obs                    -n monitoring 2>/dev/null || true
helm uninstall otel-agent             -n otel       2>/dev/null || true
helm uninstall otel-collector-gateway -n otel       2>/dev/null || true
helm uninstall tempo                  -n tempo      2>/dev/null || true
helm uninstall loki                   -n loki       2>/dev/null || true
kubectl delete ns monitoring otel tempo loki \
    --wait=false --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 1. NICo core (separate helm release, not in helmfile)
# ---------------------------------------------------------------------------
echo "=== [1/8] Uninstalling nico core ==="
helm uninstall nico -n nico-system 2>/dev/null || true

# ---------------------------------------------------------------------------
# 1b. DPF stack (setup.sh, DPF default), skipped fast when never installed.
#     Order matters: DPF CRs must be deleted while the operator, Kamaji, and
#     Argo CD controllers are still running so their finalizers can complete.
#     The four prereq releases (argo-cd, kamaji, maintenance-operator, NFD)
#     ride the helmfile destroy in step 2.
# ---------------------------------------------------------------------------
if kubectl get namespace dpf-operator-system &>/dev/null; then
    echo "=== [1b] Removing DPF stack ==="

    # DPU-facing CRs first (service chain, then provisioning objects), while
    # the controllers can still finalize them.
    kubectl delete dpudeployments,dpuservicechains,dpuservices,dpusets \
        --all -n dpf-operator-system --timeout=120s 2>/dev/null || true
    kubectl delete dpuserviceinterfaces,dpuservicetemplates,dpuserviceconfigurations,dpuservicenads \
        --all -n dpf-operator-system --timeout=120s 2>/dev/null || true
    # NOTE: if DPUs are mid-provisioning (OSInstall / ConfigureFirmware state)
    # this deletion will interrupt them and may leave them in an inconsistent
    # state. Confirm no active provisioning before running clean.sh on a live
    # site. `dpu` and `dpunodemaintenances` are omitted here: they carry owner
    # references to DpuNode and are garbage-collected automatically when
    # DpuNode is deleted.
    kubectl delete dpudevices,dpunodes \
        --all -n dpf-operator-system --timeout=180s 2>/dev/null || true
    kubectl delete bfbs,bluefieldsoftwares,dpuflavors \
        --all -n dpf-operator-system --timeout=120s 2>/dev/null || true
    kubectl delete dpfoperatorconfig --all -n dpf-operator-system \
        --timeout=120s 2>/dev/null || true
    # DPUCluster last among CRs — deleting it tears down the Kamaji
    # TenantControlPlane behind the DPU cluster.
    kubectl delete dpucluster --all -n dpf-operator-system \
        --timeout=180s 2>/dev/null || true
    kubectl delete tenantcontrolplane --all -A --timeout=180s 2>/dev/null || true
    # Kamaji `default` DataStore: carries helm.sh/resource-policy:keep (so
    # helmfile destroy won't remove it) and a kamaji finalizer. Delete it here,
    # while the kamaji controller is still alive to finalize it — otherwise its
    # finalizer blocks the datastores CRD deletion and namespace termination.
    kubectl delete datastores.kamaji.clastix.io --all -A \
        --timeout=120s 2>/dev/null || true
    # Argo CD Applications carry resources-finalizer.argocd.argoproj.io, which
    # cascades a delete onto the (now-gone) DPU cluster and cannot complete on a
    # DPU-less / VIP-unroutable cluster. Delete best-effort; stragglers get their
    # finalizers stripped below before argo-cd itself is destroyed.
    kubectl delete applications.argoproj.io --all -A \
        --timeout=60s 2>/dev/null || true

    # Best-effort finalizer strip for anything stuck after the bounded waits.
    # Cover every DPF CR kind here — plus the kamaji DataStore and the Argo CD
    # Applications/AppProjects — because `kubectl delete crd` (step 2) blocks
    # until all instances finalize, so any kind left finalizer-stuck would hang
    # the whole teardown. Applications must be stripped before argo-cd is
    # destroyed (below), or nothing can ever clear their cascade finalizer.
    for _kind in dpudeployments dpuservices dpuservicechains dpuserviceinterfaces \
                 dpuservicetemplates dpuserviceconfigurations dpuservicenads \
                 dpus dpudevices dpunodes dpunodemaintenances dpusets \
                 bfbs bluefieldsoftwares dpuflavors \
                 dpfoperatorconfigs dpuclusters tenantcontrolplanes \
                 datastores.kamaji.clastix.io \
                 applications.argoproj.io appprojects.argoproj.io; do
        # Discover with the namespace so the patch targets the resource's ACTUAL
        # namespace (a bare `-o name` + hard-coded `-n dpf-operator-system` would
        # silently miss anything outside it). Cluster-scoped kinds (e.g.
        # datastores) report an empty namespace and are patched without -n.
        # Read the whole line and split on the tab manually — `read` with
        # IFS=tab would strip the leading tab of a cluster-scoped resource's
        # empty namespace and drop it (the DataStore is exactly such a resource).
        while IFS= read -r _line; do
            _ns="${_line%%$'\t'*}"; _name="${_line#*$'\t'}"
            [[ -z "${_name}" ]] && continue
            echo "WARNING: force-removing finalizers on stuck ${_kind}/${_name}${_ns:+ (ns ${_ns})}"
            kubectl patch "${_kind}/${_name}" ${_ns:+-n "${_ns}"} --type merge \
                -p '{"metadata":{"finalizers":[]}}' 2>/dev/null || true
        done < <(kubectl get "${_kind}" -A \
            -o jsonpath='{range .items[*]}{.metadata.namespace}{"\t"}{.metadata.name}{"\n"}{end}' 2>/dev/null)
    done

    helm uninstall dpf-operator -n dpf-operator-system 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# 2. All helmfile releases in reverse dependency order:
#    nico-prereqs → node-feature-discovery → maintenance-operator → kamaji →
#    argo-cd → external-secrets → vault → cert-manager → metallb
# ---------------------------------------------------------------------------
echo "=== [2/8] Destroying helmfile releases ==="

# Delete MetalLB site config resources BEFORE helmfile destroys the operator.
# The CRD instances (IPAddressPool, BGPPeer, etc.) are in metallb-system and
# must be removed while the webhook is still running to avoid stuck finalizers.
echo "Removing MetalLB site config resources..."
kubectl delete bgpadvertisement,l2advertisement --all \
    -n metallb-system --ignore-not-found 2>/dev/null || true
kubectl delete bgppeer --all \
    -n metallb-system --ignore-not-found 2>/dev/null || true
kubectl delete ipaddresspool --all \
    -n metallb-system --ignore-not-found 2>/dev/null || true

helmfile destroy 2>/dev/null || true

# MetalLB CRDs — helm does not delete CRDs on uninstall.
echo "Removing MetalLB CRDs..."
kubectl get crd -o name | grep metallb.io \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true

# Helm does NOT delete CRDs on uninstall (to prevent accidental data loss).
# Delete postgres-operator CRDs explicitly so a subsequent setup.sh can
# reinstall them cleanly — especially important when they were previously
# managed by a different field manager (e.g. ArgoCD) which causes SSA conflicts.
echo "Removing postgres-operator CRDs and cluster-scoped RBAC..."
kubectl delete crd \
    operatorconfigurations.acid.zalan.do \
    postgresqls.acid.zalan.do \
    postgresteams.acid.zalan.do \
    --ignore-not-found 2>/dev/null || true
kubectl delete clusterrole postgres-operator postgres-pod \
    --ignore-not-found 2>/dev/null || true
kubectl delete clusterrolebinding postgres-operator \
    --ignore-not-found 2>/dev/null || true

# cert-manager CRDs, webhooks, and cluster-scoped RBAC.
# Helm does not delete CRDs on uninstall, and kustomize/ArgoCD deployments leave
# behind cluster-scoped resources without Helm ownership annotations, causing
# "cannot be imported into the current release" errors on reinstall.
echo "Removing cert-manager CRDs, webhooks, and cluster-scoped RBAC..."
kubectl get crd -o name | grep cert-manager \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
kubectl get clusterrole,clusterrolebinding -o name \
    | grep cert-manager \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
kubectl delete mutatingwebhookconfiguration cert-manager-webhook \
    --ignore-not-found 2>/dev/null || true
kubectl delete validatingwebhookconfiguration cert-manager-webhook cert-manager-approver-policy \
    --ignore-not-found 2>/dev/null || true

# external-secrets CRDs and webhooks
echo "Removing external-secrets CRDs and webhooks..."
kubectl get crd -o name | grep external-secrets.io \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
kubectl get clusterrole,clusterrolebinding -o name \
    | grep -E "external-secrets|^clusterrole.*/eso-|^clusterrolebinding.*/eso-" \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
kubectl delete validatingwebhookconfiguration externalsecret-validate secretstore-validate \
    --ignore-not-found 2>/dev/null || true

# Prometheus Operator CRDs that setup.sh applies from operators/crds/ (servicemonitors,
# podmonitors, prometheusrules, scrapeconfigs). Helm/kubectl-apply leave these behind, so
# remove them for a complete wipe. NOTE: skip this if the cluster has its own cluster-level
# Prometheus Operator that owns these CRDs.
echo "Removing Prometheus Operator (monitoring.coreos.com) CRDs..."
kubectl get crd -o name | grep monitoring.coreos.com \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true

# vault cluster-scoped RBAC and webhooks
echo "Removing vault cluster-scoped RBAC and webhooks..."
kubectl get clusterrole,clusterrolebinding -o name \
    | grep -E "vault-agent-injector|vault-server-binding" \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
kubectl delete mutatingwebhookconfiguration vault-agent-injector-cfg \
    --ignore-not-found 2>/dev/null || true

# nico-rest cluster-scoped RBAC (ClusterRole/Binding created by the nico-rest
# umbrella chart — not cleaned up by helm uninstall if originally deployed by ArgoCD)
echo "Removing nico-rest cluster-scoped RBAC..."
kubectl get clusterrole,clusterrolebinding -o name \
    | grep nico-rest \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true

# DPF stack CRDs and cluster-scoped leftovers (DPF is installed by default).
# Helm does not delete CRDs on uninstall; the prereq operators (argo-cd,
# kamaji, maintenance-operator, NFD) also leave their CRDs and cluster RBAC.
# NOTE: match only Argo *CD* CRDs (applications/appprojects/applicationsets),
# not the whole argoproj.io group — a cluster running Argo Workflows would
# otherwise have its workflows.argoproj.io CRDs (and live objects) deleted.
# --timeout bounds the delete: `kubectl delete crd` blocks on CR finalizers
# (the strip loop in step 1b handles those, but bound it anyway).
echo "Removing DPF, Argo CD, Kamaji, NFD, and maintenance-operator CRDs..."
kubectl get crd -o name \
    | grep -E '\.dpu\.nvidia\.com|(applications|appprojects|applicationsets)\.argoproj\.io|kamaji\.clastix\.io|nfd\.k8s-sigs\.io|maintenance\.nvidia\.com' \
    | xargs -r kubectl delete --ignore-not-found --timeout=120s 2>/dev/null || true
kubectl get clusterrole,clusterrolebinding -o name \
    | grep -E "dpf-operator|argo-cd|argocd|kamaji|maintenance-operator|node-feature-discovery" \
    | xargs kubectl delete --ignore-not-found 2>/dev/null || true
# CertificateRequestPolicy + RBAC (only present on approver-policy clusters)
kubectl delete certificaterequestpolicy dpf-approval-policy \
    --ignore-not-found 2>/dev/null || true
kubectl delete "clusterrole/cert-manager-policy:dpf-approval-policy" \
    "clusterrolebinding/cert-manager-policy:dpf-approval-policy" \
    --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 3. Cluster-scoped resources created by helm hooks.
#    These survive helm/helmfile uninstall because hook-delete-policy is
#    "before-hook-creation" (cleans up on next install, not on uninstall).
# ---------------------------------------------------------------------------
echo "=== [3/8] Removing cluster-scoped hook resources ==="
kubectl delete clusterissuer \
    vault-nico-issuer site-issuer selfsigned-bootstrap \
    --ignore-not-found 2>/dev/null || true
kubectl delete clustersecretstore \
    cert-manager-ns-secretstore postgres-ns-secretstore \
    --ignore-not-found 2>/dev/null || true
# Keep removed PSM/NSM hook names in cleanup so upgrades from older releases do
# not strand their cluster-scoped resources.
kubectl delete clusterexternalsecret \
    nico-roots-eso nico-db-eso \
    flow-db-eso psm-db-eso nsm-db-eso rms-db-eso \
    --ignore-not-found 2>/dev/null || true
kubectl delete clusterrole \
    vault-pki-config-reader eso-postgres-ns-role flow-vault-tokens-writer \
    --ignore-not-found 2>/dev/null || true
kubectl delete clusterrolebinding \
    vault-pki-config-reader eso-postgres-ns-rolebinding flow-vault-tokens-writer \
    --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 4. Vault init secrets (written by unseal_vault.sh, not owned by helm)
# ---------------------------------------------------------------------------
echo "=== [4/8] Removing Vault init secrets ==="
kubectl delete secret vault-cluster-keys vaultunsealkeys vaultroottoken \
    -n vault --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 5. Namespaces — helm/helmfile does not delete namespaces on uninstall.
#    Deleting the namespace also deletes all PVCs inside it.
#
#    default namespace cannot be deleted but must be purged: ArgoCD may
#    have deployed ESO (external-secrets) directly into default, leaving
#    behind deployments, services, secrets, and serviceaccounts that
#    conflict with setup.sh's helmfile install into the external-secrets ns.
# ---------------------------------------------------------------------------
echo "=== [5/8] Deleting namespaces ==="
kubectl delete ns nico-system cert-manager vault external-secrets postgres metallb-system dpf-operator-system \
    --wait=false --ignore-not-found 2>/dev/null || true

echo "Waiting for namespaces to terminate..."
kubectl wait --for=delete \
    ns/nico-system ns/cert-manager ns/vault ns/external-secrets ns/postgres ns/metallb-system ns/dpf-operator-system \
    --timeout=180s 2>/dev/null || true

echo "Purging default namespace (ESO and other non-kubespray resources)..."
kubectl delete deployment,replicaset,pod,service,secret,serviceaccount,configmap \
    -n default \
    -l "app.kubernetes.io/name=external-secrets" \
    --ignore-not-found 2>/dev/null || true
# Also remove any lingering ESO webhook secret and nico secrets by name
kubectl delete secret external-secrets-webhook nico-root nico-roots \
    -n default --ignore-not-found 2>/dev/null || true
kubectl delete serviceaccount argo-workflow eso-default-ns \
    external-secrets external-secrets-cert-controller external-secrets-webhook \
    -n default --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 5b. Preflight pods — per-node check pods left in kube-system by preflight.sh.
#     Labeled ncx-preflight=true; cleaned here (not by preflight.sh) so they
#     accumulate across runs and are only removed on explicit teardown.
# ---------------------------------------------------------------------------
echo "Removing preflight check pods..."
kubectl delete pod -n kube-system -l ncx-preflight=true \
    --ignore-not-found 2>/dev/null || true

# ---------------------------------------------------------------------------
# 6. Vault PersistentVolumes — StorageClass has reclaimPolicy: Retain, so
#    PVs are NOT deleted when PVCs are deleted (they go to "Released" state).
#    Delete them explicitly for a clean reinstall.
#    Scoped to namespaces owned by this stack to avoid removing PVs belonging
#    to other components that share the local-path-persistent StorageClass.
#
#    Retain also means the provisioner never runs its teardown helper pod, so
#    deleting the PV object alone leaves the backing directory on the node
#    (/opt/local-path-provisioner/pvc-<uid>_<ns>_<pvc>) behind forever. Patch
#    each PV to reclaimPolicy Delete first so local-path-provisioner reclaims
#    the host directory, then wait for the PVs to actually disappear — the
#    provisioner must still be running (it is removed in step 7).
#
#    Only local-path-persistent (Retain) PVs need this. PVs on the plain
#    local-path StorageClass are already reclaimPolicy Delete and are reclaimed
#    when step 5 deletes their namespace; if one is still in flight when step 7
#    removes the provisioner, the step 8 sweep picks up its directory.
# ---------------------------------------------------------------------------
echo "=== [6/8] Removing Released PersistentVolumes owned by this stack ==="

# Host path backing the provisioner, used by the orphan sweep in step 8.
# Sources in order of trust: an explicit override, the provisioner's own
# ConfigMap, then the path of a surviving local-path PV. The last one matters
# on a re-run: once the provisioner is uninstalled its ConfigMap is gone too,
# and blindly assuming the chart default would sweep a directory this cluster
# never used while leaving the real orphans in place.
LOCAL_PATH_DIR="${LOCAL_PATH_DIR:-}"

_OTHER_PATHS=""
if [[ -z "${LOCAL_PATH_DIR}" ]]; then
    _LPP_CONFIG="$(kubectl get configmap local-path-config -n local-path-storage \
        -o jsonpath='{.data.config\.json}' 2>/dev/null || true)"
    LOCAL_PATH_DIR="$(printf '%s' "${_LPP_CONFIG}" \
        | jq -r '.nodePathMap[0].paths[0] // empty' 2>/dev/null || true)"
    # This stack configures a single DEFAULT path. A nodePathMap with per-node
    # entries or several paths is valid upstream but not something the sweep
    # resolves per node, so name what it will not reach instead of silently
    # cleaning one path and reporting success.
    _OTHER_PATHS="$(printf '%s' "${_LPP_CONFIG}" \
        | jq -r --arg first "${LOCAL_PATH_DIR}" \
            '[.nodePathMap[].paths[]] | unique - [$first] | join(" ")' \
        2>/dev/null || true)"
fi

if [[ -z "${LOCAL_PATH_DIR}" ]]; then
    # Match the exact classes this stack creates, not any name that happens to
    # begin with "local-path" — an unrelated class would point the sweep at a
    # different volume's directory tree.
    LOCAL_PATH_DIR="$(kubectl get pv -o json 2>/dev/null \
        | jq -r '[.items[]
            | select(.spec.storageClassName == "local-path-persistent"
                     or .spec.storageClassName == "local-path")
            | select((.metadata.annotations["pv.kubernetes.io/provisioned-by"] // "rancher.io/local-path")
                     == "rancher.io/local-path")
            | (.spec.hostPath.path // .spec.local.path // empty)][0] // empty' \
        2>/dev/null || true)"
    # PV path is <host dir>/<pvc dir>; the sweep wants the parent.
    [[ -n "${LOCAL_PATH_DIR}" ]] && LOCAL_PATH_DIR="$(dirname "${LOCAL_PATH_DIR}")"
fi
# Only a real absolute path is usable — it is hostPath-mounted into the sweep
# pod in step 8 and used as the root of an rm, so fall back to the chart default
# rather than trust a malformed value. "/" itself is rejected: mounting the node
# root into the sweep pod is never what the provisioner config meant.
_LOCAL_PATH_RAW="${LOCAL_PATH_DIR}"
while [[ "${LOCAL_PATH_DIR}" == */ ]]; do
    LOCAL_PATH_DIR="${LOCAL_PATH_DIR%/}"
done
# Whitespace is rejected along with the rest: this value is interpolated into
# the sweep pod manifest, where a newline would rewrite the YAML around it.
if [[ "${LOCAL_PATH_DIR}" != /?* || "${LOCAL_PATH_DIR}" == *".."* \
      || "${LOCAL_PATH_DIR}" == *[[:space:]]* ]]; then
    # Say so rather than silently sweeping somewhere else — a typo in an
    # explicit LOCAL_PATH_DIR would otherwise look like a successful run.
    # Report what was supplied, not the normalized form — "/" normalizes to
    # empty and would otherwise be rejected silently.
    [[ -n "${_LOCAL_PATH_RAW}" ]] && \
        echo "  WARNING: ignoring unusable host path '${_LOCAL_PATH_RAW}' — must be absolute, not '/', and contain no '..' or whitespace"
    LOCAL_PATH_DIR=""
fi

if [[ -z "${LOCAL_PATH_DIR}" ]]; then
    LOCAL_PATH_DIR="/opt/local-path-provisioner"
    _LOCAL_PATH_ASSUMED=true
else
    _LOCAL_PATH_ASSUMED=false
fi

# Namespaces owned by this stack. Both the PV selection below and the host
# directory sweep in step 8 are scoped to these so neither touches storage
# belonging to other components sharing the same StorageClass / host path.
#
# Keep this in step with the namespaces the steps above delete — a namespace
# missing here has its PVs left Retained and its host directories accumulating,
# which is the bug this file exists to fix. loki and tempo in particular hold
# real data. flow is included for the same reason even though it predated this
# list: step 0 deletes that namespace.
_STACK_NS="nico-system cert-manager vault external-secrets postgres \
metallb-system dpf-operator-system nico-rest temporal flow \
loki tempo monitoring otel"
_STACK_NS="$(printf '%s' "${_STACK_NS}" | tr -s '[:space:]' ' ')"
_STACK_NS_RE="^($(printf '%s' "${_STACK_NS}" | tr ' ' '|'))$"

# Bound PVs are excluded: their claim is still alive, which after step 5 means a
# namespace failed to terminate and something may still be writing. Both fallback
# paths below force-delete the PV object, and once it is gone the step 8 sweep can
# no longer tell that its directory was live — so the exclusion has to happen
# here, not there. Such a directory is left for the next teardown.
#
# A failed query (API error, missing jq) must not look like "no PVs to reclaim"
# and skip the block below silently — same handling as the sweep in step 8.
if ! _STACK_PVS="$(kubectl get pv -o json 2>/dev/null \
    | jq -r --arg ns_re "${_STACK_NS_RE}" '.items[] | select(
        .spec.storageClassName == "local-path-persistent" and
        .status.phase != "Bound" and
        (.spec.claimRef.namespace // "" | test($ns_re))
      ) | .metadata.name')"; then
    echo "  WARNING: could not list PersistentVolumes — retained PVs were not reclaimed"
    _STACK_PVS=""
fi

# Delete a PV, then clear any finalizer still holding it. An external-provisioner
# finalizer would keep it Terminating forever once step 7 removes the controller
# that clears it. Nothing Bound reaches either caller, so no live claim can be
# stranded by this.
_force_delete_pv() {
    kubectl delete pv "$1" --wait=false --ignore-not-found >/dev/null 2>&1 || true
    # Only strip finalizers from a PV the API server actually accepted for
    # deletion. Without the deletionTimestamp check, a delete refused by RBAC
    # would still have its finalizers removed, leaving a live PV defenceless.
    if [[ -n "$(kubectl get pv "$1" -o jsonpath='{.metadata.deletionTimestamp}' \
        2>/dev/null || true)" ]]; then
        kubectl patch pv "$1" -p '{"metadata":{"finalizers":null}}' \
            >/dev/null 2>&1 || true
    fi
}

if [[ -n "${_STACK_PVS}" && "${SKIP_HOSTPATH_SWEEP:-false}" == "true" ]]; then
    # SKIP_HOSTPATH_SWEEP promises the host directories are left alone. Flipping
    # the reclaim policy here would have the provisioner erase them regardless,
    # so honour the flag: delete the PV objects only (the pre-existing behaviour)
    # and leave the data on disk.
    echo "  SKIP_HOSTPATH_SWEEP=true — deleting PV objects only, host directories left in place"
    # Policy stays Retain, so this removes the object without touching the data.
    for _pv in ${_STACK_PVS}; do
        _force_delete_pv "${_pv}"
    done
    _STACK_PVS=""
fi

if [[ -n "${_STACK_PVS}" ]]; then
    # Flip Retain → Delete so the provisioner's teardown helper pod runs
    # `rm -rf $VOL_DIR` on the node that owns the data.
    for _pv in ${_STACK_PVS}; do
        kubectl patch pv "${_pv}" -p \
            '{"spec":{"persistentVolumeReclaimPolicy":"Delete"}}' \
            >/dev/null 2>&1 || true
    done

    # Only the provisioner can reclaim these, so waiting on it is pointless if
    # it has no available replica (never installed, already removed, scaled to
    # zero, crash-looping). Skip straight to the fallback instead of burning the
    # full timeout — the step 8 sweep is what recovers the directories then.
    _PROVISIONER_UP="$(kubectl get deploy local-path-provisioner \
        -n local-path-storage -o jsonpath='{.status.availableReplicas}' \
        2>/dev/null || true)"

    if [[ -z "${_PROVISIONER_UP}" || "${_PROVISIONER_UP}" == "0" ]]; then
        echo "  local-path-provisioner is not running — PV host directories are left to the step 8 sweep"
        for _pv in ${_STACK_PVS}; do
            _force_delete_pv "${_pv}"
        done
    else
        # Let the provisioner drive the deletion: a Released PV with policy
        # Delete is reclaimed (helper pod, then PV object removed) by the
        # provisioner itself. Deleting the PV object here instead would race it
        # and could drop the object before the host directory is touched.
        # One shared deadline for the whole set — a per-PV timeout would
        # multiply into 120s x PV count on exactly the failure path this
        # fallback exists to handle.
        echo "Waiting for local-path-provisioner to reclaim PV host directories..."
        # shellcheck disable=SC2046,SC2086
        if ! kubectl wait --for=delete $(printf 'pv/%s ' ${_STACK_PVS}) \
            --timeout=120s >/dev/null 2>&1; then
            for _pv in ${_STACK_PVS}; do
                if kubectl get pv "${_pv}" >/dev/null 2>&1; then
                    echo "  WARNING: pv/${_pv} did not finish reclaiming — step 8 sweeps its directory under ${LOCAL_PATH_DIR}"
                    # Remove the leftover object so a later setup.sh is not blocked.
                    _force_delete_pv "${_pv}"
                fi
            done
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 7. local-path-provisioner + StorageClass (applied via kubectl in setup.sh)
# ---------------------------------------------------------------------------
echo "=== [7/8] Removing local-path-provisioner ==="
kubectl delete -f operators/storageclass-local-path-persistent.yaml \
    --ignore-not-found 2>/dev/null || true
kubectl delete -f operators/local-path-provisioner.yaml \
    --ignore-not-found 2>/dev/null || true
kubectl delete ns local-path-storage --wait=false --ignore-not-found 2>/dev/null || true
kubectl wait --for=delete ns/local-path-storage --timeout=60s 2>/dev/null || true

# ---------------------------------------------------------------------------
# 8. Orphaned host directories under ${LOCAL_PATH_DIR}.
#
#    Directories left by earlier runs (before step 6 reclaimed properly, or by
#    a PV deleted while the provisioner was down) have no PV to reclaim them
#    and accumulate across test cycles. Sweep every node with a short-lived pod
#    that removes a directory only when BOTH hold:
#      - the namespace encoded in its name is one this stack owns, and
#      - no live PV of that name exists. A Bound PV counts as live and is left
#        alone; one that is Released, Failed, or Terminating-and-unbound does
#        not, since nothing can reclaim it once step 7 removed the provisioner.
#    Everything else on the shared host path is left untouched.
#
#    Override the sweep image for air-gapped clusters:
#      export CLEAN_SWEEP_IMAGE=my-registry.example.com/busybox:1.36
#    Skip the sweep entirely with:
#      export SKIP_HOSTPATH_SWEEP=true
# ---------------------------------------------------------------------------
echo "=== [8/8] Sweeping orphaned directories under ${LOCAL_PATH_DIR} ==="

# Nothing left on the cluster identified the path, so a clean "removed 0" here
# would be indistinguishable from sweeping a directory this cluster never used.
if [[ "${_LOCAL_PATH_ASSUMED}" == "true" ]]; then
    echo "  NOTE: could not read the provisioner host path from the cluster — assuming the default."
    echo "        If nodes use a different nodePathMap, re-run with LOCAL_PATH_DIR=/your/path"
fi
if [[ -n "${_OTHER_PATHS}" ]]; then
    echo "  NOTE: nodePathMap also configures ${_OTHER_PATHS} — not swept."
    echo "        Re-run with LOCAL_PATH_DIR=<path> for each to clean those too."
fi

# Clear pods stranded by an interrupted run first, on every path through this
# step. One of those still holds an outdated snapshot of which PVs were live, so
# leaving it alive would contradict SKIP_HOSTPATH_SWEEP and outlive the branch
# below that creates no pods of its own.
# Blocking, unlike the other pod deletions here: a stale pod that is already
# running could be mid-rm, and returning before it is gone would let it keep
# deleting directories after this step decided not to.
if ! kubectl delete pod -n kube-system -l nico-lpp-sweep=true \
    --ignore-not-found --grace-period=0 --wait --timeout=30s >/dev/null 2>&1; then
    echo "  WARNING: sweep pods from an earlier clean.sh did not terminate within 30s."
    echo "           They may still be deleting directories under ${LOCAL_PATH_DIR}:"
    echo "             kubectl delete pod -n kube-system -l nico-lpp-sweep=true"
fi

if [[ "${SKIP_HOSTPATH_SWEEP:-false}" == "true" ]]; then
    echo "  SKIP_HOSTPATH_SWEEP=true — skipping"
else
    # Interpolated into the pod manifest, same as LOCAL_PATH_DIR — reject values
    # that would rewrite the YAML around it.
    _SWEEP_IMAGE="${CLEAN_SWEEP_IMAGE:-busybox:1.36}"
    if [[ "${_SWEEP_IMAGE}" == *[[:space:]]* || "${_SWEEP_IMAGE}" == *'"'* ]]; then
        echo "  WARNING: ignoring unusable CLEAN_SWEEP_IMAGE '${_SWEEP_IMAGE}' — using busybox:1.36"
        _SWEEP_IMAGE="busybox:1.36"
    fi
    _SWEEP_NS="kube-system"
    _SWEEP_TS="$(date +%s)"

    # PVs whose directories are kept. Two categories are excluded, because
    # step 7 removed the provisioner and nothing can reclaim them any more:
    #   - Terminating and no longer Bound
    #   - Released or Failed (their claim is already gone, so no pod is using
    #     the data — this is what catches a plain local-path PV whose reclaim
    #     was still in flight when the provisioner went away)
    # A Bound PV is always kept, Terminating or not: if a stack namespace failed
    # to terminate in step 5, its claim — and a pod writing to it — may still be
    # alive, and rm -rf underneath that is worse than leaving the directory for
    # the next teardown.
    # Every Bound PV is kept regardless of StorageClass — NICO_STORAGE_CLASS lets
    # a site back these PVCs with a differently named class on the same
    # provisioner, and missing one of those would mean rm -rf under a live pod.
    # The non-Bound half is restricted to this provisioner's own classes: nothing
    # else can own a directory here, and this list goes into every sweep pod, so
    # a cluster-wide one bloats the manifest for no gain.
    # A failed query must NOT be treated as "no live PVs" (that would sweep every
    # directory, including live ones), so bail out if the API call errors.
    if ! _LIVE_PVS="$(kubectl get pv -o json 2>/dev/null \
        | jq -r '.items[] | select(
            .status.phase == "Bound" or
            ((.spec.storageClassName == "local-path-persistent"
              or .spec.storageClassName == "local-path")
             and .metadata.deletionTimestamp == null
             and .status.phase != "Released" and .status.phase != "Failed")
          ) | .metadata.name')"; then
        echo "  WARNING: could not list PersistentVolumes — skipping sweep to avoid removing live data"
        _LIVE_PVS_OK=false
    else
        _LIVE_PVS_OK=true
    fi

    # Also tear the pods down if clean.sh is interrupted between creating them
    # and the cleanup below; activeDeadlineSeconds is the backstop if the
    # interruption takes this shell with it.
    trap 'kubectl delete pod -n "${_SWEEP_NS}" -l "nico-lpp-run=${_SWEEP_TS}" \
        --ignore-not-found --wait=false >/dev/null 2>&1 || true' EXIT

    _SWEEP_PODS=()
    _sweep_idx=0
    _SWEEP_NODES=""
    if [[ "${_LIVE_PVS_OK}" == "true" ]]; then
        _SWEEP_NODES="$(kubectl get nodes \
            -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)"
        [[ -n "${_SWEEP_NODES}" ]] || \
            echo "  WARNING: could not list nodes — sweep did not run; clean ${LOCAL_PATH_DIR} manually"
    fi

    for _node in ${_SWEEP_NODES}; do
        # Lowercase via tr for portability (bash 3.2 on macOS lacks ${var,,}).
        # Truncate to keep the whole name inside the 63-char DNS-1123 limit, and
        # strip any trailing "-" left by truncation (not a legal name ending).
        _safe="$(printf '%s' "${_node}" | tr '[:upper:]' '[:lower:]')"
        _safe="${_safe//[^a-z0-9-]/-}"
        _safe="${_safe:0:32}"
        while [[ "${_safe}" == *- ]]; do _safe="${_safe%?}"; done
        # A name that is all dashes leaves nothing behind, which would put a
        # trailing "-" on the pod name and fail DNS-1123.
        [[ -z "${_safe}" ]] && _safe="node"
        # Node index disambiguates nodes whose names collide after truncation.
        _sweep_idx=$(( _sweep_idx + 1 ))
        _pod="nico-lpp-${_SWEEP_TS}-${_sweep_idx}-${_safe}"

        if kubectl apply -f - >/dev/null 2>&1 <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: ${_pod}
  namespace: ${_SWEEP_NS}
  labels:
    nico-lpp-sweep: "true"
    nico-lpp-run: "${_SWEEP_TS}"
spec:
  nodeName: "${_node}"
  restartPolicy: Never
  automountServiceAccountToken: false
  # The pod carries a snapshot of which PVs were live when it was created. If it
  # sat Pending (image pull failure) past a later setup.sh, that snapshot would
  # be stale and it would delete freshly provisioned data. The cluster enforces
  # this deadline even if clean.sh is interrupted or its host goes away.
  activeDeadlineSeconds: 300
  tolerations:
  - operator: Exists
  volumes:
  - name: host
    hostPath:
      path: ${LOCAL_PATH_DIR}
      type: DirectoryOrCreate
  containers:
  - name: sweep
    image: ${_SWEEP_IMAGE}
    # Least privilege for an rm on a hostPath: uid 0 plus only the two DAC
    # capabilities needed to traverse workload-owned data (postgres pgdata is
    # mode 0700 under a non-root uid — root alone cannot descend into it).
    securityContext:
      runAsUser: 0
      allowPrivilegeEscalation: false
      capabilities:
        drop: ["ALL"]
        add: ["DAC_OVERRIDE", "DAC_READ_SEARCH"]
    volumeMounts:
    - name: host
      mountPath: /host
    env:
    - name: LIVE_PVS
      value: |
${_LIVE_PVS:+$(printf '%s\n' "${_LIVE_PVS}" | sed 's/^/        /')}
    - name: STACK_NS
      value: "${_STACK_NS}"
    # When LIVE_PVS was taken. Anything on disk newer than this was created
    # after the snapshot, so the snapshot says nothing about whether it is live.
    - name: SNAPSHOT_TS
      value: "${_SWEEP_TS}"
    command:
    - sh
    - -c
    - |
      printf "NODE=${_node}\n"
      for d in /host/pvc-*; do
        [ -d "\$d" ] || continue
        b="\$(basename "\$d")"
        # Directory name is "<pv-name>_<namespace>_<pvc-name>". A directory
        # without that suffix (older provisioner layout) yields no namespace
        # match and is left alone.
        rest="\${b#*_}"
        ns="\${rest%%_*}"
        # Directories newer than the snapshot cannot be judged by it. This pod
        # may have sat Pending while a setup.sh provisioned fresh volumes here,
        # and deleting one of those would destroy live data. Ages only make this
        # safer, so no cross-script locking is needed to rely on it. The
        # snapshot second itself counts as newer — a directory created within it
        # is equally unaccounted for. Assumes node clocks are roughly in step,
        # which a cluster requires anyway.
        mt="\$(stat -c %Y "\$d" 2>/dev/null || echo 0)"
        if ! printf '%s\n' \$STACK_NS | grep -qxF "\$ns"; then
          printf "skipped=%s\n" "\$b"
        elif [ "\$mt" -ge "\$SNAPSHOT_TS" ]; then
          printf "newer=%s\n" "\$b"
        elif printf '%s\n' "\$LIVE_PVS" | grep -qxF "\${b%%_*}"; then
          printf "kept=%s\n" "\$b"
        elif rm -rf "\$d" 2>/dev/null; then
          printf "removed=%s\n" "\$b"
        else
          printf "failed=%s\n" "\$b"
        fi
      done
      printf "sweep=done\n"
    resources:
      requests:
        cpu: 10m
        memory: 16Mi
EOF
        then
            _SWEEP_PODS+=("${_pod}")
        else
            echo "  WARNING: could not create sweep pod on node ${_node} — clean ${LOCAL_PATH_DIR} on it manually"
        fi
    done

    if [[ -n "${_SWEEP_PODS[*]:-}" ]]; then
        # One list call per tick rather than one per pod — on a large cluster
        # the per-pod form is hundreds of API calls over the full timeout.
        _deadline=$(( $(date +%s) + 120 ))
        while [[ $(date +%s) -lt "${_deadline}" ]]; do
            _phases="$(kubectl get pods -n "${_SWEEP_NS}" \
                -l "nico-lpp-run=${_SWEEP_TS}" \
                -o jsonpath='{range .items[*]}{.status.phase}{"\n"}{end}' \
                2>/dev/null || true)"
            _settled="$(printf '%s\n' "${_phases}" \
                | grep -cE '^(Succeeded|Failed)$' || true)"
            [[ "${_settled}" -ge "${#_SWEEP_PODS[@]}" ]] && break
            sleep 5
        done

        for _pod in "${_SWEEP_PODS[@]}"; do
            _logs="$(kubectl logs "${_pod}" -n "${_SWEEP_NS}" 2>/dev/null || true)"
            _node_label="$(printf '%s' "${_logs}" | grep '^NODE=' | cut -d= -f2- || true)"
            _removed="$(printf '%s' "${_logs}" | grep -c '^removed=' || true)"
            _failed="$(printf '%s' "${_logs}" | grep -c '^failed=' || true)"
            if [[ -z "${_logs}" ]] || ! printf '%s' "${_logs}" | grep -q '^sweep=done$'; then
                echo "  WARNING: sweep on ${_node_label:-${_pod}} did not complete — set CLEAN_SWEEP_IMAGE to a pre-pulled image, or clean ${LOCAL_PATH_DIR} manually"
            else
                echo "  ${_node_label:-${_pod}}: removed ${_removed} orphaned director$([[ "${_removed}" == "1" ]] && echo y || echo ies)"
                _newer="$(printf '%s' "${_logs}" | grep -c '^newer=' || true)"
                if [[ "${_newer}" != "0" ]]; then
                    echo "  NOTE: ${_newer} director$([[ "${_newer}" == "1" ]] && echo y || echo ies) on ${_node_label:-${_pod}} changed after the sweep started and were left alone"
                fi
                if [[ "${_failed}" != "0" ]]; then
                    echo "  WARNING: ${_failed} director$([[ "${_failed}" == "1" ]] && echo y || echo ies) on ${_node_label:-${_pod}} could not be removed — clean them under ${LOCAL_PATH_DIR} manually:"
                    printf '%s' "${_logs}" | sed -n 's/^failed=/    /p'
                fi
            fi
        done

        # This run's pods; anything stranded by an earlier one was cleared at the
        # top of this step.
        kubectl delete pod -n "${_SWEEP_NS}" -l "nico-lpp-run=${_SWEEP_TS}" \
            --ignore-not-found --wait=false >/dev/null 2>&1 || true
    fi
    trap - EXIT
fi

echo ""
echo "=== Clean complete ==="
