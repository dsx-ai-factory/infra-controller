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

use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::machine::MachineId;
use mac_address::MacAddress;
use rpc::Machine;
use rpc::errors::RpcDataConversionError;

use super::args::{
    Args, MachineMetadataCommandAddLabel, MachineMetadataCommandFromExpectedMachine,
    MachineMetadataCommandRemoveLabels, MachineMetadataCommandSet, MachineMetadataCommandShow,
};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn metadata(
    api_client: &ApiClient,
    cmd: Args,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    format: OutputFormat,
    extended: bool,
) -> CarbideCliResult<()> {
    match cmd {
        Args::Show(cmd) => metadata_show(api_client, cmd, output_file, format, extended).await,
        Args::Set(cmd) => metadata_set(api_client, cmd).await,
        Args::AddLabel(cmd) => metadata_add_label(api_client, cmd).await,
        Args::RemoveLabels(cmd) => metadata_remove_labels(api_client, cmd).await,
        Args::FromExpectedMachine(cmd) => metadata_from_expected_machine(api_client, cmd).await,
    }
}

async fn handle_metadata_show(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    _extended: bool,
    machine: Machine,
) -> CarbideCliResult<()> {
    let metadata = machine.metadata.ok_or(CarbideCliError::Empty)?;
    crate::metadata::display_metadata(output_file, output_format, &metadata).await
}

async fn metadata_show(
    api_client: &ApiClient,
    cmd: MachineMetadataCommandShow,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    format: OutputFormat,
    extended: bool,
) -> CarbideCliResult<()> {
    let mut machines = api_client
        .get_machines_by_ids(&[cmd.machine])
        .await?
        .machines;
    let Some(machine) = machines.pop() else {
        return Err(CarbideCliError::GenericError(format!(
            "Machine with ID {} was not found",
            cmd.machine
        )));
    };
    handle_metadata_show(output_file, &format, extended, machine).await
}

async fn fetch_machine(api_client: &ApiClient, machine_id: MachineId) -> CarbideCliResult<Machine> {
    let mut machines = api_client
        .get_machines_by_ids(&[machine_id])
        .await?
        .machines;
    machines.pop().ok_or_else(|| {
        CarbideCliError::GenericError(format!("Machine with ID {} was not found", machine_id))
    })
}

async fn metadata_set(
    api_client: &ApiClient,
    cmd: MachineMetadataCommandSet,
) -> CarbideCliResult<()> {
    let machine = fetch_machine(api_client, cmd.machine).await?;
    let metadata = crate::metadata::apply_set(machine.metadata, cmd.name, cmd.description)?;
    api_client
        .update_machine_metadata(
            machine
                .id
                .ok_or(RpcDataConversionError::MissingArgument("machine.id"))?,
            metadata,
            machine.version,
        )
        .await
}

async fn metadata_add_label(
    api_client: &ApiClient,
    cmd: MachineMetadataCommandAddLabel,
) -> CarbideCliResult<()> {
    let machine = fetch_machine(api_client, cmd.machine).await?;
    let metadata = crate::metadata::apply_add_label(machine.metadata, cmd.key, cmd.value)?;
    api_client
        .update_machine_metadata(
            machine
                .id
                .ok_or(RpcDataConversionError::MissingArgument("machine.id"))?,
            metadata,
            machine.version,
        )
        .await
}

async fn metadata_remove_labels(
    api_client: &ApiClient,
    cmd: MachineMetadataCommandRemoveLabels,
) -> CarbideCliResult<()> {
    let machine = fetch_machine(api_client, cmd.machine).await?;
    let metadata = crate::metadata::apply_remove_labels(machine.metadata, cmd.keys)?;
    api_client
        .update_machine_metadata(
            machine
                .id
                .ok_or(RpcDataConversionError::MissingArgument("machine.id"))?,
            metadata,
            machine.version,
        )
        .await?;
    Ok(())
}

#[allow(deprecated)]
async fn metadata_from_expected_machine(
    api_client: &ApiClient,
    cmd: MachineMetadataCommandFromExpectedMachine,
) -> CarbideCliResult<()> {
    let mut machines = api_client
        .get_machines_by_ids(&[cmd.machine])
        .await?
        .machines;
    if machines.len() != 1 {
        return Err(CarbideCliError::GenericError(format!(
            "Machine with ID {} was not found",
            cmd.machine
        )));
    }
    let machine = machines.remove(0);
    let bmc_mac: MacAddress = machine
        .bmc_info
        .and_then(|bmc_info| bmc_info.mac)
        .map(|mac| mac.parse())
        .transpose()
        .map_or_else(
            |e| {
                Err(CarbideCliError::GenericError(format!(
                    "Invalid BMC MAC address found for Machine with ID {}: {}",
                    cmd.machine, e
                )))
            },
            Ok,
        )?
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No BMC MAC address found for Machine with ID {}",
                cmd.machine
            ))
        })?;

    let mut metadata = machine.metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Machine does not carry Metadata that can be patched".into())
    })?;

    let expected_machines = api_client
        .0
        .get_all_expected_machines()
        .await?
        .expected_machines;
    let expected_machine = expected_machines
        .into_iter()
        .find(|em| {
            em.bmc_mac_address
                .parse::<MacAddress>()
                .is_ok_and(|m| m == bmc_mac)
        })
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No expected Machine found for Machine with ID {} and BMC Mac address {}",
                cmd.machine, bmc_mac
            ))
        })?;

    let expected_machine_metadata = expected_machine.metadata.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "No expected Machine Metadata found for Machine with ID {} and BMC Mac address {}",
            cmd.machine, bmc_mac
        ))
    })?;

    if cmd.replace_all {
        // Configure the Machines metadata in the same way as if the Machine was freshly ingested
        metadata.name = if expected_machine_metadata.name.is_empty() {
            machine.id.map(|id| id.to_string()).unwrap_or_default()
        } else {
            expected_machine_metadata.name
        };
        metadata.description = expected_machine_metadata.description;
        metadata.labels = expected_machine_metadata.labels;
    } else {
        // Add new data from expected-machines, but current values that might have been the
        // result of previous changed to the Machine.
        // This operation is lossless for existing Metadata.
        if !expected_machine_metadata.name.is_empty()
            && (metadata.name.is_empty() || metadata.name == cmd.machine.to_string())
        {
            metadata.name = expected_machine_metadata.name;
        };
        if !expected_machine_metadata.description.is_empty() && metadata.description.is_empty() {
            metadata.description = expected_machine_metadata.description;
        };
        for label in expected_machine_metadata.labels {
            if !metadata.labels.iter().any(|l| l.key == label.key) {
                metadata.labels.push(label);
            }
        }
    }

    api_client
        .update_machine_metadata(
            machine
                .id
                .ok_or(RpcDataConversionError::MissingArgument("machine.id"))?,
            metadata,
            machine.version,
        )
        .await?;
    Ok(())
}
