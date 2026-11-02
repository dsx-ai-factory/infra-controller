#!/bin/bash
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
set -eo pipefail

# This script can be used to simulate discovering a Host in the docker-compose setup
# It will use a hardcoded MAC address "00:11:22:33:44:66" (see `host_dhcp_discovery.json` in
# the respective env directory) to perform a DHCP request, and then submits Machine details.
# If you need more than one HOST, you can edit the MAC address in the file and call
# `discover_dpu.sh` to create DPU first and `discover_host.sh` to create host again

MAX_RETRY=20
if [ $# -ne 4 ]; then
  echo
  echo "Must provide api_server_host, api_server_port, data directory and discovery mode as positional arguments"
  echo
  echo "    $0" '<api_server_host> <api_server_port> <data_dir> [full|dhcp-only]'
  echo
  exit 1
fi

if [ "$FORGE_BOOTSTRAP_KIND" == "kube" ]; then
  export CERT_PATH=${CERT_PATH:=/tmp/localdev-certs}
  export GRPCURL="grpcurl --key ${CERT_PATH}/tls.key --cacert ${CERT_PATH}/ca.crt --cert ${CERT_PATH}/tls.crt"
else
  export DISABLE_TLS_ENFORCEMENT=true
  export NO_DPU_CONTAINERS=true
  export GRPCURL="grpcurl -insecure"
fi

API_SERVER_HOST=$1
API_SERVER_PORT=$2
source "$(dirname "${BASH_SOURCE[0]}")/host_port.sh"
API_SERVER=$(host_port "$API_SERVER_HOST" "$API_SERVER_PORT")
HOST_DHCP_FILE=$3/host_dhcp_discovery.json
HOST_MACHINE_FILE=$3/host_machine_discovery.json
BMC_METADATA_FILE=$3/update_host_bmc_metadata.json
DISCOVERY_MODE=$4

# Relies on the assumption that the DPU is the only entry
DPU_INFO=$(${GRPCURL} -d "{\"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines)
DPU_MACHINE_ID=$(jq -rn "${DPU_INFO}.machines[0].interfaces[0].machineId.id")
DPU_INTERFACE_ID=$(jq -rn "${DPU_INFO}.machines[0].interfaces[0].id.value")
echo "DPU machine id: ${DPU_MACHINE_ID}"
echo "DPU interface id: ${DPU_INTERFACE_ID}"

HBN_ROOT=$(cat /tmp/hbn_root)
DPU_CONFIG_FILE="/tmp/forge-dpu-agent-sim-config.toml"

# Determine the CircuitId that our host needs to use
# We use the first network segment that we can find
RESULT=$(${GRPCURL} "${API_SERVER}" forge.Forge/FindNetworkSegments)
CIRCUIT_ID=$(echo "$RESULT" | jq -r '(.networkSegments[0].config.prefixes[0].circuitId // .networkSegments[0].prefixes[0].circuitId // empty)')
if [ -z "$CIRCUIT_ID" ]; then
  echo "ERROR: could not determine CIRCUIT_ID from FindNetworkSegments response" >&2
  exit 1
fi
echo "Circuit ID is $CIRCUIT_ID"

# Simulate the DHCP request of a x86 host
# IMPORTANT: This only works a single time, because the loopback IP used in this request is hardcoded
# And that hardcoded IP will only be assigned to the first DPU that is discovered
HOST_DHCP_REQUEST=$(jq --arg circuit_id "$CIRCUIT_ID" '.circuit_id = $circuit_id' "$HOST_DHCP_FILE")
RESULT=$(echo "$HOST_DHCP_REQUEST" | ${GRPCURL} -d @ "${API_SERVER}" forge.Forge/DiscoverDhcp)
MACHINE_INTERFACE_ID=$(echo "$RESULT" | jq ".machineInterfaceId.value" | tr -d '"')
echo "Using Machine Interface with ID $MACHINE_INTERFACE_ID"
if [ "${DISCOVERY_MODE}" == "dhcp-only" ]; then
  exit 0
fi

# Simulate the Machine discovery request of a x86 host
DISCOVER_MACHINE_REQUEST=$(jq --arg machine_interface_id "$MACHINE_INTERFACE_ID" '.machine_interface_id.value = $machine_interface_id' "$HOST_MACHINE_FILE")

# Assuming ManagedHost is Host/Init state now.
RESULT=$(echo "$DISCOVER_MACHINE_REQUEST" | ${GRPCURL} -d @ "${API_SERVER}" forge.Forge/DiscoverMachine)
HOST_MACHINE_ID=$(echo "$RESULT" | jq ".machineId.id" | tr -d '"')
ACTION=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/ForgeAgentControl | jq -r .action)
echo "Forge agent control: ${ACTION}"

# Give it a BMC IP and credentials
UPDATE_BMC_METADATA=$(jq --arg machine_id "$HOST_MACHINE_ID" '.machine_id.id = $machine_id' "$BMC_METADATA_FILE")
${GRPCURL} -d "$UPDATE_BMC_METADATA" "${API_SERVER}" forge.Forge/UpdateBMCMetaData
echo "Created HOST Machine with ID $HOST_MACHINE_ID. Starting discovery."

MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
echo "State: ${MACHINE_STATE}"
ACTION=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/ForgeAgentControl | jq -r .action)
echo "Host Forge agent control: ${ACTION}"
ACTION=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/ForgeAgentControl | jq -r .action)
echo "DPU Forge agent control: ${ACTION}"

# Mark discovery complete
RESULT=$(${GRPCURL} -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/DiscoveryCompleted)

# Wait past the enforced delay until we look for DPU to have rebooted
i=0
while [[ $i -lt $MAX_RETRY ]]; do
  sleep 4

  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
  if [[ "$MACHINE_STATE" == *WaitForDPUUp* ]]; then
    break
  fi
  echo "Checking machine state. Waiting for it to be in WaitForDPUUp state. Current: $MACHINE_STATE"
  i=$((i + 1))
done
if [[ $i -ge "$MAX_RETRY" ]]; then
  echo "Even after $MAX_RETRY retries, Host did not come in WaitForDPUUp state."
  exit 1
fi

MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
echo "State: ${MACHINE_STATE}"

# Run forge-dpu-agent to report an observation, which shows that DPU has now rebooted
# Start the agent in the background to apply the networking configuration
# Put our fake binaries from dev/bin first on the path so that health check succeeds
export PREV_PATH=$PATH
export PATH=${REPO_ROOT}/dev/bin:$PATH
cd ${REPO_ROOT} && cargo run -p agent -- --config-path "$DPU_CONFIG_FILE" run --override-machine-id ${DPU_MACHINE_ID} &

# Wait until host reaches discovered state.
i=0
MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
while [[ $MACHINE_STATE != "HostInitializing/Discovered" && $MACHINE_STATE != "Ready" && $i -lt $MAX_RETRY ]]; do
  sleep 4

  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
  echo "Checking machine state. Waiting for it to be in Host/Discovered or Ready state. Current: $MACHINE_STATE"
  i=$((i + 1))
done

if [[ $i -ge "$MAX_RETRY" ]]; then
  echo "Even after $MAX_RETRY retries, Host did not come in Host/Discovered state."
  kill $(pidof forge-dpu-agent)
  export PATH=${PREV_PATH}
  exit 1
fi

${GRPCURL} -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/ForgeAgentControl
${GRPCURL} -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" "${API_SERVER}" forge.Forge/RebootCompleted

# Wait until host reaches ready state.
i=0
while [[ $MACHINE_STATE != "Ready" && $i -lt $MAX_RETRY ]]; do
  sleep 2

  MACHINE_STATE=$(${GRPCURL} -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" "${API_SERVER}" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
  echo "Checking machine state. Waiting for it to be in Ready state. Current: $MACHINE_STATE"
  i=$((i + 1))
done

kill $(pidof forge-dpu-agent)
export PATH=${PREV_PATH}

if [[ $i -ge "$MAX_RETRY" ]]; then
  echo "Even after $MAX_RETRY retries, Host did not come in Ready state."
  exit 1
fi

echo "ManagedHost is up in Ready state."
