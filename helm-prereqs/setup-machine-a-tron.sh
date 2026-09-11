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
# setup-machine-a-tron.sh — deploy machine-a-tron end to end on a NICo site
#
# machine-a-tron is a bare-metal simulator: it hosts mock DPUs and servers via a
# Redfish BMC mock so NICo can run full ingestion flows without real hardware.
# This script sets up EVERYTHING needed for a running NICo Core site to discover
# and create the simulated machines, in setup.sh style (phased, idempotent).
#
# It assumes NICo Core (nico-api, postgres/nico-pg-cluster, Vault, ESO,
# cert-manager) is already deployed on the target cluster (i.e. setup.sh has run
# and the site is bootstrapped). It does NOT deploy NICo Core.
#
# ---------------------------------------------------------------------------
# WHY EACH STEP EXISTS — the non-obvious failure modes this script prevents
# (learned the hard way; do not remove without understanding them):
#
#  * CA refresh (Phase 3): after a site reprovision the nico-system CA and
#    nico-api are recreated. A machine-a-tron left over from before still trusts
#    the OLD CA (stale nico-roots) and presents a client cert signed by the OLD
#    CA, so mTLS to nico-api fails with "client error (Connect)" on every call.
#    We always re-copy nico-roots from nico-system and (Phase 8) delete the old
#    cert secret so cert-manager reissues from the CURRENT CA.
#
#  * BMC site-root credential (Phase 5): site-explorer's check_preconditions
#    requires the Vault credential machines/bmc/site/root. It is NOT in the
#    default nico-prereqs kvSeeds, so without it site-explorer aborts every run
#    with MissingCredentials and never explores anything.
#
#  * bmc_proxy field name (Phase 6): the site_explorer config field is
#    `bmc_proxy = "host:port"` (a single string). `override_target_host` is NOT
#    a real field (older docs were wrong); override_target_ip/port are
#    DEPRECATED. Setting bmc_proxy at launch also makes allow_changing_bmc_proxy
#    default true, which is what lets machine-a-tron's configureBmcProxyHost
#    runtime call succeed instead of being PermissionDenied.
#
#  * expected_machines (chart value registerExpectedMachines: true): without a
#    matching expected_machines row (by BMC MAC), MachineCreator refuses to
#    create the managed host. machine-a-tron auto-registers them when the value
#    is true — the script asserts it.
#
#  * DHCP pool sizing (Phase 7): machine-a-tron needs one OOB IP per BMC —
#    hostCount + hostCount*dpuPerHostCount. Overflowing the OOB pool yields
#    "No IP addresses left in prefix ..." and machines never register.
#
#  * DPF prerequisites + simulation (Phase 4b), when [dpf] enabled = true in
#    the nico-core config (site config overrides global). Two parts:
#    (a) INGESTION PREREQUISITES applied whether or not the simulator is
#        deployed: the nico-api DPF-namespace Role (nico-api-dpf) and
#        CARBIDE_API_ALLOW_INSECURE_DISCOVERY=true on nico-api. Post-#3561
#        cores resolve DiscoverMachine callers by source IP, which every
#        simulated machine shares — without the flag, discovery-gated machine
#        CREATION stalls short of the target (a --skip-dpf-sim run once wedged
#        at 2838/3000) and agent self-discovery fails.
#    (b) SIMULATOR DEPLOYMENT (dev/k8s/dpf-sim-controller: namespace, DPF CRDs,
#        RBAC, Deployment) — this is the part --skip-dpf-sim skips. Without a
#        simulator (and no real operator) hosts finish ingestion but park in
#        dpuinit. GUARDRAIL: hard-fails if the real DPF operator is deployed,
#        since both would drive DPU.status.phase.
#
#  * SVI IPs on the simulated prefixes (Phase 5, scale mode): the host
#    network-config builder under FNN requires network_prefixes.svi_ip on L2
#    segments; without it get_managed_host_network_config fails with
#    "SVI IP is not allocated" and hosts never leave dpuinit. The segment
#    seeding sets svi_ip = gateway, and backfills rows created before this
#    script did so.
#
#  * machine_interfaces_deletion singleton (Phase 10 check): the
#    machine_dhcp_records VIEW inner-joins the singleton row id=1. If it is ever
#    deleted (e.g. by manual lease cleanup) the view returns zero rows and
#    DiscoverDhcp fails with "no rows ... expected to return at least one row".
#    The script restores the row if missing. NEVER delete it. To free stale
#    leases, force-delete machine records via the admin CLI or reprovision —
#    do NOT hand-delete interface/dhcp rows.
#
# ---------------------------------------------------------------------------
# Tool requirements: kubectl, helm, jq
#
# Required environment:
#   KUBECONFIG             Path to the target cluster kubeconfig (or current
#                          kubectl context already points at it).
#
# Optional environment:
#   NICO_IMAGE_REGISTRY    REQUIRED unless image.repository is set in the
#                          values file. Registry/repository prefix, without
#                          http(s):// (same convention as setup.sh). The
#                          machine-a-tron image is pulled from
#                          ${NICO_IMAGE_REGISTRY}/machine-a-tron.
#   MAT_IMAGE_TAG          REQUIRED unless image.tag is set in the values
#                          file. machine-a-tron image tag.
#   REGISTRY_PULL_SECRET   Registry password/API key. Only needed if the
#                          pull secret does not already exist in the
#                          machine-a-tron namespace.
#   REGISTRY_PULL_USERNAME Username for the pull secret. Default: $oauthtoken
#   MAT_NAMESPACE          Deployment namespace. Default: nico-mat
#   NICO_SYSTEM_NS         NICo Core namespace. Default: nico-system
#   POSTGRES_NS            Postgres namespace. Default: postgres
#   VAULT_NS               Vault namespace. Default: vault
#   BMC_USERNAME           Site BMC root username. Default: root
#   BMC_PASSWORD           Site BMC root password (rotation target). MUST
#                          differ from the mock factory defaults.
#                          Default: NicoSiteRoot1
#   OOB_DHCP_RELAY         OOB/underlay gateway (BMC DHCP relay). Auto-detected
#                          from nico-core site config if unset.
#   ADMIN_DHCP_RELAY       Admin network gateway. Auto-detected if unset.
#   HOST_COUNT             Override machines.dell-hosts.hostCount.
#   DPU_PER_HOST           Override machines.dell-hosts.dpuPerHostCount.
#   CHART_DIR              Path to the nico-machine-a-tron chart.
#                          Default: <repo>/helm/charts/nico-machine-a-tron
#   VALUES_FILE            Base values template.
#                          Default: <this dir>/values/machine-a-tron.yaml
#   SCALE_RUN_INTERVAL     Tuning override (#3738): [site_explorer] run_interval
#                          (e.g. "30s"). Unset = leave the site's value.
#   SCALE_FW_CONCURRENCY   Tuning override: [firmware_global] concurrency_limit.
#   SCALE_FW_RUN_INTERVAL  Tuning override: [firmware_global] run_interval.
#   SCALE_STATE_MAX_CONCURRENCY
#                          Tuning override: [machine_state_controller.controller]
#                          max_concurrency (parallel state-machine tasks).
#   INGEST_RATE_CSV        Where the Phase 10 loop writes its per-sample
#                          ingestion counters (CSV). Default: a file under /tmp.
#   DPF_SIM_IMAGE          dpf-sim-controller image ref for Phase 4b. Default:
#                          ${NICO_IMAGE_REGISTRY}/dpf-sim-controller:${DPF_SIM_IMAGE_TAG}
#   DPF_SIM_IMAGE_TAG      Tag for the derived default above. Default: latest
#   DPF_NAMESPACE          DPF namespace (must match the site config).
#                          Default: dpf-operator-system
#
# Usage:
#   export KUBECONFIG=/path/to/kubeconfig
#   ./setup-machine-a-tron.sh            # prompt before deploy
#   ./setup-machine-a-tron.sh -y         # non-interactive
#   ./setup-machine-a-tron.sh --skip-nico-core-config   # don't touch nico-core
#   ./setup-machine-a-tron.sh --skip-dpf-sim  # no DPF simulator / RBAC / flag
# =============================================================================

set -euo pipefail

# --- config / defaults -------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

MAT_NAMESPACE="${MAT_NAMESPACE:-nico-mat}"
NICO_SYSTEM_NS="${NICO_SYSTEM_NS:-nico-system}"
POSTGRES_NS="${POSTGRES_NS:-postgres}"
VAULT_NS="${VAULT_NS:-vault}"
BMC_USERNAME="${BMC_USERNAME:-root}"
# Site-wide BMC root password — the ROTATION TARGET. site-explorer logs into
# each mock BMC with its factory-default password, then rotates it to this
# value. It MUST DIFFER from both factory defaults below, or the rotation is a
# no-op and the mock keeps rejecting with "Factory-default password must be
# changed" (403) forever.
BMC_PASSWORD="${BMC_PASSWORD:-NicoSiteRoot1}"
# Factory defaults used by this simulation:
#   host BMCs:  `bmc_mock::DUMMY_FACTORY_PASSWORD` = "factory_password"
#   DPU BMCs:   `bmc_vendor::DpuModel::default_factory_credentials()` is shared
#               by `bmc-mock` and site-explorer
# site-explorer checks a model-specific entry, the catch-all seeded below, then
# its per-model code fallback. This catch-all matches BF2/BF3 and unrecognized
# models (`root`/`0penBmc`). BF4 needs a model-specific `bf4` entry for its
# `admin` username.
# Do not change unless those factory defaults change.
FACTORY_HOST_BMC_PASSWORD="factory_password"
FACTORY_DPU_BMC_PASSWORD="0penBmc"
# Vendor path segment for the host factory cred. LOWERCASE is required: the
# credential path is built with format!("{vendor}") and BMCVendor's Display
# impl lowercases the variant name ("Dell Inc." → BMCVendor::Dell → "dell",
# crates/bmc-vendor/src/lib.rs impl Display). to_pascalcase() exists but is
# NOT used for Vault paths.
HOST_BMC_VENDOR="${HOST_BMC_VENDOR:-dell}"
# site-default UEFI passwords — check_preconditions requires them NON-EMPTY.
# The mock BMC does not validate them, so any non-empty value works.
UEFI_DPU_PASSWORD="${UEFI_DPU_PASSWORD:-bluefield}"
UEFI_HOST_PASSWORD="${UEFI_HOST_PASSWORD:-bluefield}"
REGISTRY_PULL_USERNAME="${REGISTRY_PULL_USERNAME:-\$oauthtoken}"
PULL_SECRET_NAME="${PULL_SECRET_NAME:-machine-a-tron-pull}"
RELEASE="nico-machine-a-tron"
# The chart names the Service <release>-<podKey>-bmc-mock; the single-pod proxy
# config keys its pod "default". The old value omitted the pod key and resolved
# to nothing -- exploration failed with ConnectionRefused on every endpoint
# resolving to nothing, so every endpoint fails with ConnectionRefused.
BMC_MOCK_SVC="${BMC_MOCK_SVC:-nico-machine-a-tron-default-bmc-mock}"
BMC_MOCK_PORT="1266"
# site-explorer runs in nico-system, so it CANNOT resolve the bare service name
# (which resolves against its own namespace). bmc_proxy MUST use the
# cross-namespace FQDN of the bmc-mock service in the machine-a-tron namespace.
BMC_MOCK_FQDN="${BMC_MOCK_SVC}.${MAT_NAMESPACE}.svc.cluster.local"
NICO_DB="nico_system_nico"

# --- deployment mode ---------------------------------------------------------
# override (default): all Redfish through site_explorer.bmc_proxy → one mock.
# scale: Controller Mode — mat-k8s-controller creates one ClusterIP Service per
#   BMC with ClusterIP = BMC IP. Uses values/machine-a-tron-scale.yaml plus a
#   NICo network covering the BMC IP range. See the chart README "Controller Mode".
MAT_MODE="${MAT_MODE:-override}"
# Networks for scale mode. The OOB gateway must match the scale values file
# (oobDhcpRelayAddress). Both are sized from MEASURED demand, not from the host
# count: since the mock BMCs became DHCP clients, a 4,500-host fleet needs far
# more addresses than the obvious "one per host" arithmetic suggests.
#
# Sized to real demand, measured on a correct fleet: 3 OOB addresses per host
# (one per simulated BMC) and 1 admin address per machine, so 4,500 hosts need
# 13,500 of each and a /18's 16,382 fits.
#
# These were briefly widened to /16 after runs began exhausting the pools. That
# was treating a symptom: the fleet was rendering at DOUBLE size (see the render
# gate below), so demand was 27,000 rather than 13,500. Widening made things
# worse -- the allocator materialises the whole host space per request, measured
# 9.8ms at /18 against 28.6ms at /16, and it does that holding the fleet-wide
# admin-segment lock. Sizing these generously is NOT free.
#
# Controller Mode has the opposite constraint: its BMC addresses are ClusterIPs
# and must come from the Kubernetes ServiceCIDR, so it must override these.
SCALE_OOB_PREFIX="${SCALE_OOB_PREFIX:-10.96.64.0/18}";  SCALE_OOB_GW="${SCALE_OOB_GW:-10.96.64.1}"
SCALE_ADMIN_PREFIX="${SCALE_ADMIN_PREFIX:-10.102.0.0/18}"; SCALE_ADMIN_GW="${SCALE_ADMIN_GW:-10.102.0.1}"
SCALE_RESERVE=1
# These four are operator-overridable and are written straight into the site
# config and the DB, so validate them before anything consumes them: an
# invalid prefix would insert a network segment and only fail on the separate
# prefix insert, leaving a half-built segment that later runs skip as
# "already present".
_valid_cidr_gw() {   # $1=cidr $2=gateway $3=label
    python3 - "$1" "$2" "$3" <<'PYCHK'
import ipaddress, sys
cidr, gw, label = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    net = ipaddress.ip_network(cidr, strict=True)
except ValueError as e:
    sys.exit(f"{label}: invalid prefix {cidr!r}: {e}")
try:
    addr = ipaddress.ip_address(gw)
except ValueError as e:
    sys.exit(f"{label}: invalid gateway {gw!r}: {e}")
if addr not in net:
    sys.exit(f"{label}: gateway {gw} is outside prefix {cidr}")
PYCHK
}
_valid_cidr_gw "$SCALE_OOB_PREFIX"   "$SCALE_OOB_GW"   "SCALE_OOB"   || die "$(_valid_cidr_gw "$SCALE_OOB_PREFIX" "$SCALE_OOB_GW" "SCALE_OOB" 2>&1)"
_valid_cidr_gw "$SCALE_ADMIN_PREFIX" "$SCALE_ADMIN_GW" "SCALE_ADMIN" || die "$(_valid_cidr_gw "$SCALE_ADMIN_PREFIX" "$SCALE_ADMIN_GW" "SCALE_ADMIN" 2>&1)"
# site_explorer throughput knobs applied in scale mode (defaults 30/90/4 make
# 4500-host ingestion take ~9h; these bring it to ~1-2h).
# Defaults are the values measured best at BOTH scales:
# 1,000 hosts -> 186.8 machines/min (vs 138.5 under the old conc=200 tuning;
# the old conc=400 "overshoot" to 68/min was a lock-contention artifact, fixed);
# 4,500 hosts -> 80-100 machines/min, all 13,500 machines ready in ~3-3.5h.
# One configuration serves both scales; override only with measurements.
SCALE_CONCURRENT_EXPLORATIONS="${SCALE_CONCURRENT_EXPLORATIONS:-400}"
# NB: keep explorations_per_run MODERATE. Identification and machine creation
# only run at the END of a completed explore_site cycle — a huge per-run value
# makes every cycle deep-scan hundreds of endpoints (dozens of Redfish calls
# each) and cycles stop completing, so machines are never created. 360 is the
# measured sweet spot of that trade-off;
# raise it further only with cycle-completion measurements in hand.
SCALE_EXPLORATIONS_PER_RUN="${SCALE_EXPLORATIONS_PER_RUN:-360}"
SCALE_MACHINES_CREATED_PER_RUN="${SCALE_MACHINES_CREATED_PER_RUN:-100}"

CHART_DIR="${CHART_DIR:-${REPO_ROOT}/helm/charts/nico-machine-a-tron}"

# --- DPF simulation (Phase 4b) ------------------------------------------------
DPF_NAMESPACE="${DPF_NAMESPACE:-dpf-operator-system}"
DPF_SIM_DIR="${DPF_SIM_DIR:-${REPO_ROOT}/dev/k8s/dpf-sim-controller}"
DPF_SIM_IMAGE="${DPF_SIM_IMAGE:-}"
DPF_SIM_IMAGE_TAG="${DPF_SIM_IMAGE_TAG:-latest}"

ASSUME_YES=false
SKIP_NICO_CORE_CONFIG=false
SKIP_DPF_SIM=false
CM_JSON=""
MERGED_VALUES=""
cleanup() { rm -f "$CM_JSON" "$MERGED_VALUES" 2>/dev/null || true; }
trap cleanup EXIT

for arg in "$@"; do
    case "$arg" in
        -y|--yes) ASSUME_YES=true ;;
        --scale) MAT_MODE="scale" ;;
        --skip-nico-core-config) SKIP_NICO_CORE_CONFIG=true ;;
        --skip-dpf-sim) SKIP_DPF_SIM=true ;;
        -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -130; exit 0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if [[ "$MAT_MODE" == "scale" ]]; then
    VALUES_FILE="${VALUES_FILE:-${SCRIPT_DIR}/values/machine-a-tron-scale.yaml}"
else
    VALUES_FILE="${VALUES_FILE:-${SCRIPT_DIR}/values/machine-a-tron.yaml}"
fi

# --- helpers -----------------------------------------------------------------
_c() { printf '\033[%sm' "$1"; }
BOLD="$(_c 1)"; RED="$(_c 31)"; GREEN="$(_c 32)"; YEL="$(_c 33)"; BLU="$(_c 34)"; NC="$(_c 0)"
phase() { echo; echo "${BOLD}${BLU}== $* ==${NC}"; }
info()  { echo "  $*"; }
ok()    { echo "  ${GREEN}✓${NC} $*"; }
warn()  { echo "  ${YEL}!${NC} $*" >&2; }
die()   { echo "${RED}ERROR:${NC} $*" >&2; exit 1; }
confirm() {
    $ASSUME_YES && return 0
    read -r -p "  $* [y/N] " ans
    [[ "$ans" == "y" || "$ans" == "Y" ]]
}

# psql against the consolidated NICo DB on the Patroni primary
PG_PRIMARY=""
_pg_primary() {
    [[ -n "$PG_PRIMARY" ]] && { echo "$PG_PRIMARY"; return; }
    PG_PRIMARY="$(kubectl get pods -n "$POSTGRES_NS" -l application=spilo \
        -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.labels.spilo-role}{"\n"}{end}' 2>/dev/null \
        | awk '$2=="master"{print $1}' | head -1)"
    echo "$PG_PRIMARY"
}
psql_q() {
    local pg; pg="$(_pg_primary)"
    [[ -n "$pg" ]] || die "no Patroni primary found in namespace $POSTGRES_NS"
    kubectl exec -n "$POSTGRES_NS" "$pg" -- su postgres -c "psql -d $NICO_DB -v ON_ERROR_STOP=1 -tAc \"$1\"" 2>/dev/null
}
# count query that always yields a number — a transient kubectl/psql failure
# returns "0" instead of an empty string that would blow up (( )) arithmetic.
psql_count() { local r; r="$(psql_q "$1" || true)"; echo "${r:-0}"; }
# vault CLI on vault-0 using the root token stored in nico-system/nico-vault-token.
# Token is cached after the first read (it does not change within a run).
# env vars are exported (not inline-prefixed) so they apply across pipes, e.g.
# `echo ... | vault kv put ... -` — an inline prefix would bind them to echo only.
_VAULT_TOKEN=""
vault_cmd() {
    if [[ -z "$_VAULT_TOKEN" ]]; then
        _VAULT_TOKEN="$(kubectl get secret nico-vault-token -n "$NICO_SYSTEM_NS" -o jsonpath='{.data.token}' | base64 -d)"
        [[ -n "$_VAULT_TOKEN" ]] || die "could not read nico-vault-token from $NICO_SYSTEM_NS"
    fi
    kubectl exec -n "$VAULT_NS" vault-0 -c vault -- sh -c \
        "export VAULT_TOKEN='$_VAULT_TOKEN' VAULT_ADDR=https://127.0.0.1:8200 VAULT_SKIP_VERIFY=true; $1" 2>/dev/null
}
# copy a secret from nico-system into the machine-a-tron namespace (strip metadata)
copy_secret() {
    local name="$1"
    kubectl get secret "$name" -n "$NICO_SYSTEM_NS" -o json 2>/dev/null \
        | jq 'del(.metadata.namespace,.metadata.resourceVersion,.metadata.uid,.metadata.creationTimestamp,.metadata.ownerReferences,.metadata.annotations,.metadata.managedFields)' \
        | kubectl apply -n "$MAT_NAMESPACE" -f - >/dev/null
}

# =============================================================================
# Phase 0 — preflight
# =============================================================================
phase "Phase 0 — preflight"
for t in kubectl helm jq; do command -v "$t" >/dev/null || die "$t not found in PATH"; done
kubectl version -o json >/dev/null 2>&1 || kubectl cluster-info >/dev/null 2>&1 || die "cannot reach the cluster (check KUBECONFIG)"
ok "tools present, cluster reachable"
[[ -d "$CHART_DIR" ]] || die "chart dir not found: $CHART_DIR"
[[ -f "$VALUES_FILE" ]] || die "values file not found: $VALUES_FILE"
kubectl get deploy nico-api -n "$NICO_SYSTEM_NS" >/dev/null 2>&1 || die "nico-api not found in $NICO_SYSTEM_NS — deploy NICo Core (setup.sh) first"
[[ -n "$(_pg_primary)" ]] || die "no Postgres primary in $POSTGRES_NS"
kubectl get pod vault-0 -n "$VAULT_NS" >/dev/null 2>&1 || die "vault-0 not found in $VAULT_NS"
ok "NICo Core present: nico-api, postgres primary $(_pg_primary), vault-0"

# portable extraction (macOS BSD sed/grep lack \s): [[:space:]] + awk on quotes
MAT_IMAGE_TAG="${MAT_IMAGE_TAG:-$(grep -E '^[[:space:]]*tag:' "$VALUES_FILE" | head -1 | awk -F'"' '{print $2}')}"
MAT_IMAGE_REPO="${MAT_IMAGE_REPO:-$(grep -E '^[[:space:]]*repository:' "$VALUES_FILE" | head -1 | awk -F'"' '{print $2}')}"
# Registry-agnostic (mirrors setup.sh): the image location comes from the
# environment, never from committed defaults.
if [[ -z "$MAT_IMAGE_REPO" ]]; then
    [[ -n "${NICO_IMAGE_REGISTRY:-}" ]] || die "NICO_IMAGE_REGISTRY is unset and the values file has no image.repository (see setup.sh conventions)"
    MAT_IMAGE_REPO="${NICO_IMAGE_REGISTRY}/machine-a-tron"
fi
HOST_COUNT="${HOST_COUNT:-$(grep -E '^[[:space:]]*hostCount:' "$VALUES_FILE" | head -1 | grep -oE '[0-9]+')}"
DPU_PER_HOST="${DPU_PER_HOST:-$(grep -E '^[[:space:]]*dpuPerHostCount:' "$VALUES_FILE" | head -1 | grep -oE '[0-9]+')}"
[[ -n "$MAT_IMAGE_TAG" ]] || die "MAT_IMAGE_TAG is unset and the values file has no image.tag"
[[ "$HOST_COUNT" =~ ^[0-9]+$ && "$DPU_PER_HOST" =~ ^[0-9]+$ ]] \
    || die "could not determine hostCount/dpuPerHostCount from $VALUES_FILE (set HOST_COUNT / DPU_PER_HOST)"
# Passwords are inlined into sh -c JSON heredocs on the vault pod; quotes,
# backslashes, or whitespace would break quoting or corrupt the JSON silently.
for _pw in "$BMC_PASSWORD" "$UEFI_DPU_PASSWORD" "$UEFI_HOST_PASSWORD"; do
    case "$_pw" in
        *[\'\"\\\ ]*) die "passwords must not contain quotes, backslashes, or spaces (BMC_PASSWORD / UEFI_*_PASSWORD)" ;;
    esac
done
info "image: ${MAT_IMAGE_REPO}:${MAT_IMAGE_TAG}   hosts: ${HOST_COUNT}   dpus/host: ${DPU_PER_HOST}"

# GOTCHA: Postgres out-of-memory does not degrade gracefully. The kernel kills a
# backend, the postmaster crash-recovers, and every connection is dropped with
# in-flight transactions rolled back. Downstream that presents as stalled machine
# readiness, controller panics and deadlocks, so the real cause is easy to miss.
# A 4,500-host run against the old 4Gi default logged 531 OOM kills and never
# completed; the same run at 24Gi peaked at ~8.7GiB with zero kills. Warn here so
# an undersized database is visible at minute zero rather than hour three.
_pg_mem="$(kubectl get postgresql -n "${POSTGRES_NS:-postgres}" nico-pg-cluster \
    -o jsonpath='{.spec.resources.limits.memory}' 2>/dev/null || true)"
if [[ -n "$_pg_mem" ]]; then
    _pg_gib="${_pg_mem%Gi}"
    if [[ "$_pg_mem" == *Mi ]]; then _pg_gib=$(( ${_pg_mem%Mi} / 1024 )); fi
    if [[ "$_pg_gib" =~ ^[0-9]+$ ]]; then
        # Same rough scale as the guidance in helm-prereqs/values.yaml.
        _pg_want=4
        (( HOST_COUNT > 500 ))   && _pg_want=8
        (( HOST_COUNT > 1500 ))  && _pg_want=16
        (( HOST_COUNT > 5000 ))  && _pg_want=32
        if (( _pg_gib < _pg_want )); then
            warn "postgres memory limit is ${_pg_mem} but ${HOST_COUNT} hosts wants >= ${_pg_want}Gi"
            warn "  undersized postgres OOM-kills under load and breaks ingestion in ways that look unrelated"
            warn "  raise postgresql.resources.limits.memory in helm-prereqs/values.yaml and reinstall"
            warn "  (a resize applied before teardown is reverted: teardown deletes the postgres namespace)"
            # Don't let the warning scroll past: make the operator choose, like
            # the fit-sizing and deploy gates below. -y keeps automation moving.
            confirm "Continue anyway with ${_pg_mem} for ${HOST_COUNT} hosts?" \
                || die "aborted on postgres sizing — resize postgresql.resources.limits.memory and rerun"
        else
            ok "postgres memory ${_pg_mem} is adequate for ${HOST_COUNT} hosts (>= ${_pg_want}Gi)"
        fi
    fi
fi

# =============================================================================
# Phase 1 — namespace
# =============================================================================
phase "Phase 1 — namespace ${MAT_NAMESPACE}"
kubectl create namespace "$MAT_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
# label so ESO / nico-roots sync treats it as a managed namespace
kubectl label namespace "$MAT_NAMESPACE" nico.nvidia.com/managed=true --overwrite >/dev/null
ok "namespace ready"

# =============================================================================
# Phase 2 — image pull secret
# =============================================================================
phase "Phase 2 — image pull secret"
if kubectl get secret "$PULL_SECRET_NAME" -n "$MAT_NAMESPACE" >/dev/null 2>&1; then
    ok "pull secret ${PULL_SECRET_NAME} already exists"
elif [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
    kubectl create secret docker-registry "$PULL_SECRET_NAME" -n "$MAT_NAMESPACE" \
        --docker-server="${MAT_IMAGE_REPO%%/*}" --docker-username="$REGISTRY_PULL_USERNAME" \
        --docker-password="$REGISTRY_PULL_SECRET" \
        --dry-run=client -o yaml | kubectl apply -f - >/dev/null
    ok "pull secret ${PULL_SECRET_NAME} created"
else
    warn "pull secret ${PULL_SECRET_NAME} missing and REGISTRY_PULL_SECRET unset"
    warn "  → set REGISTRY_PULL_SECRET, or ensure the image is already cached on nodes"
fi

# =============================================================================
# Phase 3 — refresh CA + Vault secrets from nico-system  (GOTCHA: stale CA)
# =============================================================================
phase "Phase 3 — refresh nico-roots CA + Vault secrets"
copy_secret nico-roots
ok "nico-roots synced from ${NICO_SYSTEM_NS} (current CA)"

# GOTCHA: nico-roots is not the whole story. machine-a-tron's own serving/client
# certs are issued by cert-manager into per-deployment secrets, and those are NOT
# recreated by a reinstall: teardown leaves the namespace secrets behind, and
# cert-manager will not reissue a certificate that has not expired. So after a
# site reprovision rotates the nico-system CA, MAT keeps validating against the
# previous CA and every call to nico-api fails with
# `invalid peer certificate: BadSignature`. The pods then fail their TCP
# readiness probe forever — visible as a slow restart climb with zero machine
# interfaces ever registering, which reads like a scale problem but is not one.
# Drop any MAT cert secret whose CA no longer matches nico-system's, and let
# cert-manager reissue from the current one.
_current_ca=$(kubectl get secret nico-roots -n "$NICO_SYSTEM_NS" \
                -o jsonpath='{.data.ca\.crt}' 2>/dev/null | base64 -d 2>/dev/null \
              | openssl x509 -noout -fingerprint -sha256 2>/dev/null | cut -d= -f2)
if [ -n "$_current_ca" ]; then
    for _s in $(kubectl get secret -n "$MAT_NAMESPACE" \
                  -l controller.cert-manager.io/fao=true -o name 2>/dev/null); do
        _s_ca=$(kubectl get "$_s" -n "$MAT_NAMESPACE" -o jsonpath='{.data.ca\.crt}' 2>/dev/null \
                | base64 -d 2>/dev/null \
                | openssl x509 -noout -fingerprint -sha256 2>/dev/null | cut -d= -f2)
        if [ -n "$_s_ca" ] && [ "$_s_ca" != "$_current_ca" ]; then
            kubectl delete "$_s" -n "$MAT_NAMESPACE" >/dev/null 2>&1 \
              && warn "${_s#secret/}: CA did not match nico-system; deleted for reissue"
        fi
    done
    ok "MAT cert secrets checked against the current nico-system CA"
else
    warn "could not read the nico-system CA fingerprint; skipping MAT cert CA check"
fi
for s in nico-vault-approle-tokens nico-vault-token; do
    if kubectl get secret "$s" -n "$NICO_SYSTEM_NS" >/dev/null 2>&1; then
        copy_secret "$s"; ok "$s synced"
    else
        warn "$s not present in ${NICO_SYSTEM_NS}; skipping (may produce log noise only)"
    fi
done

# =============================================================================
# Phase 4 — seed site BMC root credential  (GOTCHA: not in default kvSeeds)
# =============================================================================
phase "Phase 4 — seed Vault BMC + UEFI credentials"
_kv_put() {   # $1=path  $2=username  $3=password
    vault_cmd "echo '{\"UsernamePassword\":{\"username\":\"$2\",\"password\":\"$3\"}}' | vault kv put secrets/$1 -" >/dev/null \
        || die "failed to write $1 to Vault"
}
_kv_password() {   # $1=path → prints current password (empty if absent)
    # `|| true` is load-bearing: when the path is absent, vault kv get fails and
    # under `set -e` a failing $(...) assignment would kill the whole script.
    vault_cmd "vault kv get -format=json secrets/$1" 2>/dev/null | jq -r '.data.data.UsernamePassword.password // empty' 2>/dev/null || true
}
# Factory-default creds — the INITIAL login site-explorer uses before rotating.
# Host and DPU factories DIFFER (see constants above); the wrong one yields 401
# Unauthorized on every host BMC and a permanent AvoidLockout latch.
_kv_put "machines/all_hosts/factory_default/bmc-metadata-items/${HOST_BMC_VENDOR}" root "$FACTORY_HOST_BMC_PASSWORD"
ok "factory host cred:  .../${HOST_BMC_VENDOR} = root/${FACTORY_HOST_BMC_PASSWORD}"
_kv_put "machines/all_dpus/factory_default/bmc-metadata-items/root" root "$FACTORY_DPU_BMC_PASSWORD"
ok "factory DPU cred:   .../root = root/${FACTORY_DPU_BMC_PASSWORD}"
# Site-wide root — the rotation target; must differ from both factory passwords.
_cur="$(_kv_password machines/bmc/site/root)"
if [[ -z "$_cur" || "$_cur" == "$FACTORY_HOST_BMC_PASSWORD" || "$_cur" == "$FACTORY_DPU_BMC_PASSWORD" ]]; then
    _kv_put "machines/bmc/site/root" "$BMC_USERNAME" "$BMC_PASSWORD"
    ok "machines/bmc/site/root seeded (${BMC_USERNAME}/**** — distinct from factory)"
else
    ok "machines/bmc/site/root already present with a non-factory password"
fi
# Self-heal stale per-MAC rotated creds (machines/bmc/<mac>/root). site-explorer
# writes one per BMC after rotating its password to the site root; entries
# surviving from a previous deployment poison a fresh one — the fresh mock is at
# the factory password, but the per-MAC entry makes site-explorer present the
# old rotated password: 401 → permanent AvoidLockout. Only safe to purge when
# the machine graph is empty (live machines' per-MAC creds are real).
# "Fresh deployment" = no machines AND no interfaces. machines==0 alone is
# NOT enough: mid-ingestion (interfaces DHCP'd, endpoints explored, machines
# not yet created) the per-MAC creds are LIVE — purging them forces every
# endpoint back through the credential fallback.
if [[ "$(psql_count "SELECT count(*) FROM machines;")" == "0" \
   && "$(psql_count "SELECT count(*) FROM machine_interfaces;")" == "0" ]]; then
    # Batched server-side in ONE kubectl exec — thousands of entries at scale;
    # one exec per deletion takes hours.
    _n="$(vault_cmd 'count=0
for m in $(vault kv list -format=yaml secrets/machines/bmc 2>/dev/null | sed "s/^- //" | grep -v "^site/"); do
  vault kv metadata delete "secrets/machines/bmc/${m%/}/root" >/dev/null 2>&1 && count=$((count+1))
done
echo $count' || echo 0)"
    if [[ "${_n:-0}" != "0" ]]; then
        warn "purged ${_n} stale per-MAC BMC creds from a previous deployment (machine graph was empty)"
    fi
fi
# site-explorer's check_preconditions ALSO requires the DPU + Host site_default
# UEFI creds to have a NON-EMPTY password. The nico-prereqs kvSeeds create these
# with empty passwords ("SITE SECRET: populate per site"), which fails the check
# with "vault does not have a valid password entry". Seed a non-empty value if
# the current password is empty/absent (the mock BMC does not validate it).
_seed_uefi() {   # $1=vault path  $2=password
    local path="$1" pw="$2" cur
    cur="$(_kv_password "$path")"
    if [[ -n "$cur" ]]; then
        ok "precondition cred present + valid: $path"
    else
        vault_cmd "echo '{\"UsernamePassword\":{\"username\":\"admin\",\"password\":\"${pw}\"}}' | vault kv put secrets/$path -" >/dev/null \
            || die "failed to seed UEFI cred $path"
        ok "seeded UEFI cred (was empty): $path"
    fi
}
_seed_uefi "machines/all_dpus/site_default/uefi-metadata-items/auth"  "$UEFI_DPU_PASSWORD"
_seed_uefi "machines/all_hosts/site_default/uefi-metadata-items/auth" "$UEFI_HOST_PASSWORD"

# =============================================================================
# Phase 4b — DPF operator simulator (default ON; bypass with --skip-dpf-sim)
# =============================================================================
phase "Phase 4b — DPF operator simulator"
# The simulator deploys by default ONLY when the nico-core config actually
# enables DPF ([dpf] enabled = true, site config overriding global) — on a
# non-DPF site there is nothing for it to drive, and auto-deploying DPF
# machinery there would be a nasty surprise on a production cluster.
_toml_dpf_enabled() {   # $1 = toml text → prints the [dpf] enabled value, "" if absent
    printf '%s\n' "$1" | grep -vE '^[[:space:]]*#' | awk '
        /^\[dpf\]/        {in_dpf=1; next}
        /^\[/             {in_dpf=0}
        in_dpf && /^[[:space:]]*enabled[[:space:]]*=/ {
            gsub(/[[:space:]]/,""); sub(/^enabled=/,""); print; exit }'
}
_cm_key() {   # $1=configmap $2=key → prints the key data, "" if absent
    kubectl get cm "$1" -n "$NICO_SYSTEM_NS" -o jsonpath="{.data.$2}" 2>/dev/null || true
}
DPF_ENABLED=""
for _src in \
    "nico-api-site-config-files nico-api-site-config\.toml" \
    "nico-api-site-config-files carbide-api-site-config\.toml" \
    "nico-api-config-files nico-api-config\.toml" \
    "nico-api-config-files carbide-api-config\.toml"; do
    # first hit wins: site config overrides global
    [[ -n "$DPF_ENABLED" ]] && break
    DPF_ENABLED="$(_toml_dpf_enabled "$(_cm_key $_src)")"
done

if [[ "$DPF_ENABLED" != "true" ]]; then
    ok "DPF is not enabled in the nico-core config ([dpf] enabled = ${DPF_ENABLED:-absent}) — skipping DPF prerequisites and simulator"
else
    # -------------------------------------------------------------------------
    # DPF-enabled INGESTION PREREQUISITES — applied whether or not the
    # simulator is deployed (--skip-dpf-sim only skips the simulator itself).
    # These gate machine INGESTION, not just the DPF walk: without them the
    # last hosts never finish creating and ingestion stalls short of the
    # target (learned the hard way — a --skip-dpf-sim run wedged at 2838/3000).
    # -------------------------------------------------------------------------
    # 1. nico-api's access to the DPF namespace for the DPF SDK (mirrors
    #    helm/charts/nico-api/templates/dpf-rbac.yaml on the setup-dpf-install
    #    branch, not on main yet — drop this once the chart ships it).
    kubectl apply -f - <<NICOAPIDPF >/dev/null
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: nico-api-dpf
  namespace: ${DPF_NAMESPACE}
rules:
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["bfbs", "bluefieldsoftwares", "dpudevices", "dpunodes"]
    verbs: ["get", "list", "create", "patch", "delete"]
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["dpus"]
    verbs: ["get", "list", "watch", "patch", "delete"]
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["dpunodemaintenances"]
    verbs: ["get", "patch"]
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["dpuflavors"]
    verbs: ["get", "create"]
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["dpusets"]
    verbs: ["get"]
  - apiGroups: ["provisioning.dpu.nvidia.com"]
    resources: ["dpuclusters"]
    verbs: ["get", "list"]
  - apiGroups: ["svc.dpu.nvidia.com"]
    resources: ["dpudeployments", "dpuservices", "dpuservicechains", "dpuserviceinterfaces", "dpuservicetemplates", "dpuserviceconfigurations", "dpuservicenads"]
    verbs: ["get", "list", "create", "patch", "delete"]
  - apiGroups: ["operator.dpu.nvidia.com"]
    resources: ["dpfoperatorconfigs"]
    verbs: ["get", "patch"]
  - apiGroups: [""]
    resources: ["secrets"]
    verbs: ["get", "create"]
  # DPF SDK init PATCHes this one Secret on startup; scope the grant to it
  # rather than every Secret in the namespace.
  - apiGroups: [""]
    resources: ["secrets"]
    resourceNames: ["bmc-shared-password"]
    verbs: ["patch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: nico-api-dpf
  namespace: ${DPF_NAMESPACE}
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: nico-api-dpf
subjects:
  - kind: ServiceAccount
    name: nico-api
    namespace: ${NICO_SYSTEM_NS}
NICOAPIDPF
    ok "nico-api-dpf Role/RoleBinding applied in ${DPF_NAMESPACE}"

    # 2. Post-#3561 cores resolve DiscoverMachine callers by TCP source IP;
    #    every simulated machine shares the MAT pod IP, so without this flag
    #    discovery-gated creation and agent self-discovery fail. Only set +
    #    restart when not already true — re-runs must not bounce a healthy
    #    nico-api.
    _INSECURE_NOW="$(kubectl get deploy nico-api -n "$NICO_SYSTEM_NS" \
        -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CARBIDE_API_ALLOW_INSECURE_DISCOVERY")].value}' 2>/dev/null || true)"
    if [[ "$_INSECURE_NOW" != "true" ]]; then
        info "enabling CARBIDE_API_ALLOW_INSECURE_DISCOVERY on nico-api (one-time restart)"
        kubectl -n "$NICO_SYSTEM_NS" set env deployment/nico-api CARBIDE_API_ALLOW_INSECURE_DISCOVERY=true >/dev/null
        kubectl -n "$NICO_SYSTEM_NS" rollout status deployment/nico-api --timeout=300s >/dev/null \
            || warn "nico-api rollout did not complete in time; continuing"
    fi
    ok "allow_insecure_discovery enabled on nico-api (test-env only; see #3561)"
fi

# -----------------------------------------------------------------------------
# Simulator deployment — the part --skip-dpf-sim actually skips.
# -----------------------------------------------------------------------------
if $SKIP_DPF_SIM; then
    warn "--skip-dpf-sim: prerequisites applied but the simulator is NOT deployed."
    warn "  Ingestion completes; hosts then park in dpuinit until a DPF operator"
    warn "  (or a separately-deployed simulator) drives DPU.status.phase."
elif [[ "$DPF_ENABLED" != "true" ]]; then
    : # non-DPF site: nothing to deploy (already reported above)
else
    # GUARDRAIL: never beside a real operator — both would drive
    # DPU.status.phase and fight. If the real DPF stack is deployed, this is
    # not a simulation-only cluster: refuse loudly rather than risk it.
    _REAL_DPF="$(kubectl get deploy -n "$DPF_NAMESPACE" -o name 2>/dev/null \
        | grep -vE 'deployment.apps/dpf-sim-controller$' | grep -E 'dpf.*operator|operator.*dpf' || true)"
    if [[ -n "$_REAL_DPF" ]]; then
        die "the real DPF operator is deployed in ${DPF_NAMESPACE} (${_REAL_DPF#deployment.apps/}).
       The DPF simulator must NOT run beside it — both drive DPU.status.phase.
       To bring up the simulator, remove the real DPF operator first (this must
       be a simulation-only cluster); to keep the real operator, re-run with
       --skip-dpf-sim."
    fi
    command -v make >/dev/null 2>&1 || die "make is required for the simulator deploy (or pass --skip-dpf-sim)"
    [[ -d "$DPF_SIM_DIR" ]] || die "simulator module not found at ${DPF_SIM_DIR} (or pass --skip-dpf-sim)"
    if [[ -z "$DPF_SIM_IMAGE" ]]; then
        [[ -n "${NICO_IMAGE_REGISTRY:-}" ]] \
            || die "set DPF_SIM_IMAGE (or NICO_IMAGE_REGISTRY) for the dpf-sim-controller image, or pass --skip-dpf-sim"
        DPF_SIM_IMAGE="${NICO_IMAGE_REGISTRY}/dpf-sim-controller:${DPF_SIM_IMAGE_TAG}"
    fi

    kubectl get ns "$DPF_NAMESPACE" >/dev/null 2>&1 || kubectl create ns "$DPF_NAMESPACE" >/dev/null
    _DPF_PULL_ARGS=()
    if [[ -n "${REGISTRY_PULL_SECRET:-}" ]]; then
        kubectl -n "$DPF_NAMESPACE" create secret docker-registry dpf-sim-pull \
            --docker-server="${DPF_SIM_IMAGE%%/*}" \
            --docker-username="$REGISTRY_PULL_USERNAME" \
            --docker-password="$REGISTRY_PULL_SECRET" \
            --dry-run=client -o yaml | kubectl apply -f - >/dev/null
        _DPF_PULL_ARGS=(PULL_SECRET=dpf-sim-pull)
    fi
    # `make deploy` = DPF CRDs (waits for Established) + RBAC + Deployment +
    # rollout; everything idempotent. Keep the log for failure triage.
    _DPF_LOG="$(mktemp)"
    if ! make -C "$DPF_SIM_DIR" deploy IMG="$DPF_SIM_IMAGE" DPF_NAMESPACE="$DPF_NAMESPACE" ${_DPF_PULL_ARGS[@]+"${_DPF_PULL_ARGS[@]}"} >"$_DPF_LOG" 2>&1; then
        tail -8 "$_DPF_LOG" >&2; rm -f "$_DPF_LOG"
        die "simulator deploy failed (make -C ${DPF_SIM_DIR} deploy)"
    fi
    rm -f "$_DPF_LOG"
    ok "dpf-sim-controller deployed: ${DPF_SIM_IMAGE} in ${DPF_NAMESPACE}"
fi

# =============================================================================
# Phase 5 — configure nico-core site config for the selected mode
#   override: set site_explorer.bmc_proxy (GOTCHA: field name; FQDN required)
#   scale:    REMOVE bmc_proxy, add simulated networks + throughput knobs
# =============================================================================
phase "Phase 5 — nico-core site config (${MAT_MODE} mode)"
if $SKIP_NICO_CORE_CONFIG; then
    warn "--skip-nico-core-config set; configure [site_explorer]/[networks] manually for ${MAT_MODE} mode"
elif [[ "$MAT_MODE" == "scale" ]]; then
    CM_JSON="$(mktemp)"
    kubectl get cm nico-api-site-config-files -n "$NICO_SYSTEM_NS" -o json > "$CM_JSON" 2>/dev/null \
        || die "nico-api-site-config-files configmap not found"
    _PATCH_RESULT="$(SCALE_OOB_PREFIX="$SCALE_OOB_PREFIX" SCALE_OOB_GW="$SCALE_OOB_GW" \
        SCALE_ADMIN_PREFIX="$SCALE_ADMIN_PREFIX" SCALE_ADMIN_GW="$SCALE_ADMIN_GW" \
        SCALE_RESERVE="$SCALE_RESERVE" \
        BMC_PROXY="${BMC_MOCK_FQDN}:${BMC_MOCK_PORT}" \
        KNOB_CONC="$SCALE_CONCURRENT_EXPLORATIONS" KNOB_EPR="$SCALE_EXPLORATIONS_PER_RUN" \
        KNOB_MCPR="$SCALE_MACHINES_CREATED_PER_RUN" \
        KNOB_SE_RUN_INTERVAL="${SCALE_RUN_INTERVAL:-}" \
        KNOB_FW_CONC="${SCALE_FW_CONCURRENCY:-}" \
        KNOB_FW_RUN_INTERVAL="${SCALE_FW_RUN_INTERVAL:-}" \
        KNOB_STATE_CONC="${SCALE_STATE_MAX_CONCURRENCY:-200}" \
        python3 - "$CM_JSON" <<'PY'
import json, os, sys
path = sys.argv[1]
cm = json.load(open(path))
env = os.environ
# lines managed by this script inside [site_explorer]
drop = ["bmc_proxy", "override_target_host", "override_target_ip", "override_target_port",
        "concurrent_explorations", "explorations_per_run", "machines_created_per_run"]
knobs = [
    # PROXY-DIRECT: the Redfish client injects "Forwarded: host=<BMC IP>"
    # whenever bmc_proxy is set; the mock's registry routes on it. One
    # ClusterIP service serves the whole simulated fleet.
    # CONTROLLER MODE (MAT_MULTIPOD=1) is the exception: mat-k8s-controller
    # gives every BMC its own ClusterIP and site-explorer must dial those
    # directly, so bmc_proxy stays dropped (values/machine-a-tron-scale*.yaml
    # documents this requirement -- but ONLY bmc_proxy is dropped there; the
    # throughput knobs below must still be written in both modes.
    *([] if os.environ.get("MAT_MULTIPOD") == "1"
        else [f'      bmc_proxy = "{env["BMC_PROXY"]}"']),
    f'      concurrent_explorations = {env["KNOB_CONC"]}',
    f'      explorations_per_run = {env["KNOB_EPR"]}',
    f'      machines_created_per_run = {env["KNOB_MCPR"]}',
]
# Optional tuning override (issue #3738): the explore->create cycle period.
# Only managed when explicitly set, so default runs keep the site's value.
if env.get("KNOB_SE_RUN_INTERVAL"):
    drop.append("run_interval")   # scoped to [site_explorer] by insertion point
    knobs.append(f'      run_interval = "{env["KNOB_SE_RUN_INTERVAL"]}"')

# Optional tuning overrides living OUTSIDE [site_explorer] (issue #3738).
# When the target section already exists in the site config, its managed keys
# are rewritten IN PLACE (scoped, like [site_explorer]); a section that is
# absent is emitted in a sentinel-delimited tail block regenerated each run.
# Duplicating an existing table would be a TOML parse error — a site template
# ships both [firmware_global] and [machine_state_controller].
TUNING_BEGIN = "# --- BEGIN scale tuning overrides (managed by setup-machine-a-tron.sh) ---"
TUNING_END   = "# --- END scale tuning overrides ---"
# Managed tuning keys are ALWAYS dropped from their sections in scale mode —
# with no override env set they revert to the build's defaults, so one run's
# override can never leak into the next (run independence for #3738). Set
# them via the SCALE_* envs, not by editing these keys in the site config.
MANAGED_TUNING = {
    "[firmware_global]": ("concurrency_limit", "run_interval"),
    "[machine_state_controller.controller]": ("max_concurrency",),
}
# section header -> (managed key names, replacement lines)
tuning_secs = {}
if env.get("KNOB_FW_CONC") or env.get("KNOB_FW_RUN_INTERVAL"):
    keys, lines = [], []
    if env.get("KNOB_FW_CONC"):
        keys.append("concurrency_limit"); lines.append(f'concurrency_limit = {env["KNOB_FW_CONC"]}')
    if env.get("KNOB_FW_RUN_INTERVAL"):
        keys.append("run_interval"); lines.append(f'run_interval = "{env["KNOB_FW_RUN_INTERVAL"]}"')
    tuning_secs["[firmware_global]"] = (tuple(keys), lines)
if env.get("KNOB_STATE_CONC"):
    tuning_secs["[machine_state_controller.controller]"] = (
        ("max_concurrency",), [f'max_concurrency = {env["KNOB_STATE_CONC"]}'])
networks = f'''
# --- simulated networks for machine-a-tron scale testing (managed by
# --- setup-machine-a-tron.sh --scale; safe to leave in place) ---
[networks.simulated-oob]
type = "underlay"
prefix = "{env["SCALE_OOB_PREFIX"]}"
gateway = "{env["SCALE_OOB_GW"]}"
mtu = 9000
reserve_first = {env["SCALE_RESERVE"]}

[networks.simulated-admin]
type = "admin"
prefix = "{env["SCALE_ADMIN_PREFIX"]}"
gateway = "{env["SCALE_ADMIN_GW"]}"
mtu = 9000
reserve_first = {env["SCALE_RESERVE"]}
'''
# Machine creation allocates one loopback IP per machine from pools.lo-ip —
# site templates ship tiny ranges (often only a few addresses) that exhaust
# ("Resource pool lo-ip is empty"). Pools DO reconcile at startup (unlike
# networks), so appending a simulated range takes effect on restart.
SIM_LO = ', { start = "10.103.0.1", end = "10.103.63.254" }]'
changed = False
for k, v in cm["data"].items():
    if "[site_explorer]" not in v:
        continue
    out, in_lo = [], False
    # Strip any previously-managed tuning block before re-rendering.
    if TUNING_BEGIN in v:
        pre, _, rest = v.partition(TUNING_BEGIN)
        _, _, post = rest.partition(TUNING_END)
        v_work = pre.rstrip("\n") + "\n" + post.lstrip("\n")
    else:
        v_work = v
    # The sim lo-ip range may already be present — the site's values file can
    # carry it (possibly as a MULTI-LINE ranges array). Only splice it into a
    # SINGLE-LINE `ranges = [...]` when it is genuinely absent; otherwise leave
    # the array untouched. The actual pool CAPACITY is guaranteed by the
    # Phase 5 resource_pool DB widening regardless, so a no-op here is safe.
    SIM_LO_PRESENT = "10.103.0.1" in v_work
    in_sec = ""
    seen_secs = set()
    for ln in v_work.splitlines():
        s = ln.strip()
        if s.startswith("["):
            in_sec = s
        # drop-lists are scoped per section, so a key with the same name in
        # another section is never touched.
        if in_sec == "[site_explorer]" and any(t in ln for t in drop) and not s.startswith("["):
            continue
        if in_sec in MANAGED_TUNING and not s.startswith("["):
            key = s.split("=", 1)[0].strip()
            if key in MANAGED_TUNING[in_sec]:
                continue   # managed key: replaced when overridden, reverted when not
        if s.startswith("[pools."):
            in_lo = (s == "[pools.lo-ip]")
        if (in_lo and s.startswith("ranges") and not SIM_LO_PRESENT
                and ln.rstrip().endswith("]")):
            r = ln.rstrip(); idx = r.rfind("]")
            ln = r[:idx] + SIM_LO + r[idx+1:]
        out.append(ln)
        if s == "[site_explorer]":
            out.extend(knobs)
        if s in tuning_secs:
            seen_secs.add(s)
            out.extend(tuning_secs[s][1])
    new = "\n".join(out) + ("\n" if v_work.endswith("\n") else "")
    if "[networks.simulated-oob]" not in new:
        new = new.rstrip("\n") + "\n" + networks
    # Sections not present in the file are emitted in the sentinel tail.
    # (A dotted subtable like [machine_state_controller.controller] is valid
    # there even when its parent table exists elsewhere.)
    tail = []
    for sec, (_keys, lines) in tuning_secs.items():
        if sec not in seen_secs:
            tail.append(sec); tail.extend(lines)
    if tail:
        new = new.rstrip("\n") + "\n\n" + TUNING_BEGIN + "\n" + "\n".join(tail) + "\n" + TUNING_END + "\n"
    if new != v:
        cm["data"][k] = new
        changed = True
for f in ("resourceVersion","uid","creationTimestamp","managedFields"):
    cm["metadata"].pop(f, None)
json.dump(cm, open(path, "w"))
print("changed" if changed else "nochange")
PY
)"
    if [[ "$_PATCH_RESULT" == "changed" ]]; then
        kubectl apply -f "$CM_JSON" >/dev/null
        info "scale config applied (proxy-direct bmc_proxy, simulated networks, knobs, lo-ip); restarting nico-api"
        kubectl rollout restart deployment/nico-api -n "$NICO_SYSTEM_NS" >/dev/null
        kubectl rollout status deployment/nico-api -n "$NICO_SYSTEM_NS" --timeout=180s >/dev/null \
            || warn "nico-api rollout did not complete in time; continuing"
        ok "scale networks: oob ${SCALE_OOB_PREFIX} (gw ${SCALE_OOB_GW}), admin ${SCALE_ADMIN_PREFIX} (gw ${SCALE_ADMIN_GW})"
        ok "site_explorer knobs: concurrent=${SCALE_CONCURRENT_EXPLORATIONS} per_run=${SCALE_EXPLORATIONS_PER_RUN} create/run=${SCALE_MACHINES_CREATED_PER_RUN}"
        [[ -n "${SCALE_RUN_INTERVAL:-}${SCALE_FW_CONCURRENCY:-}${SCALE_FW_RUN_INTERVAL:-}${SCALE_STATE_MAX_CONCURRENCY:-}" ]] && \
            ok "tuning overrides (#3738): se.run_interval=${SCALE_RUN_INTERVAL:-·} fw.concurrency=${SCALE_FW_CONCURRENCY:-·} fw.run_interval=${SCALE_FW_RUN_INTERVAL:-·} state.max_concurrency=${SCALE_STATE_MAX_CONCURRENCY:-·}"
    else
        ok "scale config already in place"
    fi

    # --- ensure the simulated network SEGMENTS exist -------------------------
    # Config-driven segment creation (create_initial_networks) is bootstrap-
    # once: it SKIPS entirely when the DB has multiple DNS domains ("we
    # probably created the network much earlier", crates/api-core/src/db_init.rs).
    # On an established site the new [networks.*] stanzas therefore never
    # materialize and every mat DHCP fails with "No network segment defined
    # for relay addresses". Fallback: clone an existing segment of the same
    # type (identity fields overridden, vlan/vni cleared) + insert the prefix.
    _ensure_segment() {   # $1=name $2=type-ilike $3=prefix $4=gateway $5=reserve
        local name="$1" typ="$2" pfx="$3" gw="$4" rsv="$5"
        if [[ "$(psql_count "SELECT count(*) FROM network_segments WHERE name='${name}';")" != "0" ]]; then
            ok "segment ${name} present"
            return
        fi
        warn "segment ${name} missing (config seeding is bootstrap-once on multi-domain sites) — creating from template"
        # allocation_strategy is forced to 'dynamic': templates may be
        # 'reserved' (static-assignments segments), which rejects every mat
        # DHCP with "configured for static DHCP leases only".
        psql_q "INSERT INTO network_segments
            SELECT (jsonb_populate_record(ns, jsonb_build_object(
                'id', gen_random_uuid()::text, 'name', '${name}',
                'allocation_strategy', 'dynamic',
                'vlan_id', NULL, 'vni_id', NULL))).*
            FROM network_segments ns
            WHERE ns.network_segment_type::text ILIKE '${typ}' LIMIT 1;" >/dev/null \
            || die "failed to create segment ${name} (no ${typ} template segment?)"
        # svi_ip = gateway: the FNN host network-config builder requires an
        # SVI IP on L2 segments — without it get_managed_host_network_config
        # fails with "SVI IP is not allocated" and hosts park in dpuinit at
        # waitingfornetworkconfig.
        psql_q "INSERT INTO network_prefixes (segment_id, prefix, gateway, num_reserved, svi_ip)
            SELECT id, '${pfx}'::cidr, '${gw}'::inet, ${rsv}, '${gw}'::inet
            FROM network_segments WHERE name='${name}';" >/dev/null \
            || die "failed to add prefix ${pfx} to segment ${name}"
        ok "segment ${name} created: ${pfx} (gw ${gw}, svi ${gw}, reserved ${rsv})"
    }
    _ensure_segment "simulated-oob"   "underlay" "$SCALE_OOB_PREFIX"   "$SCALE_OOB_GW"   "$SCALE_RESERVE"
    _ensure_segment "simulated-admin" "admin"    "$SCALE_ADMIN_PREFIX" "$SCALE_ADMIN_GW" "$SCALE_RESERVE"
    # Backfill segments created before this script seeded svi_ip (or created
    # by config-driven bootstrap, which leaves it NULL).
    _SVI_FIXED="$(psql_q "WITH fixed AS (
            UPDATE network_prefixes np SET svi_ip = np.gateway
            FROM network_segments ns
            WHERE ns.id = np.segment_id
              AND ns.name IN ('simulated-oob','simulated-admin')
              AND np.svi_ip IS NULL
            RETURNING 1)
        SELECT count(*) FROM fixed;" || echo 0)"
    if [[ "${_SVI_FIXED:-0}" != "0" ]]; then
        ok "backfilled svi_ip on ${_SVI_FIXED} simulated prefix(es)"
    else
        ok "simulated prefixes already carry svi_ip"
    fi

    # --- widen the lo-ip resource pool ---------------------------------------
    # Machine creation allocates one loopback IP per machine from resource_pool
    # rows. Pool DEFINITIONS are seed-once ("Declaration has drifted since
    # seed ... not re-applying", crates/api-db/src/resource_pool.rs), so config
    # changes to [pools.lo-ip] are IGNORED on established sites — rows must be
    # inserted directly. Site templates ship tiny ranges (a few addresses)
    # that exhaust instantly ("Resource pool lo-ip is empty").
    _LO_FREE="$(psql_count "SELECT count(*) FROM resource_pool WHERE name='lo-ip' AND allocated IS NULL;")"
    if (( _LO_FREE < HOST_COUNT * (1 + DPU_PER_HOST) )); then
        info "widening lo-ip pool (${_LO_FREE} free < needed) with simulated range 10.103.0.1-10.103.63.254"
        psql_q "INSERT INTO resource_pool (name, value, value_type, auto_assign, state, state_version, created)
            SELECT 'lo-ip', host('10.103.0.0'::inet + g), 'ipv4', true,
                   '{\\\"state\\\":\\\"free\\\"}'::jsonb,
                   (SELECT state_version FROM resource_pool WHERE name='lo-ip' LIMIT 1),
                   now()
            FROM generate_series(1, 16382) g
            WHERE NOT EXISTS (SELECT 1 FROM resource_pool rp WHERE rp.name='lo-ip' AND rp.value = host('10.103.0.0'::inet + g));" >/dev/null \
            || die "failed to widen lo-ip pool"
        ok "lo-ip pool: $(psql_count "SELECT count(*) FROM resource_pool WHERE name='lo-ip' AND allocated IS NULL;") free"
    else
        ok "lo-ip pool has ${_LO_FREE} free addresses"
    fi

    # --- widen the fnn-asn resource pool --------------------------------------
    # Under FNN every DPU is assigned an ASN from the fnn-asn pool; site
    # templates seed a tiny range (dev sites: ~94) that a fleet exhausts
    # instantly, after which every get_managed_host_network_config fails with
    # "FNN configured but DPU ... has not been assigned an ASN" and all hosts
    # park at waitingfornetworkconfig. Same seed-once caveat as lo-ip: rows
    # must be inserted directly. Extend contiguously from the pool's own max
    # (4-byte private ASN space, nowhere near the 4294967294 ceiling on dev
    # pools). Skipped when the site has no fnn-asn pool (non-FNN site).
    _ASN_NEED=$(( HOST_COUNT * DPU_PER_HOST ))
    _ASN_FREE="$(psql_count "SELECT count(*) FROM resource_pool WHERE name='fnn-asn' AND allocated IS NULL;")"
    _ASN_TOTAL="$(psql_count "SELECT count(*) FROM resource_pool WHERE name='fnn-asn';")"
    if (( _ASN_TOTAL == 0 )); then
        ok "no fnn-asn pool on this site (non-FNN) — skipping"
    elif (( _ASN_FREE < _ASN_NEED )); then
        info "widening fnn-asn pool (${_ASN_FREE} free < ${_ASN_NEED} needed, one per DPU)"
        psql_q "INSERT INTO resource_pool (name, value, value_type, auto_assign, state, state_version, created)
            SELECT 'fnn-asn', (b.mx + g)::text, 'integer', true,
                   '{\\\"state\\\":\\\"free\\\"}'::jsonb, b.sv, now()
            FROM (SELECT max(value::bigint) AS mx,
                         (SELECT state_version FROM resource_pool WHERE name='fnn-asn' LIMIT 1) AS sv
                  FROM resource_pool WHERE name='fnn-asn') b,
                 generate_series(1, $(( _ASN_NEED * 2 ))) g
            WHERE b.mx IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM resource_pool rp
                              WHERE rp.name='fnn-asn' AND rp.value = (b.mx + g)::text);" >/dev/null \
            || die "failed to widen fnn-asn pool"
        ok "fnn-asn pool: $(psql_count "SELECT count(*) FROM resource_pool WHERE name='fnn-asn' AND allocated IS NULL;") free"
    else
        ok "fnn-asn pool has ${_ASN_FREE} free ASNs"
    fi
else
    CM_JSON="$(mktemp)"
    kubectl get cm nico-api-site-config-files -n "$NICO_SYSTEM_NS" -o json > "$CM_JSON" 2>/dev/null \
        || die "nico-api-site-config-files configmap not found"
    if grep -q "bmc_proxy = \"${BMC_MOCK_FQDN}:${BMC_MOCK_PORT}\"" "$CM_JSON"; then
        ok "bmc_proxy already configured"
    else
        _PATCH_RESULT="$(BMC_PROXY="${BMC_MOCK_FQDN}:${BMC_MOCK_PORT}" python3 - "$CM_JSON" <<'PY'
import json, os, sys
proxy = os.environ["BMC_PROXY"]
path = sys.argv[1]
cm = json.load(open(path))
line = f'      bmc_proxy = "{proxy}"'
drop = ("bmc_proxy", "override_target_host", "override_target_ip", "override_target_port")
changed = False
for k, v in cm["data"].items():
    if "[site_explorer]" not in v:
        continue
    out = []
    for ln in v.splitlines():
        if any(t in ln for t in drop):   # strip any stale/legacy proxy lines
            continue
        out.append(ln)
        if ln.strip() == "[site_explorer]":
            out.append(line)             # insert the correct FQDN bmc_proxy
    cm["data"][k] = "\n".join(out) + ("\n" if v.endswith("\n") else "")
    changed = True
for f in ("resourceVersion","uid","creationTimestamp","managedFields"):
    cm["metadata"].pop(f, None)
json.dump(cm, open(path, "w"))
print("changed" if changed else "nochange")
PY
)"
        if [[ "$_PATCH_RESULT" == "changed" ]]; then
            kubectl apply -f "$CM_JSON" >/dev/null
            info "configmap patched; restarting nico-api to load bmc_proxy"
            kubectl rollout restart deployment/nico-api -n "$NICO_SYSTEM_NS" >/dev/null
            kubectl rollout status deployment/nico-api -n "$NICO_SYSTEM_NS" --timeout=180s >/dev/null \
                || warn "nico-api rollout did not complete in time; continuing"
            ok "bmc_proxy set to ${BMC_MOCK_FQDN}:${BMC_MOCK_PORT}"
        else
            ok "no [site_explorer] section found to patch — check the configmap manually"
        fi
    fi
fi

# =============================================================================
# Phase 6 — resolve DHCP relays + sizing check
# =============================================================================
phase "Phase 6 — DHCP relays + pool sizing (${MAT_MODE})"
if [[ "$MAT_MODE" == "scale" ]]; then
    # scale mode uses the SIMULATED networks added in Phase 5 — constants,
    # no live-config parsing needed.
    OOB_PREFIX="$SCALE_OOB_PREFIX";   OOB_DHCP_RELAY="${OOB_DHCP_RELAY:-$SCALE_OOB_GW}"
    ADMIN_PREFIX="$SCALE_ADMIN_PREFIX"; ADMIN_DHCP_RELAY="${ADMIN_DHCP_RELAY:-$SCALE_ADMIN_GW}"
    OOB_RESERVE="$SCALE_RESERVE"; ADMIN_RESERVE="$SCALE_RESERVE"
else
    SITE_CFG="$(kubectl get cm nico-api-site-config-files -n "$NICO_SYSTEM_NS" \
        -o jsonpath='{.data.nico-api-site-config\.toml}' 2>/dev/null || true)"
    # older deployments carry the config under the carbide-* key only
    [[ -n "$SITE_CFG" ]] || SITE_CFG="$(kubectl get cm nico-api-site-config-files -n "$NICO_SYSTEM_NS" \
        -o jsonpath='{.data.carbide-api-site-config\.toml}' 2>/dev/null || true)"
    # gateway lines appear as: gateway = "10.x.y.z" (admin then underlay in template order).
    # Portable parse (no mapfile / negative index — macOS ships bash 3.2).
    # Comment lines are stripped first: the config template carries commented
    # examples (e.g. "#   reserve_first = 5") that would otherwise be picked up.
    SITE_CFG_CODE="$(printf '%s\n' "$SITE_CFG" | grep -vE '^[[:space:]]*#' || true)"
    GW_LIST="$(printf '%s\n' "$SITE_CFG_CODE" | grep -oE 'gateway = "[0-9.]+"' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' || true)"
    PFX_LIST="$(printf '%s\n' "$SITE_CFG_CODE" | grep -oE 'prefix = "[0-9./]+"' | grep -oE '[0-9./]+' || true)"
    RSV_LIST="$(printf '%s\n' "$SITE_CFG_CODE" | grep -oE 'reserve_first = [0-9]+' | grep -oE '[0-9]+' || true)"
    # admin segment is the FIRST prefix/gateway pair, OOB/underlay the LAST.
    ADMIN_PREFIX="$(printf '%s\n' "$PFX_LIST" | head -1)"
    OOB_PREFIX="$(printf '%s\n' "$PFX_LIST" | tail -1)"
    OOB_DHCP_RELAY="${OOB_DHCP_RELAY:-$(printf '%s\n' "$GW_LIST" | tail -1)}"
    ADMIN_DHCP_RELAY="${ADMIN_DHCP_RELAY:-$(printf '%s\n' "$GW_LIST" | head -1)}"
    # Usable IPs per pool = 2^(32-mask) − reserve_first − 1 (broadcast).
    # reserve_first covers the network address, gateway, and operator-reserved
    # leading addresses — verified live: /28 with reserve_first=5 → 10 usable.
    ADMIN_RESERVE="$(printf '%s\n' "$RSV_LIST" | head -1)"; ADMIN_RESERVE="${ADMIN_RESERVE:-5}"
    OOB_RESERVE="$(printf '%s\n' "$RSV_LIST" | tail -1)"; OOB_RESERVE="${OOB_RESERVE:-5}"
fi
[[ -n "$OOB_DHCP_RELAY" && -n "$ADMIN_DHCP_RELAY" ]] \
    || die "could not resolve DHCP relays; set OOB_DHCP_RELAY and ADMIN_DHCP_RELAY"
ok "OOB:   relay ${OOB_DHCP_RELAY}   prefix ${OOB_PREFIX:-unknown}"
ok "admin: relay ${ADMIN_DHCP_RELAY}   prefix ${ADMIN_PREFIX:-unknown}"
_usable() { local m="${1##*/}" r="$2"; local u=$(( (1 << (32 - m)) - r - 1 )); (( u < 0 )) && u=0; echo "$u"; }
# Demand per pool (measured live):
#   OOB   = hostCount*(1 + dpuPerHost)   — one BMC IP per host and per DPU
#   admin = hostCount*(dpuPerHost + 1)   — one host-PF IP per DPU at DHCP time,
#           PLUS one admin IP per host allocated by machine creation (creation
#           fails with "No IP addresses left in prefix <admin>" without it)
if [[ "${OOB_PREFIX:-}" == */* && "${ADMIN_PREFIX:-}" == */* ]]; then
    OOB_USABLE="$(_usable "$OOB_PREFIX" "$OOB_RESERVE")"; ADMIN_USABLE="$(_usable "$ADMIN_PREFIX" "$ADMIN_RESERVE")"
    # max hosts each pool supports, then take the min
    FIT_OOB=$(( OOB_USABLE / (1 + DPU_PER_HOST) ))
    FIT_ADMIN=$(( ADMIN_USABLE / (DPU_PER_HOST + 1) ))
    FIT=$(( FIT_OOB < FIT_ADMIN ? FIT_OOB : FIT_ADMIN ))
    info "pool fit: OOB ${OOB_PREFIX} ≈${OOB_USABLE} usable → ≤${FIT_OOB} hosts; admin ${ADMIN_PREFIX} ≈${ADMIN_USABLE} usable → ≤${FIT_ADMIN} hosts"
    if [[ "${MAT_MULTIPOD:-0}" == "1" ]]; then
        # Multipod: HOST_COUNT/DPU_PER_HOST are only the FIRST pod group, so
        # the single-pool clamp above is meaningless here. Two cases matter:
        #   * pods with their own OOB relay -> capacity is that pod's segment
        #   * pods SHARING a relay          -> their demand is additive on one
        #                                      pool, and must be checked
        # Parse every pod group out of the values file and validate per relay.
        _MP_REPORT="$(python3 - "$VALUES_FILE" "$OOB_PREFIX" "$OOB_RESERVE" <<'MPCHK'
import ipaddress, re, sys

values_file, default_prefix, default_reserve = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(values_file).read()

def groups_from_yaml(doc):
    """Structural walk of a parsed values doc -> [{hosts,dpus,relay}]."""
    out = []
    for pod in (doc.get("pods") or {}).values():
        if not isinstance(pod, dict):
            continue
        for grp in (pod.get("machines") or {}).values():
            if not isinstance(grp, dict):
                continue
            hosts = int(grp.get("hostCount") or 0)
            if hosts <= 0:
                continue
            out.append({
                "hosts": hosts,
                "dpus": int(grp.get("dpuPerHostCount") or 0),
                "relay": str(grp.get("oobDhcpRelayAddress") or "").strip(),
            })
    return out

yaml_groups = None
try:
    import yaml  # structural parse is authoritative when available
    doc = yaml.safe_load(text)
    if isinstance(doc, dict):
        yaml_groups = groups_from_yaml(doc)
except ImportError:
    yaml_groups = None
except Exception as e:                      # malformed values file
    print(f"SKIP values file is not valid YAML: {e}")
    sys.exit(0)

# Dependency-free scan: walk the values file line by line and close off a
# machine group whenever a line appears at or above the group's indentation.
# Each group contributes hostCount*(1+dpuPerHostCount) BMC IPs to its relay.
groups, cur, cur_indent = [], None, None
for raw in text.splitlines():
    if not raw.strip() or raw.lstrip().startswith("#"):
        continue
    indent = len(raw) - len(raw.lstrip())
    m = re.match(r'\s*([A-Za-z0-9_.-]+)\s*:\s*(\S.*)?$', raw)
    if not m:
        continue
    key, val = m.group(1), (m.group(2) or "").strip()
    # Close the group only on a real dedent: sibling keys (dpuRebootDelay, etc.)
    # sit at the same indent as hostCount and must not end the group.
    if cur is not None and cur_indent is not None and indent < cur_indent:
        groups.append(cur); cur, cur_indent = None, None
    if key == "hostCount":
        if cur is None:
            cur, cur_indent = {"hosts": 0, "dpus": 0, "relay": ""}, indent
        cur["hosts"] = int(re.sub(r"\D", "", val) or 0)
    elif key == "dpuPerHostCount" and cur is not None:
        cur["dpus"] = int(re.sub(r"\D", "", val) or 0)
    elif key == "oobDhcpRelayAddress" and cur is not None:
        cur["relay"] = val.strip('"\'')
if cur is not None:
    groups.append(cur)

if yaml_groups is not None:
    groups = yaml_groups            # structural parse wins
groups = [g for g in groups if g["hosts"] > 0]
if not groups:
    print("SKIP values file defines no pod groups with hostCount")
    sys.exit(0)

demand = {}
for g in groups:
    relay = g["relay"] or "<default>"
    demand[relay] = demand.get(relay, 0) + g["hosts"] * (1 + g["dpus"])

def usable(cidr, reserve):
    net = ipaddress.ip_network(cidr, strict=False)
    return max(net.num_addresses - int(reserve) - 1, 0)

# A relay documented in this file gets its own segment; find the prefix whose
# comment/segment block names it, else fall back to the site's OOB prefix.
def prefix_for(relay):
    if relay == "<default>":
        return default_prefix
    # Most specific containing prefix wins: a relay sits inside both its own
    # segment and the wide ServiceCIDR, and only the narrow one is its pool.
    best = None
    for cand in re.findall(r'([0-9]+(?:\.[0-9]+){3}/[0-9]+)', text):
        try:
            net = ipaddress.ip_network(cand, strict=False)
            if ipaddress.ip_address(relay) in net and (best is None or net.prefixlen > best.prefixlen):
                best = net
        except ValueError:
            pass
    return str(best) if best else default_prefix

problems, lines = [], []
for relay, need in sorted(demand.items()):
    cidr = prefix_for(relay)
    cap = usable(cidr, default_reserve)
    ok = need <= cap
    lines.append(f"  relay {relay}: needs {need} IPs, {cidr} provides ~{cap} -> {'OK' if ok else 'TOO SMALL'}")
    if not ok:
        problems.append(f"relay {relay} needs {need} IPs but {cidr} only provides ~{cap}")

print("\n".join(lines))
if problems:
    print("FAIL " + "; ".join(problems))
MPCHK
)"; _MP_RC=$?
        printf '%s\n' "$_MP_REPORT" | while IFS= read -r _l; do [[ -n "$_l" ]] && info "$_l"; done
        # Fail closed: a validator that cannot run must not look like a pass.
        if (( _MP_RC != 0 )); then
            die "multipod sizing check failed to run (rc=${_MP_RC}); refusing to deploy unvalidated. Output: ${_MP_REPORT:-<none>}"
        fi
        if [[ "$_MP_REPORT" == *FAIL* ]]; then
            die "multipod sizing: $(printf '%s' "$_MP_REPORT" | sed -n 's/^FAIL //p') — widen that segment's prefix, lower its hostCount, or give the pod its own relay"
        fi
        if [[ "$_MP_REPORT" == SKIP* ]]; then
            die "multipod sizing could not be validated: ${_MP_REPORT#SKIP } — set HOST_COUNT/DPU_PER_HOST explicitly or fix the values file so demand can be computed"
        fi
        ok "multipod sizing validated across all pod groups"
    elif (( HOST_COUNT > FIT )); then
        (( FIT < 1 )) && die "pools too small for even 1 host × ${DPU_PER_HOST} DPUs — widen the admin/OOB prefixes or lower DPU_PER_HOST"
        warn "requested ${HOST_COUNT} hosts exceeds pool capacity (${FIT}) — auto-fitting hostCount=${FIT}"
        warn "  (override with HOST_COUNT/DPU_PER_HOST env vars, or widen the site's DHCP prefixes)"
        confirm "Proceed with hostCount=${FIT} × ${DPU_PER_HOST} DPUs?" || die "aborted on sizing"
        HOST_COUNT="$FIT"
    fi
    NEED=$(( HOST_COUNT + HOST_COUNT * DPU_PER_HOST ))
    ok "sizing: ${HOST_COUNT} hosts × ${DPU_PER_HOST} DPUs → ${NEED} OOB + $(( HOST_COUNT * (DPU_PER_HOST + 1) )) admin IPs"
else
    NEED=$(( HOST_COUNT + HOST_COUNT * DPU_PER_HOST ))
    warn "could not parse both pool prefixes — skipping sizing check (need ${NEED} OOB IPs)"
fi

# =============================================================================
# Phase 7 — DB safety: restore machine_interfaces_deletion singleton  (GOTCHA)
# =============================================================================
phase "Phase 7 — DB safety checks"
# "!= 1" (not "== 0") so a transient query failure (empty result) also takes
# the restore path — the INSERT is idempotent (ON CONFLICT DO NOTHING).
SINGLETON="$(psql_count "SELECT count(*) FROM machine_interfaces_deletion WHERE id=1;")"
if [[ "$SINGLETON" != "1" ]]; then
    warn "machine_interfaces_deletion singleton (id=1) missing — restoring"
    warn "  (its absence breaks the machine_dhcp_records view → DiscoverDhcp 'no rows' errors)"
    psql_q "INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT (id) DO NOTHING;" >/dev/null
    ok "singleton restored"
else
    ok "machine_interfaces_deletion singleton present"
fi
# Surface any auto-assign resource pool that is already dry — an exhausted
# pool fails machine creation or network-config generation with errors that
# do not name the pool (e.g. "has not been assigned an ASN"), and allocation
# failures at creation time are permanent until an operator backfills. lo-ip
# and fnn-asn are widened automatically above; anything else dry needs a look.
_DRY_POOLS="$(psql_q "SELECT string_agg(name || ' (' || total || ' total)', ', ')
    FROM (SELECT name, count(*) AS total FROM resource_pool
          WHERE auto_assign GROUP BY name
          HAVING count(*) FILTER (WHERE allocated IS NULL) = 0) dry;" || true)"
if [[ -n "${_DRY_POOLS// /}" ]]; then
    warn "auto-assign pools with ZERO free entries: ${_DRY_POOLS}"
    warn "  → allocations from these will fail silently at machine creation;"
    warn "    widen them (resource_pool INSERT) before ingesting a fleet"
else
    ok "no auto-assign resource pool is exhausted"
fi

ORPHANS="$(psql_count "SELECT count(*) FROM machine_interfaces mi WHERE NOT EXISTS (SELECT 1 FROM machines m WHERE m.id = mi.machine_id);")"
MACHINES_NOW="$(psql_count "SELECT count(*) FROM machines;")"
if [[ "${ORPHANS:-0}" -gt 0 && "${MACHINES_NOW:-0}" == "0" ]]; then
    warn "${ORPHANS} orphaned machine_interfaces (no parent machine) may hold OOB leases"
    warn "  → if DHCP later reports exhaustion, force-delete stale records via the admin CLI"
    warn "    or reprovision. Do NOT hand-delete interface/dhcp rows (breaks the singleton)."
fi

# =============================================================================
# Phase 8 — reissue client cert from current CA  (GOTCHA: stale cert)
# =============================================================================
phase "Phase 8 — reissue machine-a-tron client cert"
# Always delete: a cert issued under a previous CA fails mTLS to nico-api with
# "client error (Connect)" on every call. cert-manager reissues from the
# current CA within seconds of the deploy — there is no reason to keep it.
kubectl delete secret "${RELEASE}-certificate" -n "$MAT_NAMESPACE" --ignore-not-found >/dev/null 2>&1
ok "client cert cleared; cert-manager reissues from the current CA on deploy"

# =============================================================================
# Phase 9 — deploy the chart
# =============================================================================
phase "Phase 9 — helm upgrade --install ${RELEASE}"
MERGED_VALUES="$(mktemp)"
# Site-specific overrides ONLY (never committed). Passed as a second -f so Helm
# deep-merges it over the base template (last -f wins per key) — avoids
# unreliable duplicate top-level keys within a single YAML file.
cat > "$MERGED_VALUES" <<EOF
# --- injected by setup-machine-a-tron.sh (site-specific, do not commit) ---
image:
  repository: "${MAT_IMAGE_REPO}"
  tag: "${MAT_IMAGE_TAG}"
EOF
# MAT_MULTIPOD=1: the values file defines its own pods map (mat-0..mat-N with
# per-pod host counts and relay addresses); injecting the single-pod default
# here would deep-merge a phantom extra pod on top of it. HOST_COUNT/DPU env
# still drive the sizing math above -- set them to the fleet TOTALS.
if [[ "${MAT_MULTIPOD:-0}" != "1" ]]; then
cat >> "$MERGED_VALUES" <<EOF
pods:
  default:
    machines:
      dell-hosts:
        hostCount: ${HOST_COUNT}
        dpuPerHostCount: ${DPU_PER_HOST}
        oobDhcpRelayAddress: "${OOB_DHCP_RELAY}"
        adminDhcpRelayAddress: "${ADMIN_DHCP_RELAY}"
EOF
fi
if [[ "$MAT_MODE" == "scale" ]]; then
    # Pin every mock BMC's password to the site root ("emulates a BMC already
    # rotated by an operator"). At scale, the rotation dance is fatally racy:
    # preingestion's initial BMC reset reboots the mock, which comes back at
    # the FACTORY password while its per-MAC Vault entry says "rotated" —
    # 401 → AvoidLockout latches every DPU endpoint forever. With the pin,
    # site-explorer's documented fallback ("expected/factory failed → try the
    # sitewide root without rotation") logs straight in, and resets are
    # harmless because the password never changes.
    cat >> "$MERGED_VALUES" <<EOF
machineATron:
  hostBmcPassword: "${BMC_PASSWORD}"
  dpuBmcPassword: "${BMC_PASSWORD}"
EOF
fi
info "values: image=${MAT_IMAGE_REPO}:${MAT_IMAGE_TAG} hosts=${HOST_COUNT} dpus=${DPU_PER_HOST} oob=${OOB_DHCP_RELAY} admin=${ADMIN_DHCP_RELAY}"
confirm "Deploy ${RELEASE} to ${MAT_NAMESPACE}?" || die "aborted before deploy"
# --qps/--burst-limit: scale mode creates one Service per BMC (hundreds to
# thousands); helm's default burst (100 concurrent API calls) overwhelms
# SOCKS/ssh tunnels to the API server ("connection reset by peer").
helm upgrade --install "$RELEASE" "$CHART_DIR" -n "$MAT_NAMESPACE" --create-namespace \
    --qps "${HELM_QPS:-15}" --burst-limit "${HELM_BURST:-30}" \
    -f "$VALUES_FILE" -f "$MERGED_VALUES"
kubectl rollout status deployment/"$RELEASE" -n "$MAT_NAMESPACE" --timeout=180s \
    || warn "deployment rollout did not complete in time"

# =============================================================================
# Phase 10 — verify end to end
# =============================================================================
phase "Phase 10 — verification"
info "waiting for cert to be issued from the current CA..."
kubectl wait --for=condition=Ready certificate/"${RELEASE}-certificate" -n "$MAT_NAMESPACE" --timeout=120s >/dev/null 2>&1 \
    && ok "client certificate Ready" || warn "certificate not Ready yet — check cert-manager"

# wait windows scale with the deployment size (scale mode: hundreds-thousands)
IFACE_WAIT=$(( 90 + NEED )); (( IFACE_WAIT > 1800 )) && IFACE_WAIT=1800
info "giving machine-a-tron time to register + DHCP (up to ${IFACE_WAIT}s)..."
_end=$((SECONDS+IFACE_WAIT))
IFACES=0
while (( SECONDS < _end )); do
    IFACES="$(psql_count "SELECT count(*) FROM machine_interfaces;")"
    (( IFACES >= NEED )) && break
    sleep 10
done
IPS="$(psql_count "SELECT count(*) FROM machine_interface_addresses;")"
info "machine_interfaces=${IFACES} (need ${NEED})  ips_allocated=${IPS}"
(( IFACES >= NEED )) && ok "BMC interfaces registered + DHCP allocated" \
    || warn "fewer interfaces than expected — check pool sizing / bmc DHCP"

# --- render gate: the fleet we asked for is the fleet we got ---------------
# This script's sizing math is HOST_COUNT * (1 + DPU_PER_HOST), computed from
# the environment and never checked against what Helm actually rendered. That
# blind spot is expensive: if the scale values file keys its machine group
# `compute` while this script appends `machines.dell-hosts`, and Helm merges by
# key, so it ADDED a group instead of overriding one. Every run from Aug 15
# built 9,000 hosts / 27,000 machines while logging "4500 hosts x 2 DPUs ->
# 13500", which doubled address demand, exhausted the /18 pools, forced a /16,
# and — because allocation cost scales with subnet size under a fleet-wide
# lock — cut ingestion throughput roughly fourfold.
#
# Read the rendered config back and fail loudly on any mismatch. A wrong number
# here invalidates every measurement taken afterwards, so it is worth stopping.
_MAT_CM="$(kubectl get cm -n "$MAT_NAMESPACE" -o name 2>/dev/null \
    | grep -- '-config-files' | head -1)"
_MAT_TOML="$(kubectl get "$_MAT_CM" -n "$MAT_NAMESPACE" \
    -o jsonpath='{.data.mat\.toml}' 2>/dev/null || true)"
if [[ -n "$_MAT_TOML" ]]; then
    _GROUPS="$(printf '%s\n' "$_MAT_TOML" | grep -c '^\[machines\.' || true)"
    _HOSTS="$(printf '%s\n' "$_MAT_TOML" | awk -F'= *' '/^host_count/ {s+=$2} END {print s+0}')"
    ok "rendered fleet: ${_GROUPS} machine group(s), ${_HOSTS} hosts total"
    (( _GROUPS == 1 )) \
        || die "rendered mat.toml has ${_GROUPS} [machines.*] groups, expected 1 — a values key mismatch is multiplying the fleet; check that the scale values file keys its group 'dell-hosts'"
    (( _HOSTS == HOST_COUNT )) \
        || die "rendered fleet is ${_HOSTS} hosts but HOST_COUNT=${HOST_COUNT} — every rate measured against this run would be wrong"
else
    warn "could not read rendered mat.toml — fleet size is UNVERIFIED for this run"
fi

# --- expected_machines: required for machine creation (matched by BMC MAC) ---
# machine-a-tron auto-registers them (registerExpectedMachines: true), but on
# nico-api builds without the Machineatron→AddExpectedMachine RBAC grant
# (crates/api-core/src/auth/internal_rbac_rules.rs) the call is 403'd. Fall
# back to direct DB registration mirroring what the API call would create.
info "waiting for expected_machines (auto-registration)..."
_end=$((SECONDS+45)); EXPECTED=0
while (( SECONDS < _end )); do
    EXPECTED="$(psql_count "SELECT count(*) FROM expected_machines;")"
    (( EXPECTED > 0 )) && break
    sleep 5
done
if (( EXPECTED > 0 )); then
    ok "expected_machines=${EXPECTED} (registerExpectedMachines worked — RBAC grant present)"
else
    warn "no expected_machines — this nico-api build lacks the Machineatron"
    warn "  AddExpectedMachine RBAC grant (403). Falling back to direct DB registration."
    # scope strictly to BMC interfaces in the OOB prefix — with an unparsed
    # prefix the filter would match admin-segment interfaces too and register
    # them with the wrong factory password.
    [[ "${OOB_PREFIX:-}" == */* ]] || die "cannot scope the expected_machines fallback: OOB prefix unknown"
    psql_q "INSERT INTO expected_machines (id, serial_number, bmc_mac_address, bmc_username, bmc_password)
        SELECT gen_random_uuid(), 'MAT-' || replace(mi.mac_address::text, ':', ''), mi.mac_address, 'root', '${FACTORY_HOST_BMC_PASSWORD}'
        FROM machine_interfaces mi
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE mia.address << '${OOB_PREFIX}'::inet
          AND NOT EXISTS (SELECT 1 FROM expected_machines em WHERE em.bmc_mac_address = mi.mac_address);" >/dev/null \
        || die "expected_machines DB fallback INSERT failed"
    EXPECTED="$(psql_count "SELECT count(*) FROM expected_machines;")"
    (( EXPECTED > 0 )) || die "expected_machines still 0 after fallback — machine creation cannot proceed"
    ok "expected_machines=${EXPECTED} registered via DB fallback"
fi

# --- kick exploration: clear any AvoidLockout latched before creds existed ---
# Exploration cycles that ran before Phase 4/5 completed record Unauthorized,
# which latches a self-perpetuating AvoidLockout in the exploration report.
# This mirrors the API's clear_last_known_error + request_exploration pair
# (crates/api-db/src/explored_endpoints.rs) — including the
# waiting_for_explorer_refresh flag, which gates preingestion until a fresh
# probe lands (skipping it would let preingestion act on the stale report).
psql_q "UPDATE explored_endpoints
    SET exploration_report = jsonb_set(exploration_report, '{LastExplorationError}', 'null'::jsonb),
        exploration_requested = true,
        waiting_for_explorer_refresh = true;" >/dev/null || true
ok "cleared exploration lockouts + requested re-exploration"

# machine target = one row per host + per DPU; wait scales with host count
MACHINE_TARGET=$(( HOST_COUNT * (1 + DPU_PER_HOST) ))
# 3s/host with a 4h cap: at 4500 hosts (13.5k machines) init alone can run
# ~1h before the first machine appears; the old 90-min cap expired mid-run.
MACHINE_WAIT=$(( 420 + HOST_COUNT * 3 )); (( MACHINE_WAIT > 14400 )) && MACHINE_WAIT=14400
info "waiting for explore → rotate → preingest → identify → create (target ${MACHINE_TARGET} machines, up to ${MACHINE_WAIT}s)..."

# --- per-phase ingestion rate instrumentation (#3756) -----------------------
# One CSV row per sample; counters are cumulative so post-processing derives
# per-phase rates as deltas. `epoch` is wall-clock so rows from different runs
# and the DB's own machines.created timestamps line up exactly. Effective knob
# values are embedded as header comments so every CSV is self-describing.
INGEST_RATE_CSV="${INGEST_RATE_CSV:-/tmp/mat-ingestion-rates-$(date +%Y%m%d-%H%M%S).csv}"
{
    echo "# setup-machine-a-tron ingestion rates  host_count=${HOST_COUNT} dpu_per_host=${DPU_PER_HOST} mode=${MAT_MODE}"
    _KNOBS="$(kubectl get cm nico-api-site-config-files -n "$NICO_SYSTEM_NS" \
        -o jsonpath='{.data.nico-api-site-config\.toml}' 2>/dev/null \
        | grep -vE '^[[:space:]]*#' \
        | grep -E 'run_interval|concurrent_explorations|explorations_per_run|machines_created_per_run|concurrency_limit|max_concurrency|max_concurrent_machine_updates' \
        | sed 's/^[[:space:]]*//' | tr '\n' ';' )"
    echo "# effective_knobs: ${_KNOBS}"
    echo "epoch,dhcp_addresses,endpoints_ok,managed_hosts,machines,machines_ready"
} > "$INGEST_RATE_CSV"
info "per-phase rate samples → ${INGEST_RATE_CSV}"

_end=$((SECONDS+MACHINE_WAIT)); MACHINES=0
while (( SECONDS < _end )); do
    # single round-trip for the progress line AND the CSV sample
    _prog="$(psql_q "SELECT
        (SELECT count(*) FROM machine_interface_addresses) || '/' ||
        (SELECT count(*) FILTER (WHERE exploration_report->'LastExplorationError' = 'null'::jsonb) FROM explored_endpoints) || '/' ||
        (SELECT count(*) FROM explored_managed_hosts) || '/' ||
        (SELECT count(*) FROM machines) || '/' ||
        (SELECT count(*) FROM machines WHERE controller_state->>'state' = 'ready');" || echo '?/?/?/?/?')"
    IFS=/ read -r _dhcp _eok _mh MACHINES _rdy <<< "$_prog"
    MACHINES="${MACHINES:-0}"; [[ "$MACHINES" == "?" ]] && MACHINES=0
    # Only record real samples: a failed query yields '?' placeholders, and
    # writing those as rows corrupts the rate maths downstream (they parse as
    # 0 and look like the fleet went backwards).
    if [[ "$_prog" != *'?'* ]]; then
        echo "$(date +%s),${_dhcp},${_eok},${_mh},${MACHINES},${_rdy}" >> "$INGEST_RATE_CSV"
    else
        warn "  progress query failed; sample skipped (not written to CSV)"
    fi
    (( MACHINES >= MACHINE_TARGET )) && break
    info "  endpoints_ok=${_eok}/${IFACES}  managed_hosts=${_mh}  machines=${MACHINES} ..."
    # Re-clear any AvoidLockout that latched during the wait (e.g. an
    # exploration racing a mock reboot from preingestion's initial BMC reset).
    # Idempotent; scoped to latched endpoints only so successful reports keep
    # their state.
    psql_q "UPDATE explored_endpoints
        SET exploration_report = jsonb_set(exploration_report, '{LastExplorationError}', 'null'::jsonb),
            exploration_requested = true,
            waiting_for_explorer_refresh = true
        WHERE exploration_report->'LastExplorationError'->>'Type' IN ('AvoidLockout','Unauthorized');" >/dev/null || true
    # ...and UNPARK endpoints that have since explored clean: a lingering
    # waiting_for_explorer_refresh gates them out of preingestion
    # (find_preingest_not_waiting) even after a healthy report lands, stalling
    # the pipeline at 'initial' indefinitely.
    psql_q "UPDATE explored_endpoints
        SET waiting_for_explorer_refresh = false, exploration_requested = false
        WHERE waiting_for_explorer_refresh
          AND exploration_report->'LastExplorationError' = 'null'::jsonb;" >/dev/null || true
    sleep 25
done
ENDPOINTS="$(psql_count "SELECT count(*) FROM explored_endpoints;")"
MHOSTS="$(psql_count "SELECT count(*) FROM explored_managed_hosts;")"
# Per-phase rate summary (#3756): for each cumulative counter, the window is
# first-movement → 90%-of-final; avg rate = delta/window. Sampling-based, so
# rates are floors — exact curves come from ingestion-rate-report.sh, which
# reads the DB's own timestamps.
if [[ -s "$INGEST_RATE_CSV" ]]; then
    awk -F, -v OFS=' ' '
        /^#/ || /^epoch/ { next }
        { for (i = 2; i <= 6; i++) { v[i] = $i + 0
            if (v[i] > first_v[i] && first_t[i] == 0 && v[i] > 0) { if (base_seen[i]) { first_t[i] = $1; first_v[i] = v[i] } }
            if (!base_seen[i]) { base_seen[i] = 1; base_v[i] = v[i]; if (v[i] > 0) { first_t[i] = $1; first_v[i] = v[i] } }
            last_t[i] = $1; last_v[i] = v[i] } }
        END {
            split("dhcp_addresses endpoints_ok managed_hosts machines machines_ready", n, " ")
            for (i = 2; i <= 6; i++) {
                dt = last_t[i] - first_t[i]; dv = last_v[i] - first_v[i]
                rate = (dt > 0) ? sprintf("%.1f", dv * 60 / dt) : "n/a"
                printf "  %-16s %6d -> %-6d  %ss window  %s/min\n", n[i-1], first_v[i], last_v[i], dt, rate } }
    ' "$INGEST_RATE_CSV" | while IFS= read -r l; do info "$l"; done
    ok "rate samples: ${INGEST_RATE_CSV}"
fi
echo
if (( MACHINES >= MACHINE_TARGET )); then
    ok "${GREEN}END TO END OK${NC} — endpoints=${ENDPOINTS}, managed_hosts=${MHOSTS}, machines=${MACHINES}/${MACHINE_TARGET}"
elif (( MACHINES > 0 )); then
    ok "${GREEN}MACHINES CREATED${NC} (partial) — ${MACHINES}/${MACHINE_TARGET}; ingestion continuing in the background"
else
    warn "machines not created yet (endpoints=${ENDPOINTS}, managed_hosts=${MHOSTS})"
    warn "  check: kubectl logs -n ${NICO_SYSTEM_NS} deploy/nico-api | grep -i 'site.explor\\|MissingCred\\|Refusing\\|Failed to create'"
    warn "  check: kubectl logs -n ${MAT_NAMESPACE} deploy/${RELEASE} | grep -iE 'No IP addresses|error'"
    warn "  a common cause: admin/OOB pool exhaustion — see the sizing output of Phase 6"
fi

phase "Done"
info "machine-a-tron release ${RELEASE} deployed to ${MAT_NAMESPACE}."
info "Redeploy/iterate: re-run this script (idempotent) after 'export KUBECONFIG=...'."
