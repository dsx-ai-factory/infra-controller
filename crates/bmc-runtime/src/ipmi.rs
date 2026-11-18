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

use std::fmt::Display;
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
/// so every failure is logged with its cause and reported as unreachable.
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

    /// Logs why `operation` failed, which `Unreachable` cannot carry. The
    /// password reaches `ipmitool` through its environment, so the error holds
    /// no credential.
    fn unreachable(&self, operation: &'static str, error: &impl Display) -> PlatformError {
        tracing::warn!(
            bmc = %self.address,
            machine_id = %self.machine_id,
            operation,
            error = %format_args!("{error:#}"),
            "IPMI command failed"
        );
        PlatformError::Unreachable
    }
}

#[async_trait]
impl IpmiOps for EndpointIpmiOps {
    async fn bmc_cold_reset(&self) -> Result<(), PlatformError> {
        self.tool
            .bmc_cold_reset(self.address, &self.credential_key)
            .await
            .map_err(|error| self.unreachable("bmc_cold_reset", &error))
    }

    async fn chassis_power_reset(&self) -> Result<(), PlatformError> {
        self.tool
            .restart(&self.machine_id, self.address, false, &self.credential_key)
            .await
            .map_err(|error| self.unreachable("chassis_power_reset", &error))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_instrument::testing::capture_logs_async;
    use carbide_secrets::credentials::{BmcCredentialType, CredentialKey};

    use super::*;

    struct FailingTool;

    #[async_trait]
    impl IPMITool for FailingTool {
        async fn bmc_cold_reset(
            &self,
            _bmc_address: SocketAddr,
            _credential_key: &CredentialKey,
        ) -> Result<(), eyre::Report> {
            Err(eyre::eyre!("no credentials for key found"))
        }

        async fn restart(
            &self,
            _machine_id: &MachineId,
            _bmc_address: SocketAddr,
            _legacy_boot: bool,
            _credential_key: &CredentialKey,
        ) -> Result<(), eyre::Report> {
            Err(eyre::eyre!("unable to establish IPMI v2 / RMCP+ session"))
        }
    }

    /// A failed command is reported as unreachable, and its cause is logged
    /// so an authentication or credential failure is told apart from a
    /// network one.
    #[tokio::test]
    async fn failed_commands_log_their_cause() {
        let ipmi = EndpointIpmiOps::new(
            Arc::new(FailingTool),
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .expect("machine id"),
            "192.0.2.10:623".parse().expect("address"),
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: "02:00:00:00:00:01".parse().expect("MAC"),
                },
            },
        );
        for (operation, cause) in [
            ("bmc_cold_reset", "no credentials for key found"),
            (
                "chassis_power_reset",
                "unable to establish IPMI v2 / RMCP+ session",
            ),
        ] {
            let (result, logs) = capture_logs_async(async {
                match operation {
                    "bmc_cold_reset" => ipmi.bmc_cold_reset().await,
                    _ => ipmi.chassis_power_reset().await,
                }
            })
            .await;
            assert_eq!(result, Err(PlatformError::Unreachable), "{operation}");
            let log = logs
                .iter()
                .find(|log| log.message == "IPMI command failed")
                .unwrap_or_else(|| panic!("{operation} logs its failure"));
            assert_eq!(log.field("operation"), Some(operation));
            assert_eq!(log.field("error"), Some(cause));
            assert_eq!(log.field("bmc"), Some("192.0.2.10:623"));
        }
    }
}
