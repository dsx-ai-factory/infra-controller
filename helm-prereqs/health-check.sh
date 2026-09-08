#!/usr/bin/env bash
# =============================================================================
# health-check.sh
#
# Validates the health of an NVIDIA Infra Controller Kubernetes deployment.
# Requires: kubectl, curl (inside pods), openssl (inside nico-api pod)
#
# Usage:
#   KUBECONFIG=/path/to/kubeconfig ./health-check.sh
#
# All namespaces are auto-detected from cluster resources. Override via env:
#   NICO_NS, VAULT_NS, POSTGRES_NS, CERT_MANAGER_NS, ESO_NS, METALLB_NS
# =============================================================================
set -uo pipefail

# --------------------------------------------------------------------------
# Output helpers
# --------------------------------------------------------------------------
if [ -t 1 ]; then
  _BOLD=$(tput bold 2>/dev/null || printf '')
  _RESET=$(tput sgr0 2>/dev/null || printf '')
  _GREEN=$(tput setaf 2 2>/dev/null || printf '')
  _RED=$(tput setaf 1 2>/dev/null || printf '')
  _YELLOW=$(tput setaf 3 2>/dev/null || printf '')
  _CYAN=$(tput setaf 6 2>/dev/null || printf '')
  _DIM=$(tput dim 2>/dev/null || printf '')
else
  _BOLD=''; _RESET=''; _GREEN=''; _RED=''; _YELLOW=''; _CYAN=''; _DIM=''
fi

PASS=0; FAIL=0; WARN=0; SKIP=0

pass()    { printf "%s  ✓ PASS%s  %s\n"   "${_GREEN}"  "${_RESET}" "$*"; (( PASS++ )) || true; }
fail()    { printf "%s  ✗ FAIL%s  %s\n"   "${_RED}"    "${_RESET}" "$*"; (( FAIL++ )) || true; }
warn()    { printf "%s  ⚠ WARN%s  %s\n"   "${_YELLOW}" "${_RESET}" "$*"; (( WARN++ )) || true; }
skip()    { printf "%s  − SKIP%s  %s\n"   "${_DIM}"    "${_RESET}" "$*"; (( SKIP++ )) || true; }
section() { printf "\n%s══ %s ══%s\n" "${_BOLD}${_CYAN}" "$*" "${_RESET}"; }

# kubectl wrapper: suppresses errors (tests handle their own messaging)
kc() { kubectl "$@" 2>/dev/null; }

_pod_has_command() {
  local ns="$1" pod="$2" container="$3" tool="$4"
  if [[ -n "${container}" ]]; then
    kubectl exec -n "${ns}" "${pod}" -c "${container}" -- \
      sh -c "command -v ${tool} >/dev/null 2>&1" &>/dev/null
  else
    kubectl exec -n "${ns}" "${pod}" -- \
      sh -c "command -v ${tool} >/dev/null 2>&1" &>/dev/null
  fi
}

# --------------------------------------------------------------------------
# Preflight: kubectl accessible
# --------------------------------------------------------------------------
section "Preflight"
if ! kubectl cluster-info &>/dev/null; then
  printf "\n%s  FATAL%s  kubectl cannot reach the cluster. Set KUBECONFIG and retry.\n\n" "${_RED}${_BOLD}" "${_RESET}"
  exit 1
fi
pass "kubectl: cluster reachable"

# --------------------------------------------------------------------------
# Namespace auto-detection
# --------------------------------------------------------------------------
section "Namespace Detection"

# NICo namespace: find the namespace containing vault-cluster-info
if [[ -z "${NICO_NS:-}" ]]; then
  NICO_NS=$(kubectl get configmap vault-cluster-info -A \
    -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null || true)
  NICO_NS="${NICO_NS:-nico-system}"
fi

# vault namespace: parse from VAULT_SERVICE in vault-cluster-info
# e.g. https://vault.vault.svc.cluster.local:8200 → vault
if [[ -z "${VAULT_NS:-}" ]]; then
  _VAULT_SVC=$(kc get configmap -n "${NICO_NS}" vault-cluster-info \
    -o jsonpath='{.data.VAULT_SERVICE}' || true)
  VAULT_NS=$(printf '%s' "${_VAULT_SVC}" | sed 's|https\?://||' | cut -d: -f1 | cut -d. -f2)
  VAULT_NS="${VAULT_NS:-vault}"
fi
VAULT_ADDR=$(kc get configmap -n "${NICO_NS}" vault-cluster-info \
  -o jsonpath='{.data.VAULT_SERVICE}' 2>/dev/null || \
  printf 'https://vault.%s.svc.cluster.local:8200' "${VAULT_NS}")

# postgres namespace: parse from DB_HOST in database configmap
# e.g. nico-pg-cluster.postgres.svc.cluster.local → postgres
if [[ -z "${POSTGRES_NS:-}" ]]; then
  _DB_HOST=$(kc get configmap -n "${NICO_NS}" nico-system-nico-database-config \
    -o jsonpath='{.data.DB_HOST}' || true)
  POSTGRES_NS=$(printf '%s' "${_DB_HOST}" | cut -d. -f2)
  POSTGRES_NS="${POSTGRES_NS:-postgres}"
fi

# cert-manager, ESO, MetalLB: discover by known deployment names
if [[ -z "${CERT_MANAGER_NS:-}" ]]; then
  CERT_MANAGER_NS=$(kubectl get deployment cert-manager -A \
    -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null || printf 'cert-manager')
fi
if [[ -z "${ESO_NS:-}" ]]; then
  ESO_NS=$(kubectl get deployment external-secrets -A \
    -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null || printf 'external-secrets')
fi
if [[ -z "${METALLB_NS:-}" ]]; then
  METALLB_NS=$(kubectl get deployment metallb-controller -A \
    -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null || printf 'metallb-system')
fi

printf "  %-26s %s\n" "NICo namespace:"     "${NICO_NS}"
printf "  %-26s %s\n" "vault namespace:"       "${VAULT_NS}"
printf "  %-26s %s\n" "vault address:"         "${VAULT_ADDR}"
printf "  %-26s %s\n" "postgres namespace:"    "${POSTGRES_NS}"
printf "  %-26s %s\n" "cert-manager ns:"       "${CERT_MANAGER_NS}"
printf "  %-26s %s\n" "external-secrets ns:"   "${ESO_NS}"
printf "  %-26s %s\n" "metallb ns:"            "${METALLB_NS}"

# --------------------------------------------------------------------------
# Test helpers
# --------------------------------------------------------------------------

_check_deployment() {
  local ns="$1" name="$2"
  local desired ready
  desired=$(kc get deployment -n "${ns}" "${name}" -o jsonpath='{.spec.replicas}')
  ready=$(kc get deployment -n "${ns}" "${name}" -o jsonpath='{.status.readyReplicas}')
  desired="${desired:-0}"; ready="${ready:-0}"
  if [[ "${desired}" -gt 0 && "${ready}" -ge "${desired}" ]]; then
    pass "deployment/${name}: ${ready}/${desired} ready"
  else
    fail "deployment/${name}: ${ready}/${desired} ready"
  fi
}

_check_statefulset() {
  local ns="$1" name="$2"
  local desired ready
  desired=$(kc get statefulset -n "${ns}" "${name}" -o jsonpath='{.spec.replicas}')
  ready=$(kc get statefulset -n "${ns}" "${name}" -o jsonpath='{.status.readyReplicas}')
  desired="${desired:-0}"; ready="${ready:-0}"
  if [[ "${desired}" -gt 0 && "${ready}" -ge "${desired}" ]]; then
    pass "statefulset/${name}: ${ready}/${desired} ready"
  else
    fail "statefulset/${name}: ${ready}/${desired} ready"
  fi
}

_check_job_complete() {
  local ns="$1" name="$2"
  local status
  status=$(kc get job -n "${ns}" "${name}" \
    -o jsonpath='{.status.conditions[?(@.type=="Complete")].status}')
  if [[ "${status}" == "True" ]]; then
    pass "job/${name}: Complete"
  else
    fail "job/${name}: not Complete (status=${status:-unknown})"
  fi
}

_check_secret_exists() {
  local ns="$1" name="$2"
  if kc get secret -n "${ns}" "${name}" &>/dev/null; then
    pass "secret/${name}: exists"
  else
    fail "secret/${name}: not found in ${ns}"
  fi
}

_check_secret_key() {
  local ns="$1" name="$2" key="$3"
  local val
  val=$(kc get secret -n "${ns}" "${name}" -o jsonpath="{.data.${key}}" 2>/dev/null || true)
  if [[ -n "${val}" ]]; then
    pass "secret/${name}[${key}]: present"
  else
    fail "secret/${name}[${key}]: missing or empty"
  fi
}

_check_configmap_key() {
  local ns="$1" name="$2" key="$3"
  local val
  val=$(kc get configmap -n "${ns}" "${name}" -o jsonpath="{.data.${key}}" 2>/dev/null || true)
  if [[ -n "${val}" ]]; then
    pass "configmap/${name}[${key}]: present"
  else
    fail "configmap/${name}[${key}]: missing or empty"
  fi
}

# HTTP check: exec curl from inside a running pod
# _http_check DESC NS LABEL_SELECTOR URL EXPECTED_HTTP_CODE
_http_check() {
  local desc="$1" ns="$2" selector="$3" url="$4" expected="$5"
  local pod code
  pod=$(kc get pod -n "${ns}" -l "${selector}" \
    --field-selector=status.phase=Running \
    -o jsonpath='{.items[0].metadata.name}')
  if [[ -z "${pod}" ]]; then
    fail "${desc}: no running pod found (selector: ${selector})"
    return
  fi
  if ! _pod_has_command "${ns}" "${pod}" "" curl; then
    skip "${desc}: curl not available in pod/${pod}"
    return
  fi
  code=$(kubectl exec -n "${ns}" "${pod}" -- \
    curl -sf -o /dev/null -w '%{http_code}' --max-time 5 "${url}" 2>/dev/null || printf '000')
  if [[ "${code}" == "${expected}" ]]; then
    pass "${desc}: HTTP ${code}"
  elif [[ "${expected}" == "2xx" && "${code}" =~ ^2 ]]; then
    pass "${desc}: HTTP ${code}"
  else
    fail "${desc}: HTTP ${code} (expected ${expected})"
  fi
}

# --------------------------------------------------------------------------
# 1. Supporting infrastructure
# --------------------------------------------------------------------------
section "cert-manager"
_check_deployment "${CERT_MANAGER_NS}" cert-manager
_check_deployment "${CERT_MANAGER_NS}" cert-manager-cainjector
_check_deployment "${CERT_MANAGER_NS}" cert-manager-webhook

section "External Secrets Operator"
_check_deployment "${ESO_NS}" external-secrets
_check_deployment "${ESO_NS}" external-secrets-cert-controller
_check_deployment "${ESO_NS}" external-secrets-webhook

section "MetalLB"
_check_deployment "${METALLB_NS}" metallb-controller
_desired=$(kc get daemonset -n "${METALLB_NS}" metallb-speaker \
  -o jsonpath='{.status.desiredNumberScheduled}' || printf '0')
_ready=$(kc get daemonset -n "${METALLB_NS}" metallb-speaker \
  -o jsonpath='{.status.numberReady}' || printf '0')
_desired="${_desired:-0}"; _ready="${_ready:-0}"
if [[ "${_desired}" -gt 0 && "${_ready}" -ge "${_desired}" ]]; then
  pass "daemonset/metallb-speaker: ${_ready}/${_desired} ready"
else
  fail "daemonset/metallb-speaker: ${_ready:-0}/${_desired} ready"
fi

# --------------------------------------------------------------------------
# 1b. DPF (installed by default; --skip-dpf to opt out)
# NOTE: 'kubectl get deployment <name> -A' is invalid (by-name lookups cannot
# cross namespaces); use a field selector for the namespace autodetect.
# --------------------------------------------------------------------------
if [[ -z "${DPF_NS:-}" ]]; then
  DPF_NS=$(kubectl get deployment -A \
    --field-selector metadata.name=dpf-operator-controller-manager \
    -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null || true)
fi

section "DPF"
if [[ -n "${DPF_NS:-}" ]]; then
  _check_deployment "${DPF_NS}" dpf-operator-controller-manager
  _check_deployment "${DPF_NS}" kamaji
  _check_deployment "${DPF_NS}" maintenance-operator
  _check_deployment "${DPF_NS}" node-feature-discovery-master
  _check_deployment "${DPF_NS}" argo-cd-argocd-repo-server
  _check_statefulset "${DPF_NS}" argo-cd-argocd-application-controller
  _check_secret_exists "${DPF_NS}" hbn-user-password
  # dpf-pull-secret is optional: setup.sh only creates it when an NGC key is
  # set, and the operator pulls the public nvidia/doca images anonymously.
  if kc get secret -n "${DPF_NS}" dpf-pull-secret &>/dev/null; then
    pass "secret/dpf-pull-secret: exists"
  else
    skip "secret/dpf-pull-secret: not set (public images pull anonymously)"
  fi
  _dpf_cluster_phase=$(kc get dpucluster carbide-dpf-cluster -n "${DPF_NS}" \
    -o jsonpath='{.status.phase}' || true)
  if [[ "${_dpf_cluster_phase}" == "Ready" ]]; then
    pass "dpucluster/carbide-dpf-cluster: Ready"
  else
    fail "dpucluster/carbide-dpf-cluster: phase=${_dpf_cluster_phase:-unknown}"
  fi
  # DPFOperatorConfig only reaches Ready once its DPU-side services schedule,
  # which needs actual DPU (BlueField) nodes. On a DPU-less cluster it stays
  # Ready=False by design — warn, don't fail (see docs/manuals/dpf.md).
  _dpf_config_ready=$(kc get dpfoperatorconfig dpfoperatorconfig -n "${DPF_NS}" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' || true)
  _dpf_count=$(kc get dpus.provisioning.dpu.nvidia.com -A \
    -o name 2>/dev/null | wc -l | tr -d '[:space:]')
  if [[ "${_dpf_config_ready}" == "True" ]]; then
    pass "dpfoperatorconfig/dpfoperatorconfig: Ready"
  elif [[ "${_dpf_count:-0}" == "0" ]]; then
    warn "dpfoperatorconfig/dpfoperatorconfig: Ready=${_dpf_config_ready:-unknown} (no DPUs — DPU-side services can't schedule; expected)"
  else
    fail "dpfoperatorconfig/dpfoperatorconfig: Ready=${_dpf_config_ready:-unknown}"
  fi
elif kc get namespace dpf-operator-system &>/dev/null; then
  # The operator deployment is absent but its namespace lingers — a partial or
  # failed DPF install, not a clean opt-out. Fail rather than silently skip.
  fail "dpf-operator-controller-manager: not found, but namespace dpf-operator-system exists (partial/failed DPF install)"
else
  skip "not installed (--skip-dpf was used)"
fi

# --------------------------------------------------------------------------
# 2. Vault
# --------------------------------------------------------------------------
section "Vault"
_check_statefulset "${VAULT_NS}" vault

_vault_json_field() {
  printf '%s' "$1" | grep -o "\"$2\":[^,}]*" | cut -d: -f2 | tr -d ' "'
}

_VAULT_REPLICAS=$(kc get statefulset -n "${VAULT_NS}" vault \
  -o jsonpath='{.spec.replicas}' || printf '0')
_VAULT_REPLICAS="${_VAULT_REPLICAS:-0}"
[[ "${_VAULT_REPLICAS}" =~ ^[0-9]+$ ]] || _VAULT_REPLICAS=0
_VAULT_HA=false

if [[ "${_VAULT_REPLICAS}" -le 0 ]]; then
  fail "vault: no replicas found"
else
  for _IDX in $(seq 0 $(( _VAULT_REPLICAS - 1 ))); do
    _VAULT_POD="vault-${_IDX}"
    _VAULT_JSON=$(kubectl exec -n "${VAULT_NS}" "${_VAULT_POD}" -c vault -- \
      vault status -address="${VAULT_ADDR}" -tls-skip-verify -format=json 2>/dev/null || printf '{}')

    _VAULT_INIT=$(_vault_json_field "${_VAULT_JSON}" initialized)
    _VAULT_SEALED=$(_vault_json_field "${_VAULT_JSON}" sealed)
    _VAULT_POD_HA=$(_vault_json_field "${_VAULT_JSON}" ha_enabled)

    if [[ "${_VAULT_INIT}" == "true" ]]; then
      pass "${_VAULT_POD}: initialized"
    else
      fail "${_VAULT_POD}: not initialized"
    fi
    if [[ "${_VAULT_SEALED}" == "false" ]]; then
      pass "${_VAULT_POD}: unsealed"
    else
      fail "${_VAULT_POD}: sealed — cluster cannot operate"
    fi
    [[ "${_VAULT_POD_HA}" == "true" ]] && _VAULT_HA=true
  done
fi

if [[ "${_VAULT_HA}" == "true" ]]; then
  pass "vault: HA enabled"
else
  warn "vault: HA not enabled"
fi

# vault-pki-config job completion implies PKI+auth are configured correctly
# (detailed checks would require vault token — covered by job + approle secret checks)

# --------------------------------------------------------------------------
# 3. PostgreSQL
# --------------------------------------------------------------------------
section "PostgreSQL"
_check_statefulset "${POSTGRES_NS}" nico-pg-cluster

_PG_OP_READY=$(kc get deployment -n "${POSTGRES_NS}" postgres-operator \
  -o jsonpath='{.status.readyReplicas}' || printf '0')
if [[ "${_PG_OP_READY:-0}" -ge 1 ]]; then
  pass "deployment/postgres-operator: running"
else
  fail "deployment/postgres-operator: not ready"
fi

_DB_NAME=$(kc get configmap -n "${NICO_NS}" nico-system-nico-database-config \
  -o jsonpath='{.data.DB_NAME}' 2>/dev/null || printf 'nico_system_nico')
_DB_EXISTS=$(kubectl exec -n "${POSTGRES_NS}" nico-pg-cluster-0 -c postgres -- \
  psql -U postgres -lqt 2>/dev/null | grep -c "${_DB_NAME}" || printf '0')
if [[ "${_DB_EXISTS}" -ge 1 ]]; then
  pass "postgres: database '${_DB_NAME}' exists"
else
  fail "postgres: database '${_DB_NAME}' not found"
fi

# --------------------------------------------------------------------------
# 4. NICo core pods
# --------------------------------------------------------------------------
section "NICo Pods"
_check_deployment  "${NICO_NS}" nico-api
_check_deployment  "${NICO_NS}" nico-dhcp
_check_statefulset "${NICO_NS}" nico-dns
_check_deployment  "${NICO_NS}" nico-pxe

# Optional pods: warn if the deployment doesn't exist, fail if it exists but isn't ready
for _OPT_DEP in nico-hardware-health nico-ssh-console-rs nico-dsx-exchange-consumer; do
  if kc get deployment -n "${NICO_NS}" "${_OPT_DEP}" &>/dev/null; then
    _check_deployment "${NICO_NS}" "${_OPT_DEP}"
  else
    skip "${_OPT_DEP}: not deployed"
  fi
done

section "NICo Flow"
FLOW_NS="${FLOW_NS:-flow}"
if kc get ns "${FLOW_NS}" &>/dev/null; then
  _check_deployment "${FLOW_NS}" flow
  for _S in flow.nico.nico-pg-cluster.credentials \
            flow-certificate temporal-client-certs nico-roots; do
    _check_secret_exists "${FLOW_NS}" "${_S}"
  done
else
  skip "flow namespace not present — flow disabled or not yet deployed"
fi

section "RMS (Rack Manager Service)"
RMS_NS="${RMS_NS:-rack-manager}"
if kc get ns "${RMS_NS}" &>/dev/null; then
  _check_deployment "${RMS_NS}" rms-api-server
  for _S in rms-api-server-certificate rms.nico.nico-pg-cluster.credentials; do
    _check_secret_exists "${RMS_NS}" "${_S}"
  done
else
  skip "rack-manager namespace not present — RMS not installed (opt-in via --install-rms)"
fi

section "NICo Jobs"
_check_job_complete "${NICO_NS}" vault-pki-config
# Migration job: find by label (name includes a random suffix)
_MIG_JOB=$(kc get jobs -n "${NICO_NS}" -l 'app.kubernetes.io/name=nico-api-migrate' \
  --sort-by=.metadata.creationTimestamp \
  -o jsonpath='{.items[-1:].metadata.name}' 2>/dev/null | awk '{print $NF}' | head -1 || true)
if [[ -z "${_MIG_JOB}" ]]; then
  # fallback: name contains 'migrate'
  _MIG_JOB=$(kc get jobs -n "${NICO_NS}" --no-headers 2>/dev/null | \
    awk '/nico-api-migrate/{print $1}' | tail -1 || true)
fi
if [[ -n "${_MIG_JOB}" ]]; then
  _check_job_complete "${NICO_NS}" "${_MIG_JOB}"
else
  fail "nico-api-migrate: job not found"
fi

# --------------------------------------------------------------------------
# 5. PKI — ClusterIssuers + Certificates
# --------------------------------------------------------------------------
section "ClusterIssuers"
for _ISSUER in selfsigned-bootstrap site-issuer vault-nico-issuer; do
  _READY=$(kc get clusterissuer "${_ISSUER}" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  if [[ "${_READY}" == "True" ]]; then
    pass "clusterissuer/${_ISSUER}: Ready"
  else
    fail "clusterissuer/${_ISSUER}: not Ready (${_READY:-missing})"
  fi
done

section "Certificates"
_NOW_EPOCH=$(date +%s)
while IFS= read -r _CERT; do
  [[ -z "${_CERT}" ]] && continue
  _READY=$(kc get certificate -n "${NICO_NS}" "${_CERT}" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  if [[ "${_READY}" != "True" ]]; then
    fail "certificate/${_CERT}: not Ready"
    continue
  fi
  _EXPIRY=$(kc get certificate -n "${NICO_NS}" "${_CERT}" \
    -o jsonpath='{.status.notAfter}')
  if [[ -n "${_EXPIRY}" ]]; then
    _EXP_EPOCH=$(date -d "${_EXPIRY}" +%s 2>/dev/null || \
                 date -jf "%Y-%m-%dT%H:%M:%SZ" "${_EXPIRY}" +%s 2>/dev/null || printf '0')
    _DAYS=$(( (_EXP_EPOCH - _NOW_EPOCH) / 86400 ))
    if [[ "${_DAYS}" -lt 7 ]]; then
      warn "certificate/${_CERT}: Ready but expires in ${_DAYS} days (${_EXPIRY})"
    else
      pass "certificate/${_CERT}: Ready, expires in ${_DAYS} days"
    fi
  else
    pass "certificate/${_CERT}: Ready"
  fi
done < <(kc get certificates -n "${NICO_NS}" \
  --no-headers -o custom-columns=NAME:.metadata.name 2>/dev/null)

# --------------------------------------------------------------------------
# 6. Required secrets & ConfigMaps
# --------------------------------------------------------------------------
section "Secrets"
_check_secret_exists "${NICO_NS}" nico-vault-token
_check_secret_exists "${NICO_NS}" nico-vault-approle-tokens
_check_secret_key    "${NICO_NS}" nico-vault-approle-tokens VAULT_ROLE_ID
_check_secret_key    "${NICO_NS}" nico-vault-approle-tokens VAULT_SECRET_ID
_check_secret_exists "${NICO_NS}" nico-roots
_check_secret_exists "${NICO_NS}" ssh-host-key
_check_secret_exists "${NICO_NS}" "nico-system.nico.nico-pg-cluster.credentials"
for _CERT_S in nico-api-certificate nico-pxe-certificate \
               nico-dhcp-certificate nico-dns-certificate; do
  _check_secret_exists "${NICO_NS}" "${_CERT_S}"
done

section "ConfigMaps"
_check_configmap_key "${NICO_NS}" vault-cluster-info                   VAULT_SERVICE
_check_configmap_key "${NICO_NS}" vault-cluster-info                   NICO_VAULT_PKI_MOUNT
_check_configmap_key "${NICO_NS}" nico-system-nico-database-config DB_HOST
_check_configmap_key "${NICO_NS}" nico-system-nico-database-config DB_NAME
_check_configmap_key "${NICO_NS}" nico-system-nico-database-config DB_PORT

for _CM in nico-web-api-hostname nico-api-site-config-files \
           nico-api-config-files nico-dhcp-config nico-dns; do
  if kc get configmap -n "${NICO_NS}" "${_CM}" &>/dev/null; then
    pass "configmap/${_CM}: exists"
  else
    fail "configmap/${_CM}: not found"
  fi
done

# --------------------------------------------------------------------------
# 7. External Secrets Operator
# --------------------------------------------------------------------------
section "ClusterSecretStores"
while IFS= read -r _CSS; do
  [[ -z "${_CSS}" ]] && continue
  _READY=$(kc get clustersecretstore "${_CSS}" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  if [[ "${_READY}" == "True" ]]; then
    pass "clustersecretstore/${_CSS}: Valid"
  else
    fail "clustersecretstore/${_CSS}: not Valid (${_READY:-missing})"
  fi
done < <(kc get clustersecretstore --no-headers \
  -o custom-columns=NAME:.metadata.name 2>/dev/null)

section "ExternalSecrets"
while IFS= read -r _LINE; do
  [[ -z "${_LINE}" ]] && continue
  _ES_NAME=$(printf '%s' "${_LINE}" | awk '{print $1}')
  _ES_NS=$(printf '%s' "${_LINE}" | awk '{print $2}')
  _REASON=$(kc get externalsecret -n "${_ES_NS}" "${_ES_NAME}" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].reason}')
  if [[ "${_REASON}" == "SecretSynced" ]]; then
    pass "externalsecret/${_ES_NAME} (${_ES_NS}): SecretSynced"
  else
    fail "externalsecret/${_ES_NAME} (${_ES_NS}): ${_REASON:-not synced}"
  fi
done < <(kc get externalsecret -A --no-headers \
  -o custom-columns=NAME:.metadata.name,NS:.metadata.namespace 2>/dev/null)

# --------------------------------------------------------------------------
# 8. External Services — LoadBalancer VIPs
# --------------------------------------------------------------------------
section "External Service VIPs (LoadBalancer)"
while IFS= read -r _SVC; do
  [[ -z "${_SVC}" ]] && continue
  _IP=$(kc get svc -n "${NICO_NS}" "${_SVC}" \
    -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
  _PORT=$(kc get svc -n "${NICO_NS}" "${_SVC}" \
    -o jsonpath='{.spec.ports[0].port}')
  if [[ -n "${_IP}" && "${_IP}" != "pending" ]]; then
    pass "svc/${_SVC}: ${_IP}:${_PORT}"
  else
    fail "svc/${_SVC}: no external IP (still pending)"
  fi
done < <(kc get svc -n "${NICO_NS}" --no-headers 2>/dev/null | \
  awk '$2=="LoadBalancer"{print $1}')

# --------------------------------------------------------------------------
# 9. External VIP reachability (from MetalLB speaker — hostNetwork)
# --------------------------------------------------------------------------
# MetalLB speaker pods run with hostNetwork:true, giving them the full host
# routing table including BGP-learned routes. nc -zw2 from a speaker pod
# proves the VIP is routable and the service is accepting connections,
# equivalent to a test from an OOB-network host.
# --------------------------------------------------------------------------
section "External VIP Reachability (via MetalLB speaker)"

_SPEAKER=$(kc get pod -n "${METALLB_NS}" \
  -l "app.kubernetes.io/component=speaker,app.kubernetes.io/name=metallb" \
  --no-headers -o custom-columns=NAME:.metadata.name | head -1)

if [[ -z "${_SPEAKER:-}" ]]; then
  warn "MetalLB speaker pod not found — skipping external reachability tests"
elif ! _pod_has_command "${METALLB_NS}" "${_SPEAKER}" speaker nc; then
  skip "MetalLB speaker pod/${_SPEAKER}: nc not available — skipping external reachability tests"
else
  while IFS= read -r _SVC; do
    [[ -z "${_SVC}" ]] && continue
    _IP=$(kc get svc -n "${NICO_NS}" "${_SVC}" \
      -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
    _PORT=$(kc get svc -n "${NICO_NS}" "${_SVC}" \
      -o jsonpath='{.spec.ports[0].port}')
    _PROTO=$(kc get svc -n "${NICO_NS}" "${_SVC}" \
      -o jsonpath='{.spec.ports[0].protocol}')

    # Skip if no IP assigned yet
    [[ -z "${_IP}" || "${_IP}" == "pending" ]] && continue

    # UDP-only services cannot be tested with TCP nc; DNS UDP is covered by the
    # DNS section, NTP has no reliable probe, DHCP requires a full handshake.
    # Services with TCP+UDP on the same VIP: skip the UDP duplicate (name contains "udp").
    [[ "${_PROTO}" == "UDP" ]] && continue
    printf '%s' "${_SVC}" | grep -q "udp" && continue

    if kubectl exec -n "${METALLB_NS}" "${_SPEAKER}" -c speaker -- \
        nc -zw2 "${_IP}" "${_PORT}" &>/dev/null; then
      pass "svc/${_SVC}: ${_IP}:${_PORT} reachable from host network"
    else
      fail "svc/${_SVC}: ${_IP}:${_PORT} not reachable (BGP route missing or service not listening)"
    fi
  done < <(kc get svc -n "${NICO_NS}" --no-headers 2>/dev/null | \
    awk '$2=="LoadBalancer"{print $1}')
fi

# --------------------------------------------------------------------------
# 10. In-cluster service connectivity
# --------------------------------------------------------------------------
section "In-Cluster Service Connectivity"

_http_check "nico-api metrics"       "${NICO_NS}" \
  "app.kubernetes.io/name=nico-api"             "http://localhost:1080/metrics"  "200"
_http_check "nico-pxe metrics"       "${NICO_NS}" \
  "app.kubernetes.io/name=nico-pxe"             "http://localhost:8080/metrics"  "200"
_http_check "nico-dhcp metrics"      "${NICO_NS}" \
  "app.kubernetes.io/name=nico-dhcp"            "http://localhost:1089/metrics"  "200"

if kc get deployment -n "${NICO_NS}" nico-hardware-health &>/dev/null; then
  _http_check "nico-hardware-health" "${NICO_NS}" \
    "app.kubernetes.io/name=nico-hardware-health" "http://localhost:9009/"       "200"
fi

# TLS: verify nico-api gRPC port presents a certificate
_API_POD=$(kc get pod -n "${NICO_NS}" -l "app.kubernetes.io/name=nico-api" \
  --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}')
if [[ -n "${_API_POD:-}" ]]; then
  if ! _pod_has_command "${NICO_NS}" "${_API_POD}" "" openssl; then
    skip "nico-api gRPC TLS (:1079): openssl not available in pod/${_API_POD}"
  else
    _TLS=$(kubectl exec -n "${NICO_NS}" "${_API_POD}" -- \
      sh -c "echo | openssl s_client \
        -connect nico-api.${NICO_NS}.svc.cluster.local:1079 \
        -verify_quiet 2>&1 | head -5" 2>/dev/null || true)
    if printf '%s' "${_TLS}" | grep -qiE "CONNECTED|Protocol version|TLSv"; then
      pass "nico-api gRPC TLS (:1079): TLS handshake succeeded"
    else
      warn "nico-api gRPC TLS (:1079): could not verify (check CA trust)"
    fi
  fi
fi

# --------------------------------------------------------------------------
# 11. DNS — nico-unbound
# --------------------------------------------------------------------------
section "DNS (nico-unbound)"
_UNBOUND_POD=$(kc get pod -n "${NICO_NS}" -l "app.kubernetes.io/name=unbound" \
  --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}' || true)

if [[ -z "${_UNBOUND_POD:-}" ]]; then
  warn "nico-unbound: no running pod found (check unbound.enabled in values)"
else
  # Verify daemon is running via unbound-control (available in the unbound image)
  if ! _pod_has_command "${NICO_NS}" "${_UNBOUND_POD}" "" unbound-control; then
    skip "nico-unbound: unbound-control not available in pod/${_UNBOUND_POD}"
  else
    _UC=$(kubectl exec -n "${NICO_NS}" "${_UNBOUND_POD}" -- \
      unbound-control status 2>/dev/null | grep -c "is running" || printf '0')
    if [[ "${_UC}" -ge 1 ]]; then
      pass "nico-unbound: daemon running"
    else
      fail "nico-unbound: daemon not running (unbound-control status failed)"
    fi
  fi

  # Resolution tests: use MetalLB speaker's nslookup via the external VIP.
  # This tests the full path (VIP → service → DNS resolution) from the host network.
  _UNBOUND_VIP=$(kc get svc -n "${NICO_NS}" nico-unbound-external \
    -o jsonpath='{.status.loadBalancer.ingress[0].ip}' || true)
  if [[ -z "${_UNBOUND_VIP:-}" ]]; then
    _UNBOUND_VIP=$(kc get svc -n "${NICO_NS}" -l "app.kubernetes.io/name=unbound" \
      --no-headers 2>/dev/null | awk '$2=="LoadBalancer" && $4!="<pending>"{print $4; exit}' || true)
  fi

  if [[ -n "${_SPEAKER:-}" && -n "${_UNBOUND_VIP:-}" ]]; then
    if ! _pod_has_command "${METALLB_NS}" "${_SPEAKER}" speaker nslookup; then
      skip "nico-unbound: nslookup not available in MetalLB speaker pod/${_SPEAKER}"
      continue_dns_tests=false
    else
      continue_dns_tests=true
    fi
  else
    continue_dns_tests=false
  fi

  if [[ "${continue_dns_tests}" == "true" ]]; then
    _dns_answer_ips() {
      local hostname="$1"
      kubectl exec -n "${METALLB_NS}" "${_SPEAKER}" -c speaker -- \
        nslookup "${hostname}" "${_UNBOUND_VIP}" 2>/dev/null | \
        awk '/^Name:/ { found=1; next } found && /^Address/ { print $NF }' || true
    }

    _check_dns_name() {
      local hostname="$1"
      local answers
      answers=$(_dns_answer_ips "${hostname}" | paste -sd ',' -)
      if [[ -n "${answers:-}" ]]; then
        pass "nico-unbound: ${hostname} resolves via ${_UNBOUND_VIP} (${answers})"
      else
        fail "nico-unbound: ${hostname} did not resolve via ${_UNBOUND_VIP}"
      fi
    }

    # Recursive resolution
    _EXT=$(_dns_answer_ips "example.com" | head -1)
    if [[ -n "${_EXT:-}" ]]; then
      pass "nico-unbound: recursive resolution (example.com via ${_UNBOUND_VIP})"
    else
      fail "nico-unbound: recursive resolution failed (example.com via ${_UNBOUND_VIP})"
    fi

    # Compatibility .forge records are still consumed by DPU agents, boot
    # artifacts, DHCP, and extension services.
    _check_dns_name "carbide-api.forge"
    _check_dns_name "carbide-pxe.forge"
    _check_dns_name "carbide-static-pxe.forge"
    _check_dns_name "carbide-ntp.forge"
    _check_dns_name "unbound.forge"
    _check_dns_name "otel-receiver.forge"
    _check_dns_name "socks.forge"
  elif [[ -z "${_SPEAKER:-}" || -z "${_UNBOUND_VIP:-}" ]]; then
    warn "nico-unbound: resolution tests skipped (no MetalLB speaker or VIP available)"
  fi
fi

# --------------------------------------------------------------------------
# 12. NTP (nico-ntp)
# --------------------------------------------------------------------------
section "NTP (nico-ntp)"
_NTP_TOTAL=$(kc get pods -n "${NICO_NS}" -l "app.kubernetes.io/name=nico-ntp" \
  --no-headers 2>/dev/null | wc -l | tr -d ' ')
if [[ "${_NTP_TOTAL:-0}" -eq 0 ]]; then
  warn "nico-ntp: no pods found (may be deployed via kustomize overlay — check deploy/envs/<site>/)"
else
  _NTP_READY=$(kc get pods -n "${NICO_NS}" -l "app.kubernetes.io/name=nico-ntp" \
    --no-headers 2>/dev/null | awk '/Running/{n++} END{print n+0}')
  if [[ "${_NTP_READY}" -ge "${_NTP_TOTAL}" ]]; then
    pass "nico-ntp: ${_NTP_READY}/${_NTP_TOTAL} pods Running"
  else
    fail "nico-ntp: ${_NTP_READY}/${_NTP_TOTAL} pods Running"
  fi
fi

# --------------------------------------------------------------------------
# .forge DNS endpoint reference
# --------------------------------------------------------------------------
section ".forge DNS Endpoint Reference"
printf "  %s\n" "The following hostnames must resolve on the OOB management network:"
printf "  %s\n" "These compatibility names remain required until hardcoded agent/container references move to .nico."
printf "  %-36s %-6s %s\n" "Hostname" "Port" "Protocol"
printf "  %-36s %-6s %s\n" "--------" "----" "--------"
printf "  %-36s %-6s %s\n" "carbide-api.forge"        "443"  "gRPC/TLS (DPU agents, CLI, PXE, DHCP)"
printf "  %-36s %-6s %s\n" "carbide-pxe.forge"        "80"   "HTTP     (DPU agents - hardcoded in agent binary)"
printf "  %-36s %-6s %s\n" "carbide-static-pxe.forge" "80"   "HTTP     (host PXE loader - hardcoded in boot images)"
printf "  %-36s %-6s %s\n" "carbide-ntp.forge"        "123"  "NTP/UDP  (DPU agents - hardcoded in agent binary)"
printf "  %-36s %-6s %s\n" "unbound.forge"            "53"   "DNS      (distributed via DHCP option 6)"
printf "  %-36s %-6s %s\n" "otel-receiver.forge"      "443"  "gRPC/TLS (otel-collector sidecars)"
printf "  %-36s %-6s %s\n" "socks.forge"              "1888" "SOCKS5   (DPU extension services - hardcoded in agent binary)"
printf "\n  %s\n" "Verify with: dig +short <hostname> @<UNBOUND_VIP>"

# --------------------------------------------------------------------------
# Summary
# --------------------------------------------------------------------------
_TOTAL=$(( PASS + FAIL + WARN + SKIP ))
printf "\n%s%s════════════════════════════════════════════════%s\n" "${_BOLD}" "${_CYAN}" "${_RESET}"
if [[ "${FAIL}" -eq 0 ]]; then
  printf "%s%s  ALL CHECKS PASSED%s\n" "${_GREEN}" "${_BOLD}" "${_RESET}"
else
  printf "%s%s  %d CHECK(S) FAILED%s\n" "${_RED}" "${_BOLD}" "${FAIL}" "${_RESET}"
fi
printf "%s  ✓ %-3d passed  ✗ %-3d failed  ⚠ %-3d warnings  − %-3d skipped  (%d total)%s\n" \
  "${_BOLD}" "${PASS}" "${FAIL}" "${WARN}" "${SKIP}" "${_TOTAL}" "${_RESET}"
printf "%s%s════════════════════════════════════════════════%s\n\n" "${_BOLD}" "${_CYAN}" "${_RESET}"

[[ "${FAIL}" -eq 0 ]]
