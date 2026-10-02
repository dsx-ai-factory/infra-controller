#!/usr/bin/env bash
#
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
#
# Called from `include/Makefile-bootstrap.toml`

# Stop on first failure
set -eo pipefail

# This script can be used to simulate discovering a DPU in the docker-compose setup
# It will use a hardcoded MAC address "00:11:22:33:44:55" (see `dpu_dhcp_discovery.json`) in
# the respective environment directory to perform a DHCP request, and then submits Machine details.
# If you need more than one DPU, you can edit the MAC address in the file and call
# `discover_dpu.sh` again

MAX_RETRY=10
if [ $# -ne 1 ]; then
  echo
  echo "Must provide data directory as positional argument"
  echo
  echo "    $0" '<data_dir>'
  echo
  exit 1
fi

# Kubernetes local env uses TLS, docker-compose doesn't
if [ "$FORGE_BOOTSTRAP_KIND" == "kube" ]; then
  export CERT_PATH=${CERT_PATH:=/tmp/localdev-certs}
  if [[ ! -e ${CERT_PATH}/tls.crt ]]; then
    echo "pulling certs from pod"
    mkdir -p ${CERT_PATH}
    kubectl -n forge-system exec deploy/carbide-api -- tar cf - -C /var/run/secrets/spiffe.io/..data . | tar xf - -C ${CERT_PATH}
  fi
  export GRPCURL="grpcurl --key ${CERT_PATH}/tls.key --cacert ${CERT_PATH}/ca.crt --cert ${CERT_PATH}/tls.crt"
else
  export DISABLE_TLS_ENFORCEMENT=true
  export NO_DPU_CONTAINERS=true
  export GRPCURL="grpcurl -insecure"
fi

DATA_DIR=$1
source $DATA_DIR/envrc
source "$(dirname "${BASH_SOURCE[0]}")/host_port.sh"
API_SERVER=$(host_port "$API_SERVER_HOST" "$API_SERVER_PORT")
PXE_SERVER=$(host_port "$PXE_SERVER_HOST" "$PXE_SERVER_PORT")

DPU_CONFIG_FILE="/tmp/forge-dpu-agent-sim-config.toml"
BMC_METADATA_FILE=${DATA_DIR}/update_dpu_bmc_metadata.json

simulate_boot() {
  # Simulate the DHCP request of a DPU
  RESULT=$(${GRPCURL} -d @ "${API_SERVER}" forge.Forge/DiscoverDhcp <"${DATA_DIR}/dpu_dhcp_discovery.json")
  MACHINE_INTERFACE_ID=$(echo $RESULT | jq ".machineInterfaceId.value" | tr -d '"')
  echo "Created Machine Interface with ID $MACHINE_INTERFACE_ID"

  REAL_IP=$(${REPO_ROOT}/dev/bin/psql.sh "select address from machine_interface_addresses where interface_id='${MACHINE_INTERFACE_ID}';" | tr -d '[:space:]"')
  echo "Machines real IP: ${REAL_IP}"

  echo "Sending pxe boot request"
  curl -H "X-Forwarded-For: ${REAL_IP}" "http://${PXE_SERVER}/api/v0/pxe/boot?uuid=${MACHINE_INTERFACE_ID}&buildarch=arm64"

  echo "Sending cloud-init request"
  curl -H "X-Forwarded-For: ${REAL_IP}" "http://${PXE_SERVER}/api/v0/cloud-init/dpu/user-data"

  echo "Sending DiscoverMachine"
  # Simulate the Machine discovery request of a DPU
  RESULT=$(cat "${DATA_DIR}/dpu_machine_discovery.json" | ${GRPCURL} -H "X-Forwarded-For: ${REAL_IP}" -d @ "${API_SERVER}" forge.Forge/DiscoverMachine)
  DPU_MACHINE_ID=$(echo $RESULT | jq ".machineId.id" | tr -d '"')
  echo "DPU_MACHINE_ID: ${DPU_MACHINE_ID}"

  echo "Updating BMC Metadata"
  UPDATE_BMC_METADATA=$(jq --arg machine_id "$DPU_MACHINE_ID" '.machine_id.id = $machine_id' "$BMC_METADATA_FILE")
  ${GRPCURL} -d "$UPDATE_BMC_METADATA" "${API_SERVER}" forge.Forge/UpdateBMCMetaData

  # Mark discovery complete
  echo "Sending DiscoveryComplete"
  RESULT=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/DiscoveryCompleted)
  echo "DPU discovery completed. Waiting for it reached in Host/WaitingForDiscovery state."

  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
  echo "Created DPU Machine with ID $DPU_MACHINE_ID (state: ${MACHINE_STATE})"

  ACTION=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/ForgeAgentControl | jq -r .action)
  echo "Forge Agent Control Result: $ACTION (state: ${MACHINE_STATE})"

  if [[ "${ACTION}" == "DISCOVERY" ]]; then
    echo "Performing discovery"
    # Simulate credential settings of a DPU
    RESULT=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"credentials\": [{\"user\": \"forge\", \"password\": \"notforprod\", \"credential_purpose\": 1}] }" "${API_SERVER}" forge.Forge/UpdateMachineCredentials)
    cred_ret=$?
    if [ $cred_ret -eq 0 ]; then
      echo "Created 'forge' DPU SSH account"
    else
      echo "Failed to create DPU SSH account"
      exit $cred_ret
    fi

    # Mark discovery complete
    RESULT=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/DiscoveryCompleted)
    echo "DPU discovery completed: ${RESULT}"
  fi

  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
  echo "Machine State: ${MACHINE_STATE}"
}

echo "simulating first boot"
simulate_boot

MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
while [[ $MACHINE_STATE != "DPUInitializing/WaitingForNetworkInstall" ]]; do
  if [[ $MACHINE_STATE == "DPUInitializing/WaitingForNetworkConfig" ]]; then
    echo "DPU/WaitingForNetworkInstall skipped"
    FIRMWARE_UPDATE_SKIPPED=1
    break
  fi
  echo "Waiting for DPU state DPU/WaitingForNetworkInstall. Current: $MACHINE_STATE"
  sleep 10
  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
done
echo "State: ${MACHINE_STATE}"

if [[ -n "$FIRMWARE_UPDATE_SKIPPED" ]]; then
  echo "simulating second boot"
  simulate_boot
fi

MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
while [[ $MACHINE_STATE != "DPUInitializing/WaitingForNetworkConfig" ]]; do
  echo "Waiting for DPU state DPUInitializing/WaitingForNetworkConfig. Current: $MACHINE_STATE"
  sleep 10
  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
done
echo "State: ${MACHINE_STATE}"

# Cleanup old dirs
rm -rf /tmp/forge-hbn-chroot-*

# Make a directory to put the HBN files
# `export` so that instance_handle.sh can use it
# Must match dev/bin/crictl 's HBN_ROOT
export HBN_ROOT=/tmp/forge-hbn-chroot-integration
echo "$HBN_ROOT" >/tmp/hbn_root
mkdir -p ${HBN_ROOT}/etc/frr
mkdir -p ${HBN_ROOT}/etc/network
mkdir -p ${HBN_ROOT}/etc/supervisor/conf.d
mkdir -p ${HBN_ROOT}/etc/cumulus/acl/policy.d
mkdir -p ${HBN_ROOT}/var/support/forge-dhcp/conf

if [ "$FORGE_BOOTSTRAP_KIND" == "kube" ]; then
  # The one we got from kubectl earlier
  export ROOT_CA="${CERT_PATH}/ca.crt"
  export CLIENT_CERT="${CERT_PATH}/tls.crt"
  export CLIENT_KEY="${CERT_PATH}/tls.key"
else
  export ROOT_CA="./dev/certs/forge_developer_local_only_root_cert_pem"
  export CLIENT_CERT="./dev/certs/server_identity.pem"
  export CLIENT_KEY="./dev/certs/server_identity.key"

fi

cat <<! >$DPU_CONFIG_FILE
[forge-system]
api-server = "https://${API_SERVER}"
root-ca = "${ROOT_CA}"
client-cert = "${CLIENT_CERT}"
client-key = "${CLIENT_KEY}"

[machine]
is-fake-dpu = true

[hbn]
root-dir = "$HBN_ROOT"
skip-reload = false
!

echo "HBN files are in ${HBN_ROOT}"

# Start the agent in the background to apply the networking configuration
# Put our fake binaries from dev/bin first on the path so that health check succeeds
export PREV_PATH=$PATH
export PATH=${REPO_ROOT}/dev/bin:$PATH
cargo run -p agent -- --config-path "$DPU_CONFIG_FILE" run --override-machine-id ${DPU_MACHINE_ID} &

# Wait until DPU becomes ready
MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
while [[ $MACHINE_STATE != "HostInitializing/WaitingForDiscovery" ]]; do
  echo "Waiting for DPU state Host/WaitingForDiscovery. Current: $MACHINE_STATE"
  sleep 10
  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$DPU_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
done

echo "simulating third boot"
simulate_boot

echo "DPU is up now."
kill $(pidof forge-dpu-agent)
export PATH=${PREV_PATH}
