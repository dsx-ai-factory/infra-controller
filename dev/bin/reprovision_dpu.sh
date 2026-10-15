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

if [ $# -ne 2 ]; then
	echo
	echo "Must provide command, api_server_ip and api_server_port as positional arguments"
	echo
	echo "    $0 <api_server_ip> <api_server_port>"
	echo
	exit 1
fi

MAX_RETRY=10
source "$(dirname "${BASH_SOURCE[0]}")/host_port.sh" || exit $?
API_SERVER=$(host_port "$1" "$2")
DPU_CONFIG_FILE="/tmp/forge-dpu-agent-sim-config.toml"

HOST_MACHINE_ID=$(grpcurl -d '{}' -insecure "${API_SERVER}" forge.Forge/FindMachines | python3 -c "import sys,json
data=sys.stdin.read()
j=json.loads(data)
for machine in j['machines']:
  if machine['interfaces'][0]['attachedDpuMachineId']['id'] != machine['interfaces'][0]['machineId']['id']:
    print(machine['interfaces'][0]['machineId']['id'])
    break")

DPU_MACHINE_ID=$(grpcurl -d '{"search_config": {"include_dpus": true, "include_predicted_host": true}}' -insecure "${API_SERVER}" forge.Forge/FindMachines | python3 -c "import sys,json
data=sys.stdin.read()
j=json.loads(data)
for machine in j['machines']:
  if machine['interfaces'][0]['attachedDpuMachineId']['id'] == machine['interfaces'][0]['machineId']['id']:
    print(machine['interfaces'][0]['machineId']['id'])
    break")

echo "Found machine with host: $HOST_MACHINE_ID and DPU: $DPU_MACHINE_ID."

# Check Instance state
INSTANCE_ID=$(grpcurl -d '{}' -insecure "${API_SERVER}" forge.Forge/FindInstances | jq ".instances[0].id.value" | tr -d '"')

if [[ -n "$INSTANCE_ID" && "$INSTANCE_ID" != "null" ]]; then
	INSTANCE_STATE=$(grpcurl -d "{\"id\": {\"value\": \"$INSTANCE_ID\"}}" -insecure "${API_SERVER}" forge.Forge/FindInstances | jq ".instances[0].status.tenant.state" | tr -d '"')
	echo "Instance found with ID $INSTANCE_ID in state $INSTANCE_STATE"
fi

#
# Set maintenance mode and trigger reprovision
#
echo "Setting maintenance mode."
grpcurl -d "{\"operation\": 0, \"host_id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"reference\": \"test script\"}" -insecure "${API_SERVER}" forge.Forge/SetMaintenance
echo "Triggering DPU reprovision."
grpcurl -d "{\"dpu_id\": { \"id\": \"$DPU_MACHINE_ID\" }, \"mode\": 0, \"initiator\": 0, \"update_firmware\": true}" -insecure "${API_SERVER}" forge.Forge/TriggerDpuReprovisioning

# In case of Instance, reprovision will be triggered after user approval.
if [[ -n "$INSTANCE_ID" && "$INSTANCE_ID" != "null" ]]; then
	echo "Sending reboot message with apply_updates_on_reboot true".
	if ! grpcurl -d "{\"operation\": 0, \"instance_id\": { \"value\": \"$INSTANCE_ID\" }, \"apply_updates_on_reboot\": true}" -insecure "${API_SERVER}" forge.Forge/InvokeInstancePower; then
		echo "Failed to send reboot approval for instance $INSTANCE_ID." >&2
		exit 1
	fi
else
	echo "No instance found; skipping tenant reboot approval."
fi

#
# State machine handling
#
i=0
while [[ $i -lt $MAX_RETRY ]]; do
	sleep 4

	MACHINE_STATE=$(grpcurl -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" -insecure "$API_SERVER" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
	if [[ "${MACHINE_STATE,,}" =~ "firmwareupgrade" ]]; then
		break
	fi
	echo "Checking machine state. Waiting for it to be in FirmwareUpgrade state. Current: $MACHINE_STATE"
	i=$((i + 1))
done
if [[ $i -ge "$MAX_RETRY" ]]; then
	echo "Even after $MAX_RETRY retries, Host did not come in FirmwareUpgrade state."
	exit 1
fi

# Reboot indication from DPU and wait for state change.
grpcurl -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" -insecure "$API_SERVER" forge.Forge/ForgeAgentControl

i=0
while [[ $i -lt $MAX_RETRY ]]; do
	sleep 4

	MACHINE_STATE=$(grpcurl -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" -insecure "$API_SERVER" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
	if [[ "${MACHINE_STATE,,}" =~ "waitingfornetworkinstall" ]]; then
		break
	fi
	echo "Checking machine state. Waiting for it to be in waitingfornetworkinstall state. Current: $MACHINE_STATE"
	i=$((i + 1))
done
if [[ $i -ge "$MAX_RETRY" ]]; then
	echo "Even after $MAX_RETRY retries, Host did not come in waitingfornetworkinstall state."
	exit 1
fi

# Reboot and discovery completed indication from DPU and wait for state change.
grpcurl -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" -insecure "$API_SERVER" forge.Forge/ForgeAgentControl
grpcurl -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" -insecure "${API_SERVER}" forge.Forge/DiscoveryCompleted

i=0
while [[ $i -lt $MAX_RETRY ]]; do
	sleep 10

	MACHINE_STATE=$(grpcurl -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" -insecure "$API_SERVER" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
	if [[ "${MACHINE_STATE,,}" =~ "waitingfornetworkconfig" ]]; then
		break
	fi
	echo "Checking machine state. Waiting for it to be in waitingfornetworkconfig state. Current: $MACHINE_STATE"
	i=$((i + 1))
done
if [[ $i -ge "$MAX_RETRY" ]]; then
	echo "Even after $MAX_RETRY retries, Host did not come in waitingfornetworkconfig state."
	exit 1
fi

echo "HBN files are in ${HBN_ROOT}"

# Reboot indication from DPU and wait for state change.
grpcurl -d "{\"machine_id\": {\"id\": \"$DPU_MACHINE_ID\"}}" -insecure "$API_SERVER" forge.Forge/ForgeAgentControl

# Start the agent in the background to apply the networking configuration
# Put our fake binaries from dev/bin first on the path so that health check succeeds
export PREV_PATH=$PATH
export PATH=${REPO_ROOT}/dev/bin:$PATH
cargo run -p agent -- --config-path "$DPU_CONFIG_FILE" run --override-machine-id ${DPU_MACHINE_ID} &

if [[ -z "$INSTANCE_ID" || "$INSTANCE_ID" == "null" ]]; then # No instance is configured
	# Next state is Discovered.
	i=0
	while [[ $i -lt $MAX_RETRY ]]; do
		sleep 10

		MACHINE_STATE=$(grpcurl -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" -insecure "$API_SERVER" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
		if [[ "${MACHINE_STATE,,}" =~ "discovered" ]]; then
			break
		fi
		echo "Checking machine state. Waiting for it to be in discovered state. Current: $MACHINE_STATE"
		i=$((i + 1))
	done
	if [[ $i -ge "$MAX_RETRY" ]]; then
		echo "Even after $MAX_RETRY retries, Host did not come in discovered state."
    kill $(pidof forge-dpu-agent)
    export PATH=${PREV_PATH}
		exit 1
	fi

	# Reboot indication from DPU and wait for state change.
	grpcurl -d "{\"machine_id\": {\"id\": \"$HOST_MACHINE_ID\"}}" -insecure "$API_SERVER" forge.Forge/ForgeAgentControl
fi

# Next state is ready
i=0
while [[ $i -lt $MAX_RETRY ]]; do
	sleep 10

	MACHINE_STATE=$(grpcurl -d "{\"id\": {\"id\": \"$HOST_MACHINE_ID\"}, \"search_config\": {\"include_dpus\": true}}" -insecure "$API_SERVER" forge.Forge/FindMachines | jq ".machines[0].state" | tr -d '"')
	if [[ "${MACHINE_STATE,,}" =~ "ready" ]]; then
		break
	fi
	echo "Checking machine state. Waiting for it to be in ready state. Current: $MACHINE_STATE"
	i=$((i + 1))
done

kill $(pidof forge-dpu-agent)
export PATH=${PREV_PATH}

if [[ $i -ge "$MAX_RETRY" ]]; then
	echo "Even after $MAX_RETRY retries, Host did not come in ready state."
	exit 1
fi

# Clear maintenance mode
grpcurl -d "{\"operation\": 1, \"host_id\": {\"id\": \"$HOST_MACHINE_ID\" }}" -insecure "${API_SERVER}" forge.Forge/SetMaintenance
