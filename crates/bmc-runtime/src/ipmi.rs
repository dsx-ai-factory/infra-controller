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

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{IpmiOps, PlatformError};
use carbide_ipmi::IPMITool;
use carbide_secrets::credentials::CredentialKey;
use carbide_uuid::machine::MachineId;

/// Per-endpoint adapter exposing only the IPMI operations allowed to drivers.
///
/// `carbide-ipmi` retains responsibility for reading the credential identified
/// by `credential_key`; neither the secret nor a secret-bearing lease is stored
/// by this adapter. `ipmitool` failures carry no structure NICo can classify,
/// so every failure is reported as unreachable.
pub struct EndpointIpmiOps {
    tool: Arc<dyn IPMITool>,
    machine_id: MachineId,
    address: SocketAddr,
    credential_key: CredentialKey,
}

impl EndpointIpmiOps {
    /// Creates the adapter for one BMC endpoint.
    pub const fn new(
        tool: Arc<dyn IPMITool>,
        machine_id: MachineId,
        address: SocketAddr,
        credential_key: CredentialKey,
    ) -> Self {
        Self {
            tool,
            machine_id,
            address,
            credential_key,
        }
    }
}

#[async_trait]
impl IpmiOps for EndpointIpmiOps {
    async fn bmc_cold_reset(&self) -> Result<(), PlatformError> {
        self.tool
            .bmc_cold_reset(self.address, &self.credential_key)
            .await
            .map_err(|_| PlatformError::Unreachable)
    }

    async fn chassis_power_reset(&self) -> Result<(), PlatformError> {
        self.tool
            .restart(&self.machine_id, self.address, false, &self.credential_key)
            .await
            .map_err(|_| PlatformError::Unreachable)
    }
}
