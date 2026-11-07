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
use mac_address::MacAddress;
use rpc::errors::RpcDataConversionError;

use super::args::{
    Args, SwitchMetadataCommandAddLabel, SwitchMetadataCommandFromExpectedSwitch,
    SwitchMetadataCommandRemoveLabels, SwitchMetadataCommandSet, SwitchMetadataCommandShow,
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
        Args::FromExpectedSwitch(cmd) => metadata_from_expected_switch(api_client, cmd).await,
    }
}

async fn fetch_switch(
    api_client: &ApiClient,
    switch_id: carbide_uuid::switch::SwitchId,
) -> CarbideCliResult<rpc::forge::Switch> {
    let response = api_client
        .0
        .find_switches(rpc::forge::SwitchQuery {
            name: None,
            switch_id: Some(switch_id),
        })
        .await?;
    response.switches.into_iter().next().ok_or_else(|| {
        CarbideCliError::GenericError(format!("Switch with ID {} was not found", switch_id))
    })
}

async fn metadata_show(
    api_client: &ApiClient,
    cmd: SwitchMetadataCommandShow,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    _extended: bool,
) -> CarbideCliResult<()> {
    let switch = fetch_switch(api_client, cmd.switch).await?;
    let metadata = switch.metadata.ok_or(CarbideCliError::Empty)?;
    crate::metadata::display_metadata(output_file, &output_format, &metadata).await
}

async fn metadata_set(
    api_client: &ApiClient,
    cmd: SwitchMetadataCommandSet,
) -> CarbideCliResult<()> {
    let switch = fetch_switch(api_client, cmd.switch).await?;
    let metadata = crate::metadata::apply_set(switch.metadata, cmd.name, cmd.description)?;
    api_client
        .update_switch_metadata(
            switch
                .id
                .ok_or(RpcDataConversionError::MissingArgument("switch.id"))?,
            metadata,
            switch.version,
        )
        .await
}

async fn metadata_add_label(
    api_client: &ApiClient,
    cmd: SwitchMetadataCommandAddLabel,
) -> CarbideCliResult<()> {
    let switch = fetch_switch(api_client, cmd.switch).await?;
    let metadata = crate::metadata::apply_add_label(switch.metadata, cmd.key, cmd.value)?;
    api_client
        .update_switch_metadata(
            switch
                .id
                .ok_or(RpcDataConversionError::MissingArgument("switch.id"))?,
            metadata,
            switch.version,
        )
        .await
}

async fn metadata_remove_labels(
    api_client: &ApiClient,
    cmd: SwitchMetadataCommandRemoveLabels,
) -> CarbideCliResult<()> {
    let switch = fetch_switch(api_client, cmd.switch).await?;
    let metadata = crate::metadata::apply_remove_labels(switch.metadata, cmd.keys)?;
    api_client
        .update_switch_metadata(
            switch
                .id
                .ok_or(RpcDataConversionError::MissingArgument("switch.id"))?,
            metadata,
            switch.version,
        )
        .await
}

async fn metadata_from_expected_switch(
    api_client: &ApiClient,
    cmd: SwitchMetadataCommandFromExpectedSwitch,
) -> CarbideCliResult<()> {
    let switch = fetch_switch(api_client, cmd.switch).await?;
    let bmc_mac: MacAddress = switch
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.mac.as_ref())
        .map(|mac| mac.parse())
        .transpose()
        .map_or_else(
            |e| {
                Err(CarbideCliError::GenericError(format!(
                    "Invalid BMC MAC address found for Switch with ID {}: {}",
                    cmd.switch, e
                )))
            },
            Ok,
        )?
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No BMC MAC address found for Switch with ID {}",
                cmd.switch
            ))
        })?;

    let mut metadata = switch.metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Switch does not carry Metadata that can be patched".into())
    })?;

    let expected_switches = api_client
        .0
        .get_all_expected_switches()
        .await?
        .expected_switches;
    let expected_switch = expected_switches
        .into_iter()
        .find(|es| {
            es.bmc_mac_address
                .parse::<MacAddress>()
                .is_ok_and(|m| m == bmc_mac)
        })
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No expected Switch found for Switch with ID {} and BMC Mac address {}",
                cmd.switch, bmc_mac
            ))
        })?;

    let expected_switch_metadata = expected_switch.metadata.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "No expected Switch Metadata found for Switch with ID {} and BMC Mac address {}",
            cmd.switch, bmc_mac
        ))
    })?;

    if cmd.replace_all {
        metadata.name = if expected_switch_metadata.name.is_empty() {
            switch
                .id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        } else {
            expected_switch_metadata.name
        };
        metadata.description = expected_switch_metadata.description;
        metadata.labels = expected_switch_metadata.labels;
    } else {
        if !expected_switch_metadata.name.is_empty()
            && (metadata.name.is_empty() || metadata.name == cmd.switch.to_string())
        {
            metadata.name = expected_switch_metadata.name;
        };
        if !expected_switch_metadata.description.is_empty() && metadata.description.is_empty() {
            metadata.description = expected_switch_metadata.description;
        };
        for label in expected_switch_metadata.labels {
            if !metadata.labels.iter().any(|l| l.key == label.key) {
                metadata.labels.push(label);
            }
        }
    }

    api_client
        .update_switch_metadata(
            switch
                .id
                .ok_or(RpcDataConversionError::MissingArgument("switch.id"))?,
            metadata,
            switch.version,
        )
        .await?;
    Ok(())
}
