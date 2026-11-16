/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineId;

use crate::CarbideClientError;
use crate::cfg::Options;
use crate::client::create_forge_client;

pub(super) async fn completed(
    config: &Options,
    machine_id: &MachineId,
) -> Result<(), CarbideClientError> {
    let mut client = create_forge_client(config).await?;
    let request = tonic::Request::new(rpc::MachineDiscoveryCompletedRequest {
        machine_id: Some(*machine_id),
    });
    client.discovery_completed(request).await?;
    Ok(())
}
pub(super) async fn rebooted(
    config: &Options,
    machine_id: &MachineId,
) -> Result<(), CarbideClientError> {
    let mut client = create_forge_client(config).await?;
    let request = tonic::Request::new(rpc::MachineRebootCompletedRequest {
        machine_id: Some(*machine_id),
    });
    client.reboot_completed(request).await?;
    Ok(())
}

/// A freshly registered Scout proves this exact Core-owned boot has left the ISO.
pub(super) async fn acknowledge_boot(
    config: &Options,
    machine_id: &MachineId,
) -> Result<(), CarbideClientError> {
    let command_line = std::fs::read_to_string("/proc/cmdline")?;
    let parameter = |key: &str| {
        command_line
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
            .map(str::to_string)
    };
    let expected_machine_id = parameter("host_boot_expected_machine_id=");
    let generation = parameter("host_boot_generation=");
    let token = parameter("host_boot_token=");
    let (expected_machine_id, generation, token) = match (expected_machine_id, generation, token) {
        (None, None, None) => return Ok(()),
        (Some(id), Some(generation), Some(token)) => (id, generation, token),
        _ => {
            return Err(CarbideClientError::GenericError(
                "incomplete Core boot attempt parameters".into(),
            ));
        }
    };
    let mut client = create_forge_client(config).await?;
    client
        .acknowledge_host_boot(tonic::Request::new(rpc::AcknowledgeHostBootRequest {
            expected_machine_id: Some(::rpc::common::Uuid {
                value: expected_machine_id,
            }),
            generation,
            acknowledgement_token: token,
            evidence: rpc::HostBootEvidence::ScoutRegistered as i32,
            machine_id: Some(*machine_id),
        }))
        .await?;
    Ok(())
}
