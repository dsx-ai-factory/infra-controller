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
use rpc::errors::RpcDataConversionError;

use super::args::{
    Args, PowerShelfMetadataCommandAddLabel, PowerShelfMetadataCommandFromExpectedPowerShelf,
    PowerShelfMetadataCommandRemoveLabels, PowerShelfMetadataCommandSet,
    PowerShelfMetadataCommandShow,
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
        Args::FromExpectedPowerShelf(cmd) => {
            metadata_from_expected_power_shelf(api_client, cmd).await
        }
    }
}

async fn fetch_power_shelf(
    api_client: &ApiClient,
    power_shelf_id: carbide_uuid::power_shelf::PowerShelfId,
) -> CarbideCliResult<rpc::forge::PowerShelf> {
    let response = api_client
        .0
        .find_power_shelves(rpc::forge::PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        })
        .await?;
    response.power_shelves.into_iter().next().ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "Power Shelf with ID {} was not found",
            power_shelf_id
        ))
    })
}

async fn metadata_show(
    api_client: &ApiClient,
    cmd: PowerShelfMetadataCommandShow,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    _extended: bool,
) -> CarbideCliResult<()> {
    let power_shelf = fetch_power_shelf(api_client, cmd.power_shelf).await?;
    let metadata = power_shelf.metadata.ok_or(CarbideCliError::Empty)?;
    crate::metadata::display_metadata(output_file, &output_format, &metadata).await
}

async fn metadata_set(
    api_client: &ApiClient,
    cmd: PowerShelfMetadataCommandSet,
) -> CarbideCliResult<()> {
    let ps = fetch_power_shelf(api_client, cmd.power_shelf).await?;
    let metadata = crate::metadata::apply_set(ps.metadata, cmd.name, cmd.description)?;
    api_client
        .update_power_shelf_metadata(
            ps.id
                .ok_or(RpcDataConversionError::MissingArgument("power_shelf.id"))?,
            metadata,
            ps.version,
        )
        .await
}

async fn metadata_add_label(
    api_client: &ApiClient,
    cmd: PowerShelfMetadataCommandAddLabel,
) -> CarbideCliResult<()> {
    let ps = fetch_power_shelf(api_client, cmd.power_shelf).await?;
    let metadata = crate::metadata::apply_add_label(ps.metadata, cmd.key, cmd.value)?;
    api_client
        .update_power_shelf_metadata(
            ps.id
                .ok_or(RpcDataConversionError::MissingArgument("power_shelf.id"))?,
            metadata,
            ps.version,
        )
        .await
}

async fn metadata_remove_labels(
    api_client: &ApiClient,
    cmd: PowerShelfMetadataCommandRemoveLabels,
) -> CarbideCliResult<()> {
    let ps = fetch_power_shelf(api_client, cmd.power_shelf).await?;
    let metadata = crate::metadata::apply_remove_labels(ps.metadata, cmd.keys)?;
    api_client
        .update_power_shelf_metadata(
            ps.id
                .ok_or(RpcDataConversionError::MissingArgument("power_shelf.id"))?,
            metadata,
            ps.version,
        )
        .await
}

async fn metadata_from_expected_power_shelf(
    api_client: &ApiClient,
    cmd: PowerShelfMetadataCommandFromExpectedPowerShelf,
) -> CarbideCliResult<()> {
    let power_shelf = fetch_power_shelf(api_client, cmd.power_shelf).await?;

    let serial_number = power_shelf
        .config
        .as_ref()
        .map(|c| c.name.clone())
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No config/serial number found for Power Shelf with ID {}",
                cmd.power_shelf
            ))
        })?;

    let mut metadata = power_shelf.metadata.ok_or_else(|| {
        CarbideCliError::GenericError(
            "Power Shelf does not carry Metadata that can be patched".into(),
        )
    })?;

    let expected_power_shelves = api_client
        .0
        .get_all_expected_power_shelves()
        .await?
        .expected_power_shelves;
    let expected_power_shelf = expected_power_shelves
        .into_iter()
        .find(|eps| eps.shelf_serial_number == serial_number)
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No expected Power Shelf found for Power Shelf with ID {} and serial number {}",
                cmd.power_shelf, serial_number
            ))
        })?;

    let expected_metadata = expected_power_shelf.metadata.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "No expected Power Shelf Metadata found for Power Shelf with ID {} and serial number {}",
            cmd.power_shelf, serial_number
        ))
    })?;

    if cmd.replace_all {
        metadata.name = if expected_metadata.name.is_empty() {
            power_shelf
                .id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        } else {
            expected_metadata.name
        };
        metadata.description = expected_metadata.description;
        metadata.labels = expected_metadata.labels;
    } else {
        if !expected_metadata.name.is_empty()
            && (metadata.name.is_empty() || metadata.name == cmd.power_shelf.to_string())
        {
            metadata.name = expected_metadata.name;
        };
        if !expected_metadata.description.is_empty() && metadata.description.is_empty() {
            metadata.description = expected_metadata.description;
        };
        for label in expected_metadata.labels {
            if !metadata.labels.iter().any(|l| l.key == label.key) {
                metadata.labels.push(label);
            }
        }
    }

    api_client
        .update_power_shelf_metadata(
            power_shelf
                .id
                .ok_or(RpcDataConversionError::MissingArgument("power_shelf.id"))?,
            metadata,
            power_shelf.version,
        )
        .await?;
    Ok(())
}
