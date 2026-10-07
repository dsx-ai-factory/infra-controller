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

use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use carbide_uuid::machine_validation::MachineValidationId;
use lazy_static::lazy_static;
use rpc::forge::{ForgeAgentControlResponse, MachineArchitecture};
use tempfile::TempDir;

use crate::api_client::ClientApiError;
use crate::config::MachineATronContext;

lazy_static! {
    static ref BMC_MOCK_SOCKET_TEMP_DIR: TempDir = tempfile::Builder::new()
        .prefix("bmc-mock")
        .tempdir()
        .unwrap();
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum PxeBootTarget {
    Exit,
    Scout,    // PXE script is booting scout.efi
    DpuAgent, // PXE script is booting carbide.efi
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct PxeResponse {
    pub(super) boot_target: PxeBootTarget,
    pub(super) machine_interface_id: Option<MachineInterfaceId>,
}

#[derive(thiserror::Error, Debug)]
pub(super) enum PxeError {
    #[error("API client error running PXE request: {0}")]
    ClientApi(#[from] ClientApiError),
    #[error("PXE request failed with status: {0}")]
    PxeRequest(#[from] tonic::Status),
    #[error("error sending PXE request: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("PXE script contains an invalid machine interface ID '{0}'")]
    InvalidMachineInterfaceId(String),
}

pub(super) async fn forge_agent_control(
    app_context: &MachineATronContext,
    machine_id: MachineId,
) -> Option<ForgeAgentControlResponse> {
    match app_context
        .forge_api_client
        .forge_agent_control(machine_id)
        .await
    {
        Ok(response) => Some(response),
        Err(e) => {
            if e.code() == tonic::Code::NotFound {
                return None;
            }
            tracing::warn!(
                error = %e,
                "Error getting control action",
            );
            Some(ForgeAgentControlResponse::noop())
        }
    }
}

pub(super) fn get_validation_id(
    response: &ForgeAgentControlResponse,
) -> Option<MachineValidationId> {
    if let Some(rpc::forge::forge_agent_control_response::Action::MachineValidation(
        machine_validation,
    )) = &response.action
    {
        machine_validation.validation_id
    } else {
        None
    }
}

pub(super) async fn send_pxe_boot_request(
    app_context: &MachineATronContext,
    arch: MachineArchitecture,
    client_ip: std::net::IpAddr,
    product: Option<String>,
) -> Result<PxeResponse, PxeError> {
    let pxe_script = app_context
        .forge_api_client
        .get_pxe_instructions(rpc::forge::PxeInstructionRequest {
            arch: arch.into(),
            product,
            client_ip: Some(client_ip.to_string()),
            ..Default::default()
        })
        .await?
        .pxe_script;
    tracing::info!("PXE Request successful");

    let response = parse_pxe_response(&pxe_script)?;
    match response.boot_target {
        PxeBootTarget::Exit => {
            tracing::info!("PXE Request is EXIT");
        }
        PxeBootTarget::Scout => {
            tracing::info!("PXE Request boots Scout");
        }
        PxeBootTarget::DpuAgent => {
            tracing::info!("PXE Request boots DPU agent");
        }
    }

    Ok(response)
}

fn parse_pxe_response(pxe_script: &str) -> Result<PxeResponse, PxeError> {
    let machine_interface_id = pxe_script
        .split_ascii_whitespace()
        .find_map(|token| token.strip_prefix("machine_id="))
        .or_else(|| {
            pxe_script.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("echo Interface ID:")
                    .map(str::trim)
            })
        })
        .map(|value| {
            value
                .parse()
                .map_err(|_| PxeError::InvalidMachineInterfaceId(value.to_string()))
        })
        .transpose()?;

    let boot_target = if pxe_script.contains("exit") {
        PxeBootTarget::Exit
    } else if let Some(kernel_url) = pxe_script
        .lines()
        .find(|l| l.starts_with("kernel"))
        .and_then(|l| l.split_ascii_whitespace().nth(1))
    {
        if kernel_url.ends_with("/carbide.efi") {
            PxeBootTarget::DpuAgent
        } else if kernel_url.ends_with("/scout.efi") {
            PxeBootTarget::Scout
        } else {
            tracing::error!(
                pxe_script = %pxe_script,
                "Could not determine what to do with kernel URL returned by PXE script, will treat as 'exit'",
            );
            PxeBootTarget::Exit
        }
    } else {
        tracing::error!(
            pxe_script = %pxe_script,
            "Could not determine what to do with PXE script (no kernel line, no exit line), will treat as 'exit'",
        );
        PxeBootTarget::Exit
    };

    Ok(PxeResponse {
        boot_target,
        machine_interface_id,
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    #[test]
    fn parses_pxe_boot_target_and_machine_interface_id() {
        let machine_interface_id: MachineInterfaceId =
            "0fd6e9a3-06fc-4a22-ad29-aca299677b00".parse().unwrap();

        check_cases(
            [
                Case {
                    scenario: "Scout kernel command line",
                    input: format!(
                        "kernel http://carbide-pxe/scout.efi console=tty0 machine_id={machine_interface_id}"
                    ),
                    expect: Yields(PxeResponse {
                        boot_target: PxeBootTarget::Scout,
                        machine_interface_id: Some(machine_interface_id),
                    }),
                },
                Case {
                    scenario: "DPU agent kernel command line",
                    input: format!(
                        "kernel http://carbide-pxe/carbide.efi machine_id={machine_interface_id}"
                    ),
                    expect: Yields(PxeResponse {
                        boot_target: PxeBootTarget::DpuAgent,
                        machine_interface_id: Some(machine_interface_id),
                    }),
                },
                Case {
                    scenario: "known interface exiting to installed OS",
                    input: format!("echo Interface ID: {machine_interface_id}\nexit ||"),
                    expect: Yields(PxeResponse {
                        boot_target: PxeBootTarget::Exit,
                        machine_interface_id: Some(machine_interface_id),
                    }),
                },
                Case {
                    scenario: "unknown interface exits without identity",
                    input: "echo this is an unknown host interface\nexit ||".to_string(),
                    expect: Yields(PxeResponse {
                        boot_target: PxeBootTarget::Exit,
                        machine_interface_id: None,
                    }),
                },
                Case {
                    scenario: "malformed machine interface ID",
                    input: "kernel http://carbide-pxe/scout.efi machine_id=not-a-uuid".to_string(),
                    expect: Fails,
                },
            ],
            |pxe_script| parse_pxe_response(&pxe_script).map_err(drop),
        );
    }
}
