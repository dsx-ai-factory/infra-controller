/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine::HostMachineId;
use eyre::{Context, eyre};
use model::nic_firmware::NicFirmwareCardSelection;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::MachinesByIdsRequest;
use serde::Serialize;

use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::{async_write, async_write_table_as_csv, async_writeln};

/// `MachineConfigOutput` keeps the edit token beside the selected profiles;
/// the card array uses the same JSON representation accepted by `--cards-file`.
#[derive(Serialize)]
struct MachineConfigOutput {
    machine_id: HostMachineId,
    version: String,
    profile_id: Option<String>,
    card_selections: Vec<NicFirmwareCardSelection>,
}

pub(super) async fn show(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let response = ctx
        .api_client
        .0
        .find_machines_by_ids(MachinesByIdsRequest {
            machine_ids: vec![args.machine_id.into()],
            include_history: false,
        })
        .await
        .wrap_err_with(|| {
            format!(
                "while attempting to read NIC firmware configuration for {}",
                args.machine_id
            )
        })?;
    let mut machines = response.machines.into_iter();
    let machine = machines
        .next()
        .ok_or(CarbideCliError::MachineNotFound(args.machine_id.into()))?;
    if machines.next().is_some() || machine.id != Some(args.machine_id.into()) {
        return Err(eyre!(
            "core returned an unexpected machine for {}",
            args.machine_id
        )
        .into());
    }
    let config = machine
        .config
        .ok_or_else(|| eyre!("core returned machine {} without a config", args.machine_id))?;
    let output = MachineConfigOutput {
        machine_id: args.machine_id,
        version: machine.version,
        profile_id: config.nic_firmware_profile_id,
        card_selections: config
            .nic_firmware_card_selections
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()
            .wrap_err("while attempting to decode saved NIC firmware card selections")?,
    };
    match ctx.config.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&output)
                .wrap_err("while attempting to serialize NIC firmware configuration as JSON")?;
            async_writeln!(ctx.output_file, "{json}")
                .wrap_err("while attempting to write NIC firmware configuration")?;
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&output)
                .wrap_err("while attempting to serialize NIC firmware configuration as YAML")?;
            async_write!(ctx.output_file, "{yaml}")
                .wrap_err("while attempting to write NIC firmware configuration")?;
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {
            let mut table = Table::new();
            table.set_titles(row![
                "Machine ID",
                "Machine Version",
                "Scope",
                "Selector",
                "Profile ID"
            ]);
            table.add_row(row![
                output.machine_id,
                output.version,
                "Machine",
                "",
                output.profile_id.as_deref().unwrap_or("")
            ]);
            for card in output.card_selections {
                table.add_row(row![
                    output.machine_id,
                    output.version,
                    "Card",
                    card.selector,
                    card.profile_id
                ]);
            }
            match ctx.config.format {
                OutputFormat::Csv => async_write_table_as_csv!(ctx.output_file, table)
                    .wrap_err("while attempting to write NIC firmware configuration as CSV")?,
                _ => async_write!(ctx.output_file, "{table}")
                    .wrap_err("while attempting to write NIC firmware configuration")?,
            }
        }
    }
    Ok(())
}
