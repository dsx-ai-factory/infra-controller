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
