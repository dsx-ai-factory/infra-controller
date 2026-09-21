#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf -- "${TEST_DIR}"' EXIT
DEVSPACE_TEST_SLEEP="$(command -v sleep)"
export DEVSPACE_TEST_SLEEP

# Run an unchanged copy so the script finds a harmless `setup-local.sh`.
mkdir -p "${TEST_DIR}/dev/deployment/devspace" "${TEST_DIR}/rest-api/scripts"
cp "${SCRIPT_DIR}/setup-rest-integration.sh" "${TEST_DIR}/dev/deployment/devspace/"
printf '#!/usr/bin/env bash\nexit 0\n' > "${TEST_DIR}/rest-api/scripts/setup-local.sh"
chmod +x "${TEST_DIR}/rest-api/scripts/setup-local.sh"
cat > "${TEST_DIR}/dev/deployment/devspace/core-admin.sh" <<'ADMIN'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  '-f json machine show')
    printf 'poll\n' >> "${DEVSPACE_TEST_CASE_DIR}/core-polls"
    state=Ready
    if [[ "$(wc -l < "${DEVSPACE_TEST_CASE_DIR}/core-polls")" == 1 ]]; then
      state=HostInitializing/WaitingForDiscovery
    elif [[ "${DEVSPACE_TEST_ASSIGNED:-0}" == 1 ]]; then
      state=Assigned/Ready
    fi
    printf '{"machines":[{"id":"host","machine_type":2,"state":"%s"}]}\n' "$state"
    ;;
  'scout-stream ping host')
    printf 'ping\n' >> "${DEVSPACE_TEST_CASE_DIR}/scout-pings"
    # A missing or unresponsive stream must be retried before declaring ready.
    [[ "$(wc -l < "${DEVSPACE_TEST_CASE_DIR}/scout-pings")" != 1 ]]
    ;;
  *) exit 1 ;;
esac
ADMIN

kubectl() {
  case "$*" in
    'rollout '*) ;;
    'wait --for=condition=Ready certificate/core-grpc-client-site-agent-certs '*) ;;
    'get service nico-machine-a-tron-mat-0-bmc-mock '*)
      printf '%s' "${DEVSPACE_TEST_CLUSTER_IP}"
      ;;
    'get configmap nico-api-site-config-files '*)
      printf '%s\n' '{"data":{"config.toml":"[site_explorer]\nbmc_proxy = \"bmc-mock:1266\"\n"}}'
      ;;
    'patch configmap nico-api-site-config-files '*)
      printf '%s\n' "${@: -1}" > "${DEVSPACE_TEST_CASE_DIR}/patch.json"
      ;;
    'exec deployment/nico-api '*' -- curl '*)
      # Reach both status requests, then stop before unrelated inventory polling.
      if [[ -s "${DEVSPACE_TEST_CASE_DIR}/status-urls" && "${DEVSPACE_TEST_VERIFY:-0}" != 1 ]]; then
        printf '%s\n' '{"machines":[]}'
      else
        # Leave out BMC addresses so setup skips unrelated endpoint recovery.
        printf '%s\n' '{"machines":[{}]}'
      fi
      printf '%s\n' "${@: -1}" >> "${DEVSPACE_TEST_CASE_DIR}/status-urls"
      ;;
    'port-forward '*)
      printf '%s\n' "${BASHPID}" >> "${DEVSPACE_TEST_CASE_DIR}/forward-pids"
      printf 'Forwarding from 127.0.0.1:%s\n' "${@: -1}"
      exec "${DEVSPACE_TEST_SLEEP}" 30
      ;;
    'logs statefulset/nico-rest-site-agent '*)
      printf 'CoreGrpcClient: Successfully connected to server\n'
      ;;
    'logs deployment/nico-rest-site-worker '*)
      printf '%s\n' \
        '{"Activity":"UpdateMachinesInDB","Site ID":"00000000-0000-4000-8000-000000000002","msg":"Received Machine inventory page: 1 of 1, page size: 1, total count: 1"}' \
        '{"Activity":"UpdateMachinesInDB","Site ID":"00000000-0000-4000-8000-000000000002","msg":"completed activity"}'
      ;;
    'get configmap nico-rest-site-agent-config '*)
      printf '00000000-0000-4000-8000-000000000002'
      ;;
    'get secret site-registration '*)
      printf '00000000-0000-4000-8000-000000000002' | base64
      ;;
    *)
      printf 'unexpected kubectl call: %s\n' "$*" >&2
      return 1
      ;;
  esac
}

curl() {
  # The script checks both forward logs after its readiness requests.
  for ((attempt = 0; attempt < 500; attempt++)); do
    if [[ -s "${LOCAL_DEV_REST_WORK_DIR}/api-port-forward.log" &&
          -s "${LOCAL_DEV_REST_WORK_DIR}/keycloak-port-forward.log" ]]; then
      if [[ -n "${DEVSPACE_TEST_SIGNAL:-}" ]]; then
        builtin kill -"${DEVSPACE_TEST_SIGNAL}" "${BASHPID}"
        printf 'continued after signal\n' >&2
        return 1
      fi
      case "$*" in
        *protocol/openid-connect/token*) printf '{"access_token":"fixture"}\n' ;;
        *'/nico/site/'*)
          printf '{"id":"00000000-0000-4000-8000-000000000002","isOnline":true,"status":"Registered"}\n'
          ;;
        *'/nico/machine?'*)
          usable=false
          [[ ! -f "${DEVSPACE_TEST_CASE_DIR}/machine-polled" ]] || usable=true
          touch "${DEVSPACE_TEST_CASE_DIR}/machine-polled"
          printf '[{"controllerMachineId":"host","siteId":"00000000-0000-4000-8000-000000000002","isUsableByTenant":%s}]\n' "${usable}"
          ;;
      esac
      return 0
    fi
    "${DEVSPACE_TEST_SLEEP}" 0.01
  done
  printf 'fake port-forwards did not start\n' >&2
  return 1
}

sleep() { :; }

# Join the fake forwards when the script's cleanup stops them.
kill() {
  if [[ "$#" == 1 ]]; then
    builtin kill "$@" 2>/dev/null || true
    wait "$1" 2>/dev/null || true
  else
    builtin kill "$@"
  fi
}
export -f kubectl curl sleep kill

export LOCAL_DEV_NAMESPACE=nico-system
export LOCAL_DEV_DSX_GATEWAY_ENABLED=0
export LOCAL_DEV_REST_API_FORWARD_PORT=18388
export LOCAL_DEV_KEYCLOAK_FORWARD_PORT=18082

while read -r scenario cluster_ip expected_proxy; do
  DEVSPACE_TEST_CASE_DIR="${TEST_DIR}/${scenario}"
  DEVSPACE_TEST_CLUSTER_IP="${cluster_ip}"
  LOCAL_DEV_REST_WORK_DIR="${DEVSPACE_TEST_CASE_DIR}/work"
  export DEVSPACE_TEST_CASE_DIR DEVSPACE_TEST_CLUSTER_IP LOCAL_DEV_REST_WORK_DIR
  mkdir -p "${DEVSPACE_TEST_CASE_DIR}"

  script_status=0
  bash "${TEST_DIR}/dev/deployment/devspace/setup-rest-integration.sh" \
    > "${DEVSPACE_TEST_CASE_DIR}/output" 2>&1 || script_status=$?
  if [[ "${script_status}" != 1 ]] || ! grep -Fq \
    "machine-a-tron at ${expected_proxy} did not report a valid expected host count before REST verification" \
    "${DEVSPACE_TEST_CASE_DIR}/output"; then
    printf '%s: setup did not reach the expected host count guard\n' "${scenario}" >&2
    cat "${DEVSPACE_TEST_CASE_DIR}/output" >&2
    exit 1
  fi
  if ! jq -e --arg proxy "${expected_proxy}" \
    '.data["config.toml"] == ("[site_explorer]\nbmc_proxy = \"" + $proxy + "\"\n")' \
    "${DEVSPACE_TEST_CASE_DIR}/patch.json" > /dev/null; then
    printf '%s: ConfigMap patch contains the wrong BMC proxy\n' "${scenario}" >&2
    exit 1
  fi
  mapfile -t status_urls < "${DEVSPACE_TEST_CASE_DIR}/status-urls"
  expected_url="https://${expected_proxy}/machines/status"
  if [[ "${#status_urls[@]}" != 2 || "${status_urls[0]}" != "${expected_url}" ||
        "${status_urls[1]}" != "${expected_url}" ]]; then
    printf '%s: expected both status requests to use %s, got %s\n' \
      "${scenario}" "${expected_url}" "${status_urls[*]}" >&2
    exit 1
  fi
  printf '%s BMC Service address passed\n' "${scenario}"
done <<'CASES'
IPv4 192.0.2.5 192.0.2.5:1266
IPv6 2001:db8::5 [2001:db8::5]:1266
CASES

for signal in INT TERM; do
  DEVSPACE_TEST_CASE_DIR="${TEST_DIR}/${signal}"
  DEVSPACE_TEST_SIGNAL="${signal}"
  LOCAL_DEV_REST_WORK_DIR="${DEVSPACE_TEST_CASE_DIR}/work"
  export DEVSPACE_TEST_CASE_DIR DEVSPACE_TEST_SIGNAL LOCAL_DEV_REST_WORK_DIR
  mkdir -p "${DEVSPACE_TEST_CASE_DIR}"
  script_status=0
  bash "${TEST_DIR}/dev/deployment/devspace/setup-rest-integration.sh" \
    > "${DEVSPACE_TEST_CASE_DIR}/output" 2>&1 || script_status=$?
  expected_status=130
  [[ "${signal}" != TERM ]] || expected_status=143
  if [[ "${script_status}" != "${expected_status}" ]] || \
    grep -q 'continued after signal' "${DEVSPACE_TEST_CASE_DIR}/output"; then
    printf '%s: setup did not exit immediately with status %s\n' "${signal}" "${expected_status}" >&2
    cat "${DEVSPACE_TEST_CASE_DIR}/output" >&2
    exit 1
  fi
  while read -r pid; do
    if builtin kill -0 "${pid}" 2>/dev/null; then
      printf '%s: port-forward %s survived cleanup\n' "${signal}" "${pid}" >&2
      exit 1
    fi
  done < "${DEVSPACE_TEST_CASE_DIR}/forward-pids"
  printf '%s exits and cleans up port-forwards\n' "${signal}"
done

unset DEVSPACE_TEST_SIGNAL
DEVSPACE_TEST_VERIFY=1
DEVSPACE_TEST_CASE_DIR="${TEST_DIR}/readiness"
LOCAL_DEV_REST_WORK_DIR="${DEVSPACE_TEST_CASE_DIR}/work"
export DEVSPACE_TEST_VERIFY DEVSPACE_TEST_CASE_DIR LOCAL_DEV_REST_WORK_DIR
mkdir -p "${DEVSPACE_TEST_CASE_DIR}"
bash "${TEST_DIR}/dev/deployment/devspace/setup-rest-integration.sh" \
  > "${DEVSPACE_TEST_CASE_DIR}/output" 2>&1
grep -q 'machines_ready=false' "${DEVSPACE_TEST_CASE_DIR}/output"
grep -q 'all 1 hosts ready in Core, usable in REST, and synced' "${DEVSPACE_TEST_CASE_DIR}/output"
[[ "$(wc -l < "${DEVSPACE_TEST_CASE_DIR}/core-polls")" == 3 ]]
[[ "$(wc -l < "${DEVSPACE_TEST_CASE_DIR}/scout-pings")" == 2 ]]
printf 'Readiness waits for REST usability, Core Ready, and a responding Scout stream\n'

DEVSPACE_TEST_ASSIGNED=1
DEVSPACE_TEST_CASE_DIR="${TEST_DIR}/assigned-readiness"
LOCAL_DEV_REST_WORK_DIR="${DEVSPACE_TEST_CASE_DIR}/work"
export DEVSPACE_TEST_ASSIGNED DEVSPACE_TEST_CASE_DIR LOCAL_DEV_REST_WORK_DIR
mkdir -p "${DEVSPACE_TEST_CASE_DIR}"
bash "${TEST_DIR}/dev/deployment/devspace/setup-rest-integration.sh" \
  > "${DEVSPACE_TEST_CASE_DIR}/output" 2>&1
grep -q 'all 1 hosts ready in Core, usable in REST, and synced' "${DEVSPACE_TEST_CASE_DIR}/output"
[[ ! -e "${DEVSPACE_TEST_CASE_DIR}/scout-pings" ]]
printf 'Assigned/Ready hosts do not require Scout while running a tenant OS\n'
