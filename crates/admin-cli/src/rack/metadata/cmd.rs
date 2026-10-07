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
    Args, RackMetadataCommandAddLabel, RackMetadataCommandFromExpectedRack,
    RackMetadataCommandRemoveLabels, RackMetadataCommandSet, RackMetadataCommandShow,
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
        Args::FromExpectedRack(cmd) => metadata_from_expected_rack(api_client, cmd).await,
    }
}

async fn metadata_show(
    api_client: &ApiClient,
    cmd: RackMetadataCommandShow,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    _extended: bool,
) -> CarbideCliResult<()> {
    let mut racks = api_client.get_one_rack(cmd.rack.clone()).await?.racks;
    let Some(rack) = racks.pop() else {
        return Err(CarbideCliError::GenericError(format!(
            "Rack with ID {} was not found",
            cmd.rack
        )));
    };
    let metadata = rack.metadata.ok_or(CarbideCliError::Empty)?;
    crate::metadata::display_metadata(output_file, &output_format, &metadata).await
}

async fn fetch_rack(
    api_client: &ApiClient,
    rack_id: carbide_uuid::rack::RackId,
) -> CarbideCliResult<rpc::forge::Rack> {
    let mut racks = api_client.get_one_rack(rack_id.clone()).await?.racks;
    racks.pop().ok_or_else(|| {
        CarbideCliError::GenericError(format!("Rack with ID {} was not found", rack_id))
    })
}

async fn metadata_set(api_client: &ApiClient, cmd: RackMetadataCommandSet) -> CarbideCliResult<()> {
    let rack = fetch_rack(api_client, cmd.rack).await?;
    let metadata = crate::metadata::apply_set(rack.metadata, cmd.name, cmd.description)?;
    api_client
        .update_rack_metadata(
            rack.id
                .ok_or(RpcDataConversionError::MissingArgument("rack.id"))?,
            metadata,
            rack.version,
        )
        .await
}

async fn metadata_add_label(
    api_client: &ApiClient,
    cmd: RackMetadataCommandAddLabel,
) -> CarbideCliResult<()> {
    let rack = fetch_rack(api_client, cmd.rack).await?;
    let metadata = crate::metadata::apply_add_label(rack.metadata, cmd.key, cmd.value)?;
    api_client
        .update_rack_metadata(
            rack.id
                .ok_or(RpcDataConversionError::MissingArgument("rack.id"))?,
            metadata,
            rack.version,
        )
        .await
}

async fn metadata_remove_labels(
    api_client: &ApiClient,
    cmd: RackMetadataCommandRemoveLabels,
) -> CarbideCliResult<()> {
    let rack = fetch_rack(api_client, cmd.rack).await?;
    let metadata = crate::metadata::apply_remove_labels(rack.metadata, cmd.keys)?;
    api_client
        .update_rack_metadata(
            rack.id
                .ok_or(RpcDataConversionError::MissingArgument("rack.id"))?,
            metadata,
            rack.version,
        )
        .await?;
    Ok(())
}

async fn metadata_from_expected_rack(
    api_client: &ApiClient,
    cmd: RackMetadataCommandFromExpectedRack,
) -> CarbideCliResult<()> {
    let mut racks = api_client.get_one_rack(cmd.rack.clone()).await?.racks;
    if racks.len() != 1 {
        return Err(CarbideCliError::GenericError(format!(
            "Rack with ID {} was not found",
            cmd.rack
        )));
    }
    let rack = racks.remove(0);

    let mut metadata = rack.metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Rack does not carry Metadata that can be patched".into())
    })?;

    let expected_racks = api_client.0.get_all_expected_racks().await?.expected_racks;
    let expected_rack = expected_racks
        .into_iter()
        .find(|er| er.rack_id.as_ref() == Some(&cmd.rack))
        .ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "No expected Rack found for Rack with ID {}",
                cmd.rack
            ))
        })?;

    let expected_rack_metadata = expected_rack.metadata.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "No expected Rack Metadata found for Rack with ID {}",
            cmd.rack
        ))
    })?;

    if cmd.replace_all {
        metadata.name = if expected_rack_metadata.name.is_empty() {
            rack.id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        } else {
            expected_rack_metadata.name
        };
        metadata.description = expected_rack_metadata.description;
        metadata.labels = expected_rack_metadata.labels;
    } else {
        if !expected_rack_metadata.name.is_empty()
            && (metadata.name.is_empty() || metadata.name == cmd.rack.to_string())
        {
            metadata.name = expected_rack_metadata.name;
        };
        if !expected_rack_metadata.description.is_empty() && metadata.description.is_empty() {
            metadata.description = expected_rack_metadata.description;
        };
        for label in expected_rack_metadata.labels {
            if !metadata.labels.iter().any(|l| l.key == label.key) {
                metadata.labels.push(label);
            }
        }
    }

    api_client
        .update_rack_metadata(
            rack.id
                .ok_or(RpcDataConversionError::MissingArgument("rack.id"))?,
            metadata,
            rack.version,
        )
        .await?;
    Ok(())
}
