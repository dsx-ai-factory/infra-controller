#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../.." && pwd)"

CORE_NAMESPACE="${LOCAL_DEV_NAMESPACE:-nico-system}"
REST_NAMESPACE="nico-rest"
DSX_GATEWAY_ENABLED="${LOCAL_DEV_DSX_GATEWAY_ENABLED:-0}"
CORE_SITE_CONFIG_MAP="nico-api-site-config-files"
MACHINE_A_TRON_DEPLOYMENT="nico-machine-a-tron-mat-0"
MACHINE_A_TRON_BMC_SERVICE="${MACHINE_A_TRON_DEPLOYMENT}-bmc-mock"
API_FORWARD_PORT="${LOCAL_DEV_REST_API_FORWARD_PORT:-18388}"
KEYCLOAK_FORWARD_PORT="${LOCAL_DEV_KEYCLOAK_FORWARD_PORT:-18082}"
DSX_GATEWAY_FORWARD_PORT="${LOCAL_DEV_DSX_GATEWAY_FORWARD_PORT:-18080}"
WORK_DIR="${LOCAL_DEV_REST_WORK_DIR:-${HOME}/Developer/_agent-tmp/devspace-rest}"
verify_attempts=180
verify_sleep_seconds=5

api_forward_pid=""
keycloak_forward_pid=""
dsx_gateway_forward_pid=""

cleanup() {
  if [[ -n "${api_forward_pid}" ]]; then
    kill "${api_forward_pid}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${keycloak_forward_pid}" ]]; then
    kill "${keycloak_forward_pid}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${dsx_gateway_forward_pid}" ]]; then
    kill "${dsx_gateway_forward_pid}" >/dev/null 2>&1 || true
  fi
}

require_bin() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required binary: %s\n' "$1" >&2
    exit 1
  }
}

trap cleanup EXIT INT TERM

require_bin curl
require_bin jq
require_bin kubectl
require_bin base64

mkdir -p "${WORK_DIR}"

kubectl rollout status deployment/nico-api -n "${CORE_NAMESPACE}" --timeout=300s >/dev/null
kubectl rollout status "deployment/${MACHINE_A_TRON_DEPLOYMENT}" \
  -n "${CORE_NAMESPACE}" --timeout=300s >/dev/null
machine_a_tron_bmc_ip="$(kubectl get service "${MACHINE_A_TRON_BMC_SERVICE}" \
  -n "${CORE_NAMESPACE}" -o jsonpath='{.spec.clusterIP}')"
if [[ -z "${machine_a_tron_bmc_ip}" || "${machine_a_tron_bmc_ip}" == "None" ]]; then
  printf 'machine-a-tron BMC Service has no ClusterIP\n' >&2
  exit 1
fi

# Some ARM64 local clusters resolve this Service name correctly but connections
# made by Core's HTTP client still time out. Put the current literal ClusterIP
# in the local site configuration and restart Core. Updating the ConfigMap keeps
# this setup path within Kubernetes administration instead of bypassing Core's
# authorization rules for SetDynamicConfig. The literal address also works on
# AMD64.
machine_a_tron_bmc_proxy="${machine_a_tron_bmc_ip}:1266"
# Keep the port separate from an IPv6 address in both the config and URLs.
if [[ "${machine_a_tron_bmc_ip}" == *:* ]]; then
  machine_a_tron_bmc_proxy="[${machine_a_tron_bmc_ip}]:1266"
fi
if ! site_config_patch="$(kubectl get configmap "${CORE_SITE_CONFIG_MAP}" \
  -n "${CORE_NAMESPACE}" -o json | jq -c --arg proxy "${machine_a_tron_bmc_proxy}" '
    {data: (
      (.data // {}) |
      with_entries(select(
        .value | type == "string" and
        test("(?m)^[[:space:]]*bmc_proxy[[:space:]]*=")
      )) |
      map_values(gsub(
        "(?m)^[[:space:]]*bmc_proxy[[:space:]]*=[[:space:]]*\"[^\"]*\"";
        "bmc_proxy = \"" + $proxy + "\""
      ))
    )}
  ')"; then
  printf 'Core site ConfigMap does not contain a writable bmc_proxy setting\n' >&2
  exit 1
fi
if ! jq -e --arg proxy "bmc_proxy = \"${machine_a_tron_bmc_proxy}\"" '
  (.data | length) > 0 and all(.data[]; contains($proxy))
' <<<"${site_config_patch}" >/dev/null; then
  printf 'Core site ConfigMap does not contain a bmc_proxy setting\n' >&2
  exit 1
fi
kubectl patch configmap "${CORE_SITE_CONFIG_MAP}" -n "${CORE_NAMESPACE}" \
  --type=merge --patch "${site_config_patch}" >/dev/null
kubectl rollout restart deployment/nico-api -n "${CORE_NAMESPACE}" >/dev/null
kubectl rollout status deployment/nico-api -n "${CORE_NAMESPACE}" --timeout=300s >/dev/null

expected_host_count=0
for ((attempt = 1; attempt <= verify_attempts; attempt++)); do
  if machine_status="$(kubectl exec deployment/nico-api -n "${CORE_NAMESPACE}" -- \
    curl --fail --insecure --silent --max-time 5 \
    "https://${machine_a_tron_bmc_proxy}/machines/status" 2>/dev/null)"; then
    if ! expected_host_count="$(jq -er \
      'if type == "object" and (.machines | type) == "array" then
         .machines | length
       else 0 end' \
      <<<"${machine_status}")"; then
      printf 'machine-a-tron at %s returned invalid machine status JSON\n' \
        "${machine_a_tron_bmc_proxy}" >&2
      exit 1
    fi
  else
    expected_host_count=0
  fi
  if [[ "${expected_host_count}" =~ ^[1-9][0-9]*$ ]]; then
    break
  fi
  if [[ "${attempt}" == "${verify_attempts}" ]]; then
    printf 'machine-a-tron at %s did not report any expected hosts after %s attempts\n' \
      "${machine_a_tron_bmc_proxy}" "${verify_attempts}" >&2
    exit 1
  fi
  sleep "${verify_sleep_seconds}"
done

# Clear any lockout-protection errors cached while Core was still using the
# hostname, then refresh the report. Refresh alone persists an AvoidLockout
# report without resetting the failed pre-ingestion state. Include both host
# and DPU BMC endpoints from machine-a-tron.
mapfile -t machine_a_tron_bmc_endpoints < <(jq -r '
  [.machines[] | .bmc.ip, (.dpus[]?.bmc.ip)] |
  map(select(. != null and . != "")) |
  unique[]
' <<<"${machine_status}")

# Allow explorations that started before the proxy update to finish writing
# their reports. Subsequent cycles read the updated proxy from shared runtime
# state.
sleep 6
for endpoint in "${machine_a_tron_bmc_endpoints[@]}"; do
  if ! kubectl exec deployment/nico-api -n "${CORE_NAMESPACE}" -- \
    /opt/carbide/nico-admin-cli \
    -f json \
    --api-url "https://nico-api.${CORE_NAMESPACE}.svc.cluster.local:1079" \
    site-explorer get-report endpoint "${endpoint}" >/dev/null 2>&1; then
    continue
  fi
  kubectl exec deployment/nico-api -n "${CORE_NAMESPACE}" -- \
    /opt/carbide/nico-admin-cli \
    --api-url "https://nico-api.${CORE_NAMESPACE}.svc.cluster.local:1079" \
    site-explorer clear-error "${endpoint}"
  kubectl exec deployment/nico-api -n "${CORE_NAMESPACE}" -- \
    /opt/carbide/nico-admin-cli \
    --api-url "https://nico-api.${CORE_NAMESPACE}.svc.cluster.local:1079" \
    site-explorer refresh "${endpoint}" >/dev/null
done

kubectl rollout status deployment/nico-rest-api -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
kubectl rollout status deployment/nico-rest-cert-manager -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
kubectl rollout status deployment/nico-rest-cloud-worker -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
kubectl rollout status deployment/nico-rest-site-worker -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
kubectl rollout status deployment/nico-rest-site-manager -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
if [[ "${DSX_GATEWAY_ENABLED}" == "1" ]]; then
  kubectl rollout status deployment/nico-mcp -n "${REST_NAMESPACE}" --timeout=300s >/dev/null
  kubectl wait --for=condition=Programmed gateway/dsx-gateway-csc \
    -n dsx-gateway-csc --timeout=300s >/dev/null
fi
kubectl wait --for=condition=Ready certificate/core-grpc-client-site-agent-certs \
  -n "${REST_NAMESPACE}" --timeout=240s >/dev/null

kubectl port-forward --address 127.0.0.1 -n "${REST_NAMESPACE}" \
  service/nico-rest-api "${API_FORWARD_PORT}:8388" \
  >"${WORK_DIR}/api-port-forward.log" 2>&1 &
api_forward_pid=$!

kubectl port-forward --address 127.0.0.1 -n "${REST_NAMESPACE}" \
  service/keycloak "${KEYCLOAK_FORWARD_PORT}:8082" \
  >"${WORK_DIR}/keycloak-port-forward.log" 2>&1 &
keycloak_forward_pid=$!

if [[ "${DSX_GATEWAY_ENABLED}" == "1" ]]; then
  kubectl port-forward --address 127.0.0.1 -n dsx-gateway-csc \
    service/dsx-gateway-csc "${DSX_GATEWAY_FORWARD_PORT}:80" \
    >"${WORK_DIR}/dsx-gateway-port-forward.log" 2>&1 &
  dsx_gateway_forward_pid=$!
fi

curl --fail --silent --show-error --retry 120 --retry-connrefused --retry-delay 1 \
  --max-time 5 "http://localhost:${KEYCLOAK_FORWARD_PORT}/realms/nico-dev" >/dev/null
curl --fail --silent --show-error --retry 120 --retry-connrefused --retry-delay 1 \
  --max-time 5 "http://localhost:${API_FORWARD_PORT}/healthz" >/dev/null

api_forward_log="$(<"${WORK_DIR}/api-port-forward.log")"
keycloak_forward_log="$(<"${WORK_DIR}/keycloak-port-forward.log")"
if ! kill -0 "${api_forward_pid}" >/dev/null 2>&1 || \
  [[ "${api_forward_log}" != *"Forwarding from 127.0.0.1:${API_FORWARD_PORT}"* ]]; then
  printf 'REST API port-forward failed; port %s may already be in use\n' \
    "${API_FORWARD_PORT}" >&2
  sed -n '1,120p' "${WORK_DIR}/api-port-forward.log" >&2
  exit 1
fi
if ! kill -0 "${keycloak_forward_pid}" >/dev/null 2>&1 || \
  [[ "${keycloak_forward_log}" != *"Forwarding from 127.0.0.1:${KEYCLOAK_FORWARD_PORT}"* ]]; then
  printf 'Keycloak port-forward failed; port %s may already be in use\n' \
    "${KEYCLOAK_FORWARD_PORT}" >&2
  sed -n '1,120p' "${WORK_DIR}/keycloak-port-forward.log" >&2
  exit 1
fi
if [[ "${DSX_GATEWAY_ENABLED}" == "1" ]]; then
  dsx_gateway_forward_log="$(<"${WORK_DIR}/dsx-gateway-port-forward.log")"
  if ! kill -0 "${dsx_gateway_forward_pid}" >/dev/null 2>&1 || \
    [[ "${dsx_gateway_forward_log}" != *"Forwarding from 127.0.0.1:${DSX_GATEWAY_FORWARD_PORT}"* ]]; then
    printf 'DSX Agent Gateway port-forward failed; port %s may already be in use\n' \
      "${DSX_GATEWAY_FORWARD_PORT}" >&2
    sed -n '1,120p' "${WORK_DIR}/dsx-gateway-port-forward.log" >&2
    exit 1
  fi
fi

setup_started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

NAMESPACE="${REST_NAMESPACE}" \
API_URL="http://localhost:${API_FORWARD_PORT}" \
KEYCLOAK_URL="http://localhost:${KEYCLOAK_FORWARD_PORT}" \
RESTART_API=0 \
  "${REPO_ROOT}/rest-api/scripts/setup-local.sh" site-agent

kubectl rollout status statefulset/nico-rest-site-agent \
  -n "${REST_NAMESPACE}" --timeout=300s >/dev/null

site_agent_logs=""
core_connected=0
for attempt in {1..60}; do
  site_agent_logs="$(kubectl logs statefulset/nico-rest-site-agent \
    -n "${REST_NAMESPACE}" --since-time="${setup_started_at}" 2>/dev/null || true)"
  case "${site_agent_logs}" in
    *"CoreGrpcClient: Successfully connected to server"*)
      core_connected=1
      break
      ;;
  esac
  sleep 5
done
if [[ "${core_connected}" != "1" ]]; then
  printf 'site-agent did not establish a Core gRPC connection\n' >&2
  printf '%s\n' "${site_agent_logs}" >&2
  exit 1
fi

site_id="$(kubectl get configmap nico-rest-site-agent-config -n "${REST_NAMESPACE}" \
  -o jsonpath='{.data.CLUSTER_ID}')"
secret_site_id="$(kubectl get secret site-registration -n "${REST_NAMESPACE}" \
  -o jsonpath='{.data.site-uuid}' 2>/dev/null | base64 -d 2>/dev/null || true)"
if [[ -z "${site_id}" || "${site_id}" == "00000000-0000-4000-8000-000000000001" || \
  "${secret_site_id}" != "${site_id}" ]]; then
  printf 'site-agent ConfigMap and registration Secret do not contain the same registered site ID\n' >&2
  exit 1
fi

inventory_started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Discovery may still be adding machines after the first positive status
# response used for endpoint recovery. Take a fresh snapshot so verification
# expects the complete inventory rather than that earlier partial count.
if ! machine_status="$(kubectl exec deployment/nico-api -n "${CORE_NAMESPACE}" -- \
  curl --fail --insecure --silent --max-time 5 \
  "https://${machine_a_tron_bmc_proxy}/machines/status" 2>/dev/null)"; then
  printf 'machine-a-tron at %s did not return machine status before REST verification\n' \
    "${machine_a_tron_bmc_proxy}" >&2
  exit 1
fi
if ! expected_host_count="$(jq -er \
  'if type == "object" and (.machines | type) == "array" then
     .machines | length
   else 0 end' \
  <<<"${machine_status}")"; then
  printf 'machine-a-tron at %s returned invalid machine status JSON\n' \
    "${machine_a_tron_bmc_proxy}" >&2
  exit 1
fi
if [[ ! "${expected_host_count}" =~ ^[1-9][0-9]*$ ]]; then
  printf 'machine-a-tron at %s did not report a valid expected host count before REST verification\n' \
    "${machine_a_tron_bmc_proxy}" >&2
  exit 1
fi

site_ready=false
machines_ready=false
machine_count=0
fresh_cycle=false
# Site registration and the first four-minute inventory cycle can take around ten
# minutes on a clean cluster. Allow fifteen minutes while retaining the full check.
for ((attempt = 1; attempt <= verify_attempts; attempt++)); do
  site_ready=false
  machines_ready=false
  machine_count=0
  token="$(curl --fail --silent --max-time 5 -X POST \
    "http://localhost:${KEYCLOAK_FORWARD_PORT}/realms/nico-dev/protocol/openid-connect/token" \
    -H "Content-Type: application/x-www-form-urlencoded" \
    -d "client_id=nico-api" \
    -d "client_secret=nico-local-secret" \
    -d "grant_type=password" \
    -d "username=admin@example.com" \
    -d "password=adminpassword" 2>/dev/null | jq -er '.access_token' 2>/dev/null || true)"
  if [[ -n "${token}" ]]; then
    site="$(curl --fail --silent --max-time 5 \
      "http://localhost:${API_FORWARD_PORT}/v2/org/test-org/nico/site/${site_id}" \
      -H "Authorization: Bearer ${token}" 2>/dev/null || true)"
    machines="$(curl --fail --silent --max-time 5 \
      "http://localhost:${API_FORWARD_PORT}/v2/org/test-org/nico/machine?siteId=${site_id}&isMissingOnSite=false&pageSize=100" \
      -H "Authorization: Bearer ${token}" 2>/dev/null || true)"
    site_ready="$(jq -r --arg site_id "${site_id}" \
      '.id == $site_id and .isOnline == true and .status == "Registered"' \
      <<<"${site}" 2>/dev/null || printf 'false')"
    machines_ready="$(jq -r --arg site_id "${site_id}" \
      --argjson expected_host_count "${expected_host_count}" \
      'type == "array" and length == $expected_host_count and
       all(.[]; .siteId == $site_id)' \
      <<<"${machines}" 2>/dev/null || printf 'false')"
    machine_count="$(jq -r 'if type == "array" then length else 0 end' \
      <<<"${machines}" 2>/dev/null || printf '0')"
  fi

  site_worker_logs="$(kubectl logs deployment/nico-rest-site-worker \
    -n "${REST_NAMESPACE}" --since-time="${inventory_started_at}" 2>/dev/null || true)"
  fresh_cycle="$(jq -Rrs --arg site_id "${site_id}" '
    [splits("\n") | fromjson?
     | select(.Activity == "UpdateMachinesInDB" and .["Site ID"] == $site_id)
     | .msg] |
    reduce .[] as $msg (
      {saw_last_nonempty_page: false, complete: false};
      if ($msg | startswith("Received Machine inventory page:")) then
        ($msg | capture("^Received Machine inventory page: (?<current>[0-9]+) of (?<pages>[0-9]+), page size: [0-9]+, total count: (?<items>[0-9]+)$")) as $page |
        .saw_last_nonempty_page = (
          ($page.current | tonumber) == ($page.pages | tonumber) and
          ($page.items | tonumber) > 0
        )
      elif .saw_last_nonempty_page and $msg == "completed activity" then
        .complete = true
      else . end
    ) | .complete
  ' <<<"${site_worker_logs}" 2>/dev/null || printf 'false')"

  if [[ "${site_ready}" == "true" && "${machines_ready}" == "true" && \
    "${fresh_cycle}" == "true" ]]; then
    printf 'REST API reports site %s online with all %s hosts synced from a current inventory\n' \
      "${site_id}" "${expected_host_count}"
    break
  fi
  if [[ "${attempt}" == "${verify_attempts}" ]]; then
    printf 'REST integration verification failed: site_ready=%s machines_ready=%s rest_machines=%s expected_hosts=%s fresh_cycle=%s\n' \
      "${site_ready}" "${machines_ready}" "${machine_count}" \
      "${expected_host_count}" "${fresh_cycle}" >&2
    exit 1
  fi
  sleep "${verify_sleep_seconds}"
done

if [[ "${DSX_GATEWAY_ENABLED}" == "1" ]]; then
  mcp_headers=(
    -H "Authorization: Bearer ${token}"
    -H "Accept: application/json, text/event-stream"
    -H "Content-Type: application/json"
  )
  curl --fail --silent --show-error --max-time 30 \
    -D "${WORK_DIR}/dsx-gateway-initialize.headers" \
    -o "${WORK_DIR}/dsx-gateway-initialize.body" \
    "${mcp_headers[@]}" \
    --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"nico-devspace","version":"1"}}}' \
    "http://localhost:${DSX_GATEWAY_FORWARD_PORT}/mcp"

  mcp_session_id="$(awk 'tolower($0) ~ /^mcp-session-id:/ {
    sub(/^[^:]*:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit
  }' "${WORK_DIR}/dsx-gateway-initialize.headers")"
  session_header=()
  if [[ -n "${mcp_session_id}" ]]; then
    session_header=(-H "Mcp-Session-Id: ${mcp_session_id}")
  fi

  curl --fail --silent --show-error --max-time 30 \
    -o /dev/null \
    "${mcp_headers[@]}" "${session_header[@]}" \
    --data '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    "http://localhost:${DSX_GATEWAY_FORWARD_PORT}/mcp"
  curl --fail --silent --show-error --max-time 60 \
    -o "${WORK_DIR}/dsx-gateway-tools-list.body" \
    "${mcp_headers[@]}" "${session_header[@]}" \
    --data '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
    "http://localhost:${DSX_GATEWAY_FORWARD_PORT}/mcp"

  tools_list_json="$(jq -c . "${WORK_DIR}/dsx-gateway-tools-list.body" 2>/dev/null || \
    sed -n 's/^data: //p' "${WORK_DIR}/dsx-gateway-tools-list.body" | head -1)"
  if ! jq -e '
    .result.tools as $tools |
    ($tools | type == "array" and length > 0) and
    all($tools[]; (.name | type == "string" and startswith("nico_")) and
      .inputSchema.properties.shard_id == null)
  ' <<<"${tools_list_json}" >/dev/null; then
    printf 'DSX Agent Gateway did not return direct NICo MCP tools without shard routing\n' >&2
    printf '%s\n' "${tools_list_json}" >&2
    exit 1
  fi
  printf 'CSC Agent Gateway authenticated and listed direct NICo MCP tools without shard routing\n'
fi

printf 'REST integration setup complete\n'
