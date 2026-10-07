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

use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use mac_address::MacAddress;
use prettytable::{Cell, Row, Table};
use rpc::errors::RpcDataConversionError;
use rpc::forge::{BmcEndpointRequest, PowerOptions};

use super::args::{ShowPowerOptions, UpdatePowerOptions};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn power_options_show(
    args: ShowPowerOptions,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    if let Some(machine_id) = args.machine {
        let mut power_options = api_client.get_power_options(vec![machine_id]).await?;
        if power_options.len() != 1 {
            return Err(CarbideCliError::GenericError(format!(
                "More than one entry is received for id: {machine_id}; Data: {power_options:?}"
            )));
        }

        let power_options = power_options.remove(0);
        power_options_show_one(&power_options, output_format)?;

        return Ok(());
    }

    power_options_show_all(output_format, api_client).await
}

fn power_options_show_one(
    power_option: &PowerOptions,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    if output_format == OutputFormat::Json {
        println!("{}", serde_json::to_string(power_option)?);
        return Ok(());
    }
    let mut lines = String::new();
    let width = 35;
    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Host ID",
        power_option
            .host_id
            .map(|x| x.to_string())
            .unwrap_or_default()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Desired Power State Version", power_option.desired_power_state_version,
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {:?}",
        "Desired Power State",
        power_option.desired_state()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Desired Power State (Updated at)",
        power_option
            .desired_state_updated_at
            .map(|x| x.to_string())
            .unwrap_or_default()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {:?}",
        "Actual Power State",
        power_option.actual_state()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Actual Power State (Updated at)",
        power_option
            .actual_state_updated_at
            .map(|x| x.to_string())
            .unwrap_or_default()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Next Power State Fetch At",
        power_option
            .next_power_state_fetch_at
            .map(|x| x.to_string())
            .unwrap_or_default()
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}",
        "Current Off Counter", power_option.off_counter
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {}/{}",
        "Tried Triggering On At/Counter",
        power_option
            .tried_triggering_on_at
            .map(|x| x.to_string())
            .unwrap_or_default(),
        power_option.tried_triggering_on_counter
    )?;

    writeln!(
        &mut lines,
        "{:<width$}: {} (Carbide will wait for DPUs to come up before rebooting host after power on)",
        "Wait Until Next Reboot",
        power_option
            .wait_until_time_before_performing_next_power_action
            .map(|x| x.to_string())
            .unwrap_or_default(),
    )?;

    print!("{lines}");
    Ok(())
}

async fn power_options_show_all(
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let mut table = Table::new();
    let all_options = api_client.get_power_options(vec![]).await?;

    if output_format == OutputFormat::Json {
        println!("{}", serde_json::to_string(&all_options)?);
        return Ok(());
    }
    let headers = vec![
        "Host ID",
        "Desired Power State",
        "Actual Power State",
        "Off Counter/Next Cycle At",
    ];

    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));

    for power_option in all_options {
        table.add_row(prettytable::row![
            power_option
                .host_id
                .map(|x| x.to_string())
                .unwrap_or_default(),
            format!(
                "{:?} ({})\n{}",
                power_option.desired_state(),
                power_option.desired_power_state_version,
                power_option
                    .desired_state_updated_at
                    .map(|x| x.to_string())
                    .unwrap_or_default(),
            ),
            format!(
                "{:?}\n{}",
                power_option.actual_state(),
                power_option
                    .actual_state_updated_at
                    .map(|x| x.to_string())
                    .unwrap_or_default()
            ),
            format!(
                "{}\n{}",
                power_option.off_counter,
                power_option
                    .next_power_state_fetch_at
                    .map(|x| x.to_string())
                    .unwrap_or_default(),
            )
        ]);
    }

    table.printstd();
    Ok(())
}

pub(super) async fn update_power_option(
    args: UpdatePowerOptions,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let Some(updated_power_option) = api_client
        .0
        .update_power_option(args)
        .await?
        .response
        .into_iter()
        .next()
    else {
        return Err(RpcDataConversionError::MissingArgument("response").into());
    };
    println!("Power options updated successfully!!");
    println!("Updated power options are");
    power_options_show_one(&updated_power_option, OutputFormat::AsciiTable)
}

pub(crate) async fn get_machine_state(
    api_client: &ApiClient,
    mac_address: &MacAddress,
) -> Result<(), CarbideCliError> {
    let machine_state = api_client
        .0
        .determine_machine_ingestion_state(BmcEndpointRequest {
            mac_address: Some(mac_address.to_string()),
            ip_address: "".to_string(),
        })
        .await?;

    println!(
        "Machine ingestion state is: {:#?}",
        machine_state.machine_ingestion_state()
    );

    Ok(())
}

pub(crate) async fn allow_ingestion_and_power_on(
    api_client: &ApiClient,
    mac_address: &MacAddress,
) -> Result<(), CarbideCliError> {
    api_client
        .0
        .allow_ingestion_and_power_on(BmcEndpointRequest {
            mac_address: Some(mac_address.to_string()),
            ip_address: "".to_string(),
        })
        .await?;

    println!("Command completed without errors");

    Ok(())
}
