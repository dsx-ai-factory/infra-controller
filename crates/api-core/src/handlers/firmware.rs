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

use std::collections::{HashMap, HashSet};

use ::rpc::model::firmware::firmware_component_type_from_rpc;
use ::rpc::{Timestamp, forge as rpc};
use carbide_firmware::FirmwareConfigSnapshot;
use chrono::TimeZone;
use itertools::Itertools;
use model::firmware::{
    DesiredFirmwareVersions, FirmwareComponent, FirmwareComponentType, FirmwareEntry,
    FirmwareFileArtifact, HostFirmwareConfig,
};
use regex::Regex;
use tonic::{Request, Response, Status};
use url::Url;

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_request_data_redacted};

pub(crate) async fn set_firmware_update_time_window(
    api: &Api,
    request: Request<rpc::SetFirmwareUpdateTimeWindowRequest>,
) -> Result<Response<rpc::SetFirmwareUpdateTimeWindowResponse>, Status> {
    let request = request.into_inner();
    let start = request.start_timestamp.unwrap_or_default().seconds;
    let end = request.end_timestamp.unwrap_or_default().seconds;
    // Sanity checks
    if start != 0 || end != 0 {
        if start == 0 || end == 0 {
            return Err(CarbideError::InvalidArgument(
                "start and end must both be zero or nonzero".to_string(),
            )
            .into());
        }
        if start >= end {
            return Err(CarbideError::InvalidArgument("start must precede end".to_string()).into());
        }
        if end < chrono::Utc::now().timestamp() {
            return Err(CarbideError::InvalidArgument("end occurs in the past".to_string()).into());
        }
    }

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    tracing::info!(
        start_time = ?chrono::Utc.timestamp_opt(start, 0),
        end_time = ?chrono::Utc.timestamp_opt(end, 0),
        machine_ids = ?request.machine_ids,
        "Setting firmware update time window",
    );

    db::machine::update_firmware_update_time_window_start_end(
        &request.machine_ids,
        chrono::Utc
            .timestamp_opt(request.start_timestamp.unwrap_or_default().seconds, 0)
            .earliest()
            .unwrap_or(chrono::Utc::now()),
        chrono::Utc
            .timestamp_opt(request.end_timestamp.unwrap_or_default().seconds, 0)
            .earliest()
            .unwrap_or(chrono::Utc::now()),
        &mut txn,
    )
    .await
    .map_err(crate::CarbideError::from)?;

    txn.commit().await.map_err(crate::CarbideError::from)?;

    Ok(Response::new(rpc::SetFirmwareUpdateTimeWindowResponse {}))
}

async fn effective_host_firmware_snapshot(
    api: &Api,
) -> Result<FirmwareConfigSnapshot, CarbideError> {
    let host_firmware_configs =
        db::host_firmware_config::list_configs(&api.database_connection).await?;

    Ok(api
        .runtime_config
        .get_firmware_config()
        .create_snapshot_with_overrides(host_firmware_configs))
}

pub(crate) async fn list_host_firmware(
    api: &Api,
    _request: Request<rpc::ListHostFirmwareRequest>,
) -> Result<Response<rpc::ListHostFirmwareResponse>, Status> {
    let mut ret = vec![];
    for entry in effective_host_firmware_snapshot(api).await?.into_values() {
        for (component, component_info) in entry.components {
            for firmware in component_info.known_firmware {
                if firmware.default {
                    ret.push(rpc::AvailableHostFirmware {
                        vendor: entry.vendor.to_string(),
                        model: entry.model.clone(),
                        r#type: component.to_string(),
                        inventory_name_regex: component_info
                            .current_version_reported_as
                            .clone()
                            .map(|x| x.as_str().to_string())
                            .unwrap_or("UNSPECIFIED".to_string()),
                        version: firmware.version.clone(),
                        needs_explicit_start: entry.explicit_start_needed,
                    });
                }
            }
        }
    }
    Ok(Response::new(rpc::ListHostFirmwareResponse {
        available: ret,
    }))
}

pub(crate) async fn get_desired_firmware_versions(
    api: &Api,
    request: Request<rpc::GetDesiredFirmwareVersionsRequest>,
) -> Result<Response<rpc::GetDesiredFirmwareVersionsResponse>, Status> {
    log_request_data(&request);

    let entries = load_desired_firmware_version_entries(api).await?;
    Ok(Response::new(rpc::GetDesiredFirmwareVersionsResponse {
        entries,
    }))
}

pub(super) async fn load_desired_firmware_version_entries(
    api: &Api,
) -> Result<Vec<rpc::DesiredFirmwareVersionEntry>, CarbideError> {
    effective_host_firmware_snapshot(api)
        .await?
        .into_values()
        .map(|firmware| {
            let vendor = firmware.vendor;
            let model = firmware.model.clone();
            let component_versions = DesiredFirmwareVersions::from(firmware).versions;

            Ok::<_, serde_json::Error>(rpc::DesiredFirmwareVersionEntry {
                vendor: vendor.to_string(),
                model,
                // Launder firmware.components through serde::value to convert FirmwareComponentType
                // to String (serde is configured to lowercase it.)
                component_versions: serde_json::from_value(serde_json::to_value(
                    component_versions,
                )?)?,
            })
        })
        .try_collect()
        .map_err(CarbideError::from)
}

pub(crate) async fn upsert_host_firmware_config(
    api: &Api,
    request: Request<rpc::UpsertHostFirmwareConfigRequest>,
) -> Result<Response<rpc::HostFirmwareConfigResponse>, Status> {
    log_request_data_redacted(format_upsert_host_firmware_config_request_redacted(
        request.get_ref(),
    ));

    let request = request.into_inner();
    let patch = HostFirmwareConfigPatch::from_request(&request)?;

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    let vendor = patch.vendor.to_pascalcase();
    let model = patch.model.clone();

    db::host_firmware_config::lock_for_update(&mut txn, &vendor, &model)
        .await
        .map_err(crate::CarbideError::from)?;

    let existing = db::host_firmware_config::get(&mut txn, &vendor, &model)
        .await
        .map_err(crate::CarbideError::from)?
        .map(|row| row.into_config());
    let firmware = merge_host_firmware_config_patch(existing, patch)?;
    let row = db::host_firmware_config::upsert(&mut txn, &firmware)
        .await
        .map_err(crate::CarbideError::from)?;

    txn.commit().await.map_err(crate::CarbideError::from)?;

    Ok(Response::new(host_firmware_config_response(row)))
}

pub(crate) async fn delete_host_firmware_config(
    api: &Api,
    request: Request<rpc::DeleteHostFirmwareConfigRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let vendor = parse_vendor(&request.vendor)?.to_pascalcase();
    let model = parse_host_firmware_config_model(&request.model)?;

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    db::host_firmware_config::lock_for_update(&mut txn, &vendor, &model)
        .await
        .map_err(crate::CarbideError::from)?;
    db::host_firmware_config::delete(&mut txn, &vendor, &model)
        .await
        .map_err(crate::CarbideError::from)?;

    txn.commit().await.map_err(crate::CarbideError::from)?;

    Ok(Response::new(()))
}

fn format_upsert_host_firmware_config_request_redacted(
    request: &rpc::UpsertHostFirmwareConfigRequest,
) -> String {
    let mut request = request.clone();
    for component in &mut request.components {
        for firmware in &mut component.firmware {
            for artifact in &mut firmware.artifacts {
                if !artifact.url.is_empty() {
                    artifact.url = "[REDACTED]".to_string();
                }
            }
        }
    }
    format!("{request:?}")
}

struct HostFirmwareConfigPatch {
    vendor: bmc_vendor::BMCVendor,
    model: String,
    components: HashMap<FirmwareComponentType, FirmwareComponentPatch>,
    explicit_start_needed: Option<bool>,
    ordering: Vec<FirmwareComponentType>,
}

struct FirmwareComponentPatch {
    current_version_reported_as: Regex,
    preingest_upgrade_when_below: Option<String>,
    known_firmware: Vec<FirmwareEntryPatch>,
}

struct FirmwareEntryPatch {
    entry: FirmwareEntry,
    preingestion_exclusive_config: Option<bool>,
}

impl FirmwareComponentPatch {
    fn into_component(self) -> FirmwareComponent {
        FirmwareComponent {
            current_version_reported_as: Some(self.current_version_reported_as),
            preingest_upgrade_when_below: self.preingest_upgrade_when_below,
            known_firmware: self
                .known_firmware
                .into_iter()
                .map(FirmwareEntryPatch::into_entry)
                .collect(),
        }
    }
}

impl FirmwareEntryPatch {
    fn into_entry(mut self) -> FirmwareEntry {
        self.entry.preingestion_exclusive_config =
            self.preingestion_exclusive_config.unwrap_or(false);
        self.entry
    }
}

impl HostFirmwareConfigPatch {
    fn from_request(request: &rpc::UpsertHostFirmwareConfigRequest) -> Result<Self, CarbideError> {
        let vendor = parse_vendor(&request.vendor)?;
        let model = parse_host_firmware_config_model(&request.model)?;

        let ordering = request
            .ordering
            .iter()
            .map(|component| {
                firmware_component_type_from_rpc(*component).map_err(CarbideError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut seen_components = HashSet::new();
        let mut components = HashMap::new();
        for component in &request.components {
            let component_type =
                firmware_component_type_from_rpc(component.r#type).map_err(CarbideError::from)?;
            if !seen_components.insert(component_type) {
                return Err(CarbideError::InvalidArgument(format!(
                    "duplicate firmware component type {component_type}"
                )));
            }

            let current_version_reported_as = component_regex(vendor, &model, component_type)?;
            let preingest_upgrade_when_below = parse_preingest_upgrade_when_below(
                component_type,
                component.preingest_upgrade_when_below.as_deref(),
            )?;
            let firmware_entries =
                firmware_entry_patches_from_version_configs(component_type, &component.firmware)?;

            components.insert(
                component_type,
                FirmwareComponentPatch {
                    current_version_reported_as,
                    preingest_upgrade_when_below,
                    known_firmware: firmware_entries,
                },
            );
        }

        Ok(Self {
            vendor,
            model,
            components,
            explicit_start_needed: request.explicit_start_needed,
            ordering,
        })
    }
}

fn merge_host_firmware_config_patch(
    existing: Option<HostFirmwareConfig>,
    patch: HostFirmwareConfigPatch,
) -> Result<HostFirmwareConfig, CarbideError> {
    let mut runtime_config = existing.unwrap_or_else(|| HostFirmwareConfig {
        vendor: patch.vendor,
        model: patch.model.clone(),
        components: HashMap::new(),
        explicit_start_needed: None,
        ordering: Vec::new(),
    });

    runtime_config.vendor = patch.vendor;
    runtime_config.model = patch.model;
    if let Some(explicit_start_needed) = patch.explicit_start_needed {
        runtime_config.explicit_start_needed = Some(explicit_start_needed);
    }
    if !patch.ordering.is_empty() {
        runtime_config.ordering = patch.ordering;
    }

    for (component_type, incoming_component) in patch.components {
        if let Some(existing_component) = runtime_config.components.get_mut(&component_type) {
            merge_host_firmware_component(existing_component, incoming_component);
        } else {
            runtime_config
                .components
                .insert(component_type, incoming_component.into_component());
        }
    }

    validate_host_firmware_config(&runtime_config)?;
    Ok(runtime_config)
}

// Merge an incoming component patch into an existing component. Incoming firmware
// versions are upserted by version string, omitted component fields are
// preserved, omitted version-level fields are preserved for existing versions,
// and a newly supplied default version clears any previous default for the
// component.
fn merge_host_firmware_component(
    existing_component: &mut FirmwareComponent,
    incoming_component: FirmwareComponentPatch,
) {
    existing_component.current_version_reported_as =
        Some(incoming_component.current_version_reported_as);
    if incoming_component.preingest_upgrade_when_below.is_some() {
        existing_component.preingest_upgrade_when_below =
            incoming_component.preingest_upgrade_when_below;
    }

    let incoming_sets_default = incoming_component
        .known_firmware
        .iter()
        .any(|firmware| firmware.entry.default);
    if incoming_sets_default {
        for firmware in &mut existing_component.known_firmware {
            firmware.default = false;
        }
    }

    for incoming_firmware in incoming_component.known_firmware {
        if let Some(existing_firmware) = existing_component
            .known_firmware
            .iter_mut()
            .find(|firmware| firmware.version == incoming_firmware.entry.version)
        {
            let mut incoming_entry = incoming_firmware.entry;
            if !incoming_sets_default {
                incoming_entry.default = existing_firmware.default;
            }
            incoming_entry.preingestion_exclusive_config = incoming_firmware
                .preingestion_exclusive_config
                .unwrap_or(existing_firmware.preingestion_exclusive_config);
            *existing_firmware = incoming_entry;
        } else {
            existing_component
                .known_firmware
                .push(incoming_firmware.into_entry());
        }
    }
}

// Validate the final firmware config after applying a patch. The persisted
// config must have a complete component ordering and exactly one default
// firmware version per component.
fn validate_host_firmware_config(firmware: &HostFirmwareConfig) -> Result<(), CarbideError> {
    if firmware.components.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one firmware component is required".to_string(),
        ));
    }

    if firmware.ordering.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "ordering is required".to_string(),
        ));
    }

    let mut ordered_components = HashSet::new();
    for component_type in &firmware.ordering {
        if !ordered_components.insert(*component_type) {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate firmware component type {component_type} in ordering"
            )));
        }
        if !firmware.components.contains_key(component_type) {
            return Err(CarbideError::InvalidArgument(format!(
                "ordering includes unconfigured firmware component {component_type}"
            )));
        }
    }

    for component_type in firmware.components.keys() {
        if !ordered_components.contains(component_type) {
            return Err(CarbideError::InvalidArgument(format!(
                "component {component_type} must be included in ordering"
            )));
        }
    }

    for (component_type, component) in &firmware.components {
        if component.known_firmware.is_empty() {
            return Err(CarbideError::InvalidArgument(format!(
                "component {component_type} must include at least one firmware version"
            )));
        }

        let mut seen_versions = HashSet::new();
        for firmware in &component.known_firmware {
            if firmware.version.trim().is_empty() {
                return Err(CarbideError::InvalidArgument(
                    "firmware version is required".to_string(),
                ));
            }
            if !seen_versions.insert(firmware.version.as_str()) {
                return Err(CarbideError::InvalidArgument(format!(
                    "duplicate firmware version {} for component {component_type}",
                    firmware.version
                )));
            }
        }

        let default_count = component
            .known_firmware
            .iter()
            .filter(|firmware| firmware.default)
            .count();
        if default_count != 1 {
            return Err(CarbideError::InvalidArgument(format!(
                "component {component_type} must include exactly one default firmware version"
            )));
        }
    }

    Ok(())
}

fn parse_host_firmware_config_model(model: &str) -> Result<String, CarbideError> {
    let model = model.trim();
    if model.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "model is required".to_string(),
        ));
    }
    Ok(model.to_string())
}

fn parse_vendor(vendor: &str) -> Result<bmc_vendor::BMCVendor, CarbideError> {
    let vendor = vendor.trim();
    if vendor.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "vendor is required".to_string(),
        ));
    }

    let parsed = bmc_vendor::BMCVendor::from(vendor.to_ascii_lowercase().as_str());
    if parsed == bmc_vendor::BMCVendor::Unknown {
        return Err(CarbideError::InvalidArgument(format!(
            "unknown host firmware vendor {vendor}"
        )));
    }
    Ok(parsed)
}

fn component_regex(
    vendor: bmc_vendor::BMCVendor,
    model: &str,
    component_type: FirmwareComponentType,
) -> Result<Regex, CarbideError> {
    if let Some(regex) = catalog_component_regex(vendor, model, component_type) {
        return Regex::new(regex).map_err(|error| {
            CarbideError::InvalidArgument(format!(
                "invalid current_version_reported_as for {vendor} {model} {component_type}: {error}"
            ))
        });
    }

    Err(CarbideError::InvalidArgument(format!(
        "no current_version_reported_as mapping found for {vendor} {model} {component_type}"
    )))
}

fn catalog_component_regex(
    vendor: bmc_vendor::BMCVendor,
    model: &str,
    component_type: FirmwareComponentType,
) -> Option<&'static str> {
    match vendor {
        bmc_vendor::BMCVendor::Dell => match component_type {
            FirmwareComponentType::Bmc => Some("^Installed-.*__iDRAC."),
            FirmwareComponentType::Uefi => Some("^Installed-.*__BIOS.Setup."),
            FirmwareComponentType::CpldMb => Some("^Installed-.*__CPLD.Embedded."),
            _ => None,
        },
        bmc_vendor::BMCVendor::Lenovo | bmc_vendor::BMCVendor::LenovoAMI => {
            if model_matches(model, "ThinkSystem HS350X V3")
                && component_type == FirmwareComponentType::Bmc
            {
                return Some("BMCImage1");
            }

            match component_type {
                FirmwareComponentType::Bmc => Some("^BMC-Primary"),
                FirmwareComponentType::Uefi => Some("^UEFI"),
                _ => None,
            }
        }
        bmc_vendor::BMCVendor::Nvidia => nvidia_component_regex(model, component_type),
        _ => None,
    }
}

fn nvidia_component_regex(
    model: &str,
    component_type: FirmwareComponentType,
) -> Option<&'static str> {
    if model_matches(model, "DGXH100") {
        match component_type {
            FirmwareComponentType::CombinedBmcUefi => Some("^HostBMC_0$"),
            FirmwareComponentType::Cx7 => Some("^CX7_[0-9]+$"),
            FirmwareComponentType::HGXBmc => Some("^HGX_FW_BMC_0$"),
            FirmwareComponentType::Uefi => Some("^HostBIOS_0$"),
            _ => None,
        }
    } else if model_matches(model, "GB200 NVL") {
        match component_type {
            FirmwareComponentType::Bmc => Some("^FW_BMC_0$"),
            FirmwareComponentType::HGXBmc => Some("^HGX_FW_GPU_0$"),
            FirmwareComponentType::Uefi => Some("^HGX_FW_CPU_0$"),
            _ => None,
        }
    } else {
        None
    }
}

fn model_matches(model: &str, expected: &str) -> bool {
    normalized_model_name(model) == normalized_model_name(expected)
}

fn normalized_model_name(model: &str) -> String {
    model
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn firmware_entry_patches_from_version_configs(
    component_type: FirmwareComponentType,
    firmware_versions: &[rpc::HostFirmwareVersionConfig],
) -> Result<Vec<FirmwareEntryPatch>, CarbideError> {
    if firmware_versions.is_empty() {
        return Err(CarbideError::InvalidArgument(format!(
            "component {component_type} must include at least one firmware version"
        )));
    }

    let default_count = firmware_versions
        .iter()
        .filter(|version| version.default)
        .count();

    if default_count > 1 {
        return Err(CarbideError::InvalidArgument(format!(
            "component {component_type} must include at most one default firmware version"
        )));
    }

    let mut seen_versions = HashSet::new();
    let mut entries = Vec::with_capacity(firmware_versions.len());
    for version in firmware_versions {
        let version_string = version.version.trim();
        if version_string.is_empty() {
            return Err(CarbideError::InvalidArgument(
                "firmware version is required".to_string(),
            ));
        }
        if !seen_versions.insert(version_string.to_string()) {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate firmware version {version_string} for component {component_type}"
            )));
        }
        if version.artifacts.is_empty() {
            return Err(CarbideError::InvalidArgument(format!(
                "firmware version {version_string} for component {component_type} must include at least one artifact URL"
            )));
        }

        let mut files = Vec::with_capacity(version.artifacts.len());
        for artifact in &version.artifacts {
            let url = artifact.url.trim();
            if url.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "artifact url for firmware version {version_string} must not be empty"
                )));
            }
            validate_firmware_artifact_url(version_string, url)?;

            files.push(FirmwareFileArtifact {
                filename: None,
                url: Some(url.to_string()),
                sha256: parse_optional_sha256(artifact.sha256.as_deref())?.unwrap_or_default(),
            });
        }

        entries.push(FirmwareEntryPatch {
            entry: FirmwareEntry {
                version: version_string.to_string(),
                default: version.default,
                install_only_specified: version.install_only_specified,
                power_drains_needed: version.power_drains_needed,
                pre_update_resets: version.pre_update_resets,
                files,
                ..Default::default()
            },
            preingestion_exclusive_config: version.preingestion_exclusive_config,
        });
    }

    Ok(entries)
}

fn validate_firmware_artifact_url(version: &str, url: &str) -> Result<(), CarbideError> {
    let parsed = Url::parse(url).map_err(|error| {
        CarbideError::InvalidArgument(format!(
            "artifact url for firmware version {version} is invalid: {error}"
        ))
    })?;

    if parsed.scheme() != "https" {
        return Err(CarbideError::InvalidArgument(format!(
            "artifact url for firmware version {version} must use https"
        )));
    }

    Ok(())
}

fn parse_preingest_upgrade_when_below(
    component_type: FirmwareComponentType,
    value: Option<&str>,
) -> Result<Option<String>, CarbideError> {
    value
        .map(str::trim)
        .map(|value| {
            if value.is_empty() {
                Err(CarbideError::InvalidArgument(format!(
                    "preingest_upgrade_when_below for component {component_type} must not be empty"
                )))
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

fn parse_optional_sha256(value: Option<&str>) -> Result<Option<String>, CarbideError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "sha256 must not be empty when provided".to_string(),
        ));
    }
    let value = value.to_ascii_lowercase();
    let decoded = hex::decode(&value)
        .map_err(|error| CarbideError::InvalidArgument(format!("invalid sha256: {error}")))?;
    if decoded.len() != 32 {
        return Err(CarbideError::InvalidArgument(
            "sha256 must decode to 32 bytes".to_string(),
        ));
    }
    Ok(Some(value))
}

fn host_firmware_config_response(
    row: db::host_firmware_config::HostFirmwareConfigRow,
) -> rpc::HostFirmwareConfigResponse {
    let created_at = row.created_at;
    let updated_at = row.updated_at;
    let config = row.into_config();

    rpc::HostFirmwareConfigResponse {
        vendor: config.vendor.to_pascalcase(),
        model: config.model,
        components: config
            .components
            .into_iter()
            .map(
                |(component_type, component)| rpc::HostFirmwareComponentConfigResponse {
                    r#type: rpc::HostFirmwareComponentType::from(component_type) as i32,
                    current_version_reported_as: component
                        .current_version_reported_as
                        .map(|regex| regex.as_str().to_string()),
                    firmware: component
                        .known_firmware
                        .into_iter()
                        .map(host_firmware_version_config_response)
                        .collect(),
                    preingest_upgrade_when_below: component.preingest_upgrade_when_below,
                },
            )
            .collect(),
        explicit_start_needed: config.explicit_start_needed.unwrap_or(false),
        ordering: config
            .ordering
            .into_iter()
            .map(|component_type| rpc::HostFirmwareComponentType::from(component_type) as i32)
            .collect(),
        created_at: Some(Timestamp::from(created_at)),
        updated_at: Some(Timestamp::from(updated_at)),
    }
}

fn host_firmware_version_config_response(
    firmware: FirmwareEntry,
) -> rpc::HostFirmwareVersionConfig {
    rpc::HostFirmwareVersionConfig {
        version: firmware.version,
        default: firmware.default,
        artifacts: firmware
            .files
            .into_iter()
            .map(|artifact| rpc::HostFirmwareArtifact {
                url: artifact.url.unwrap_or_default(),
                sha256: if artifact.sha256.is_empty() {
                    None
                } else {
                    Some(artifact.sha256)
                },
            })
            .collect(),
        install_only_specified: firmware.install_only_specified,
        power_drains_needed: firmware.power_drains_needed,
        pre_update_resets: firmware.pre_update_resets,
        preingestion_exclusive_config: Some(firmware.preingestion_exclusive_config),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn component_regex_uses_catalog_mapping() {
        let dell_bmc_regex = component_regex(
            bmc_vendor::BMCVendor::Dell,
            "poweredge_r760",
            FirmwareComponentType::Bmc,
        )
        .unwrap();
        assert_eq!(dell_bmc_regex.as_str(), "^Installed-.*__iDRAC.");

        let cx7_regex = component_regex(
            bmc_vendor::BMCVendor::Nvidia,
            "DGXH100",
            FirmwareComponentType::Cx7,
        )
        .unwrap();
        assert!(cx7_regex.is_match("CX7_0"));

        let cpld_regex = component_regex(
            bmc_vendor::BMCVendor::Dell,
            "poweredge_r760",
            FirmwareComponentType::CpldMb,
        )
        .unwrap();
        assert_eq!(cpld_regex.as_str(), "^Installed-.*__CPLD.Embedded.");

        let lenovo_ami_regex = component_regex(
            bmc_vendor::BMCVendor::LenovoAMI,
            "ThinkSystem SR650 V2",
            FirmwareComponentType::Bmc,
        )
        .unwrap();
        assert_eq!(lenovo_ami_regex.as_str(), "^BMC-Primary");
    }

    #[test]
    fn component_regex_rejects_missing_catalog_mapping() {
        assert!(
            component_regex(
                bmc_vendor::BMCVendor::Dell,
                "PowerEdge R750",
                FirmwareComponentType::Cx7,
            )
            .is_err()
        );
    }

    #[test]
    fn parse_host_firmware_config_model_trims_and_rejects_empty_values() {
        scenarios!(run = |input| parse_host_firmware_config_model(input).map_err(drop);
            "valid models" {
                "DGXH100" => Yields("DGXH100".to_string()),
                " dgxh100 " => Yields("dgxh100".to_string()),
            }

            "invalid models" {
                "" => Fails,
                "   " => Fails,
            }
        );
    }

    #[test]
    fn parse_vendor_trims_and_rejects_invalid_values() {
        scenarios!(run = |input| parse_vendor(input).map_err(drop);
            "trimmed canonical vendors are accepted" {
                " Dell " => Yields(bmc_vendor::BMCVendor::Dell),
            }

            "invalid vendors are rejected" {
                "   " => Fails,
                "Acme" => Fails,
            }
        );
    }

    #[test]
    fn parse_preingest_upgrade_when_below_trims_and_rejects_empty_values() {
        assert_eq!(
            parse_preingest_upgrade_when_below(FirmwareComponentType::Bmc, Some(" 7.20.10.50 "))
                .unwrap(),
            Some("7.20.10.50".to_string())
        );
        assert!(parse_preingest_upgrade_when_below(FirmwareComponentType::Bmc, Some(" ")).is_err());
        assert_eq!(
            parse_preingest_upgrade_when_below(FirmwareComponentType::Bmc, None).unwrap(),
            None
        );
    }

    #[test]
    fn parse_optional_sha256_validates_length() {
        let valid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_optional_sha256(Some(valid)).unwrap(),
            Some(valid.to_string())
        );
        assert!(parse_optional_sha256(Some("abc")).is_err());
        assert_eq!(parse_optional_sha256(None).unwrap(), None);
    }

    #[test]
    fn validate_firmware_artifact_url_requires_https() {
        validate_firmware_artifact_url("1.0.0", "https://firmware.example.invalid/fw.bin").unwrap();

        assert!(
            validate_firmware_artifact_url("1.0.0", "http://firmware.example.invalid/fw.bin")
                .is_err()
        );
        assert!(validate_firmware_artifact_url("1.0.0", "file:///tmp/fw.bin").is_err());
        assert!(validate_firmware_artifact_url("1.0.0", "not a url").is_err());
    }

    #[test]
    fn upsert_host_firmware_config_request_log_redacts_artifact_urls() {
        let request = rpc::UpsertHostFirmwareConfigRequest {
            vendor: "Dell".to_string(),
            model: "PowerEdge R760".to_string(),
            components: vec![rpc::UpsertHostFirmwareComponentConfig {
                r#type: rpc::HostFirmwareComponentType::Bmc as i32,
                firmware: vec![rpc::HostFirmwareVersionConfig {
                    version: "1.0.0".to_string(),
                    default: true,
                    artifacts: vec![rpc::HostFirmwareArtifact {
                        url: "https://firmware.example.invalid/fw.bin?token=secret".to_string(),
                        sha256: Some("checksum-present".to_string()),
                    }],
                    install_only_specified: false,
                    power_drains_needed: None,
                    pre_update_resets: false,
                    preingestion_exclusive_config: None,
                }],
                preingest_upgrade_when_below: Some("1.0.0".to_string()),
            }],
            explicit_start_needed: Some(true),
            ordering: vec![rpc::HostFirmwareComponentType::Bmc as i32],
        };

        let logged = format_upsert_host_firmware_config_request_redacted(&request);

        assert!(logged.contains("[REDACTED]"));
        assert!(!logged.contains("firmware.example.invalid"));
        assert!(!logged.contains("token=secret"));
        assert!(logged.contains("PowerEdge R760"));
        assert!(logged.contains("1.0.0"));
    }

    #[test]
    fn firmware_entries_preserve_artifact_urls_and_optional_sha256() {
        let valid_sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let entries = firmware_entry_patches_from_version_configs(
            FirmwareComponentType::Cx7,
            &[rpc::HostFirmwareVersionConfig {
                version: "28.47.2682".to_string(),
                default: true,
                artifacts: vec![rpc::HostFirmwareArtifact {
                    url: "https://firmware.example.invalid/cx7/fw.bin".to_string(),
                    sha256: Some(valid_sha.to_string()),
                }],
                install_only_specified: true,
                power_drains_needed: Some(1),
                pre_update_resets: true,
                preingestion_exclusive_config: Some(true),
            }],
        )
        .unwrap();

        assert_eq!(entries[0].preingestion_exclusive_config, Some(true));
        let entry = &entries[0].entry;
        assert_eq!(entry.filename, None);
        assert!(entry.filenames.is_empty());
        assert_eq!(entry.url, None);
        assert_eq!(
            entry.files[0].url.as_deref(),
            Some("https://firmware.example.invalid/cx7/fw.bin")
        );
        assert_eq!(entry.files[0].filename, None);
        assert_eq!(entry.files[0].sha256, valid_sha);
        assert!(entry.install_only_specified);
        assert_eq!(entry.power_drains_needed, Some(1));
        assert!(entry.pre_update_resets);
    }

    #[test]
    fn firmware_entries_reject_duplicate_versions() {
        assert!(
            firmware_entry_patches_from_version_configs(
                FirmwareComponentType::Cx7,
                &[
                    test_version_config("28.47.2682", true),
                    test_version_config("28.47.2682", false),
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn firmware_entries_reject_multiple_defaults() {
        assert!(
            firmware_entry_patches_from_version_configs(
                FirmwareComponentType::Cx7,
                &[
                    test_version_config("28.47.2682", true),
                    test_version_config("28.48.1111", true),
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn firmware_entries_allow_no_default_in_patch() {
        let entries = firmware_entry_patches_from_version_configs(
            FirmwareComponentType::Cx7,
            &[test_version_config("28.47.2682", false)],
        )
        .unwrap();

        assert!(!entries[0].entry.default);
    }

    #[test]
    fn merge_host_firmware_config_adds_new_version_without_removing_existing_versions() {
        let existing = test_firmware(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![
                    test_entry(
                        "7.10",
                        false,
                        "https://firmware.example.invalid/bmc-7.10.bin",
                    ),
                    test_entry(
                        "7.20",
                        true,
                        "https://firmware.example.invalid/bmc-7.20.bin",
                    ),
                ],
            ),
        )]));
        let patch = test_patch(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.30",
                    true,
                    "https://firmware.example.invalid/bmc-7.30.bin",
                )],
            ),
        )]));

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();
        let bmc = merged
            .components
            .get(&FirmwareComponentType::Bmc)
            .expect("bmc component");

        assert_eq!(
            bmc.known_firmware
                .iter()
                .map(|firmware| (firmware.version.as_str(), firmware.default))
                .collect::<Vec<_>>(),
            vec![("7.10", false), ("7.20", false), ("7.30", true)]
        );
    }

    #[test]
    fn merge_host_firmware_config_replaces_existing_version_and_preserves_default() {
        let existing = test_firmware(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.20",
                    true,
                    "https://firmware.example.invalid/old-bmc.bin",
                )],
            ),
        )]));
        let patch = test_patch(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.20",
                    false,
                    "https://firmware.example.invalid/new-bmc.bin",
                )],
            ),
        )]));

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();
        let firmware = &merged.components[&FirmwareComponentType::Bmc].known_firmware[0];

        assert_eq!(firmware.version, "7.20");
        assert!(firmware.default);
        assert_eq!(
            firmware.files[0].url.as_deref(),
            Some("https://firmware.example.invalid/new-bmc.bin")
        );
    }

    #[test]
    fn merge_host_firmware_config_preserves_preingestion_exclusive_config_when_omitted() {
        let mut existing_entry =
            test_entry("7.20", true, "https://firmware.example.invalid/old-bmc.bin");
        existing_entry.preingestion_exclusive_config = true;
        let existing = test_firmware(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component("^FW_BMC_0$", vec![existing_entry]),
        )]));
        let patch = HostFirmwareConfigPatch {
            components: HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component_patch_with_preingestion_config(
                    "^FW_BMC_0$",
                    vec![(
                        test_entry(
                            "7.20",
                            false,
                            "https://firmware.example.invalid/new-bmc.bin",
                        ),
                        None,
                    )],
                ),
            )]),
            ..test_patch(HashMap::new())
        };

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();
        let firmware = &merged.components[&FirmwareComponentType::Bmc].known_firmware[0];

        assert!(firmware.default);
        assert!(firmware.preingestion_exclusive_config);
        assert_eq!(
            firmware.files[0].url.as_deref(),
            Some("https://firmware.example.invalid/new-bmc.bin")
        );
    }

    #[test]
    fn merge_host_firmware_config_clears_preingestion_exclusive_config_when_false() {
        let mut existing_entry =
            test_entry("7.20", true, "https://firmware.example.invalid/old-bmc.bin");
        existing_entry.preingestion_exclusive_config = true;
        let existing = test_firmware(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component("^FW_BMC_0$", vec![existing_entry]),
        )]));
        let patch = HostFirmwareConfigPatch {
            components: HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component_patch_with_preingestion_config(
                    "^FW_BMC_0$",
                    vec![(
                        test_entry(
                            "7.20",
                            false,
                            "https://firmware.example.invalid/new-bmc.bin",
                        ),
                        Some(false),
                    )],
                ),
            )]),
            ..test_patch(HashMap::new())
        };

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();
        let firmware = &merged.components[&FirmwareComponentType::Bmc].known_firmware[0];

        assert!(firmware.default);
        assert!(!firmware.preingestion_exclusive_config);
    }

    #[test]
    fn merge_host_firmware_config_preserves_omitted_components() {
        let existing = test_firmware(HashMap::from([
            (
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.20",
                        true,
                        "https://firmware.example.invalid/bmc.bin",
                    )],
                ),
            ),
            (
                FirmwareComponentType::Uefi,
                test_component(
                    "^HostBIOS_0$",
                    vec![test_entry(
                        "1.0.0",
                        true,
                        "https://firmware.example.invalid/uefi.bin",
                    )],
                ),
            ),
        ]));
        let patch = test_patch(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.30",
                    true,
                    "https://firmware.example.invalid/bmc-7.30.bin",
                )],
            ),
        )]));

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();

        assert!(merged.components.contains_key(&FirmwareComponentType::Bmc));
        assert!(merged.components.contains_key(&FirmwareComponentType::Uefi));
    }

    #[test]
    fn merge_host_firmware_config_preserves_optional_fields_when_omitted() {
        let existing = HostFirmwareConfig {
            explicit_start_needed: Some(true),
            ordering: vec![FirmwareComponentType::Bmc],
            ..test_firmware(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.20",
                        true,
                        "https://firmware.example.invalid/bmc.bin",
                    )],
                ),
            )]))
        };
        let patch = test_patch(HashMap::new());

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();

        assert_eq!(merged.explicit_start_needed, Some(true));
        assert_eq!(merged.ordering, vec![FirmwareComponentType::Bmc]);
    }

    #[test]
    fn merge_host_firmware_config_updates_optional_fields_when_present() {
        let existing = HostFirmwareConfig {
            explicit_start_needed: Some(true),
            ordering: vec![FirmwareComponentType::Uefi, FirmwareComponentType::Bmc],
            ..test_firmware(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.20",
                        true,
                        "https://firmware.example.invalid/bmc.bin",
                    )],
                ),
            )]))
        };
        let patch = HostFirmwareConfigPatch {
            explicit_start_needed: Some(false),
            ordering: vec![FirmwareComponentType::Bmc],
            ..test_patch(HashMap::new())
        };

        let merged = merge_host_firmware_config_patch(Some(existing), patch).unwrap();

        assert_eq!(merged.explicit_start_needed, Some(false));
        assert_eq!(merged.ordering, vec![FirmwareComponentType::Bmc]);
    }

    #[test]
    fn merge_host_firmware_config_keeps_omitted_explicit_start_absent_on_create() {
        let patch = HostFirmwareConfigPatch {
            ordering: vec![FirmwareComponentType::Bmc],
            ..test_patch(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.30",
                        true,
                        "https://firmware.example.invalid/bmc-7.30.bin",
                    )],
                ),
            )]))
        };

        let merged = merge_host_firmware_config_patch(None, patch).unwrap();

        assert_eq!(merged.explicit_start_needed, None);
    }

    #[test]
    fn merge_host_firmware_config_rejects_new_component_without_default() {
        let patch = HostFirmwareConfigPatch {
            ordering: vec![FirmwareComponentType::Bmc],
            ..test_patch(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.30",
                        false,
                        "https://firmware.example.invalid/bmc-7.30.bin",
                    )],
                ),
            )]))
        };

        assert!(merge_host_firmware_config_patch(None, patch).is_err());
    }

    #[test]
    fn merge_host_firmware_config_rejects_create_without_ordering() {
        let patch = test_patch(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.30",
                    true,
                    "https://firmware.example.invalid/bmc-7.30.bin",
                )],
            ),
        )]));

        assert!(merge_host_firmware_config_patch(None, patch).is_err());
    }

    #[test]
    fn merge_host_firmware_config_rejects_added_component_without_ordering_update() {
        let existing = test_firmware(HashMap::from([(
            FirmwareComponentType::Bmc,
            test_component(
                "^FW_BMC_0$",
                vec![test_entry(
                    "7.20",
                    true,
                    "https://firmware.example.invalid/bmc.bin",
                )],
            ),
        )]));
        let patch = test_patch(HashMap::from([(
            FirmwareComponentType::Uefi,
            test_component(
                "^HostBIOS_0$",
                vec![test_entry(
                    "1.0.0",
                    true,
                    "https://firmware.example.invalid/uefi.bin",
                )],
            ),
        )]));

        assert!(merge_host_firmware_config_patch(Some(existing), patch).is_err());
    }

    #[test]
    fn merge_host_firmware_config_rejects_ordering_that_omits_component() {
        let patch = HostFirmwareConfigPatch {
            ordering: vec![FirmwareComponentType::Bmc],
            ..test_patch(HashMap::from([
                (
                    FirmwareComponentType::Bmc,
                    test_component(
                        "^FW_BMC_0$",
                        vec![test_entry(
                            "7.30",
                            true,
                            "https://firmware.example.invalid/bmc-7.30.bin",
                        )],
                    ),
                ),
                (
                    FirmwareComponentType::Uefi,
                    test_component(
                        "^HostBIOS_0$",
                        vec![test_entry(
                            "1.0.0",
                            true,
                            "https://firmware.example.invalid/uefi.bin",
                        )],
                    ),
                ),
            ]))
        };

        assert!(merge_host_firmware_config_patch(None, patch).is_err());
    }

    #[test]
    fn merge_host_firmware_config_rejects_ordering_for_unconfigured_component() {
        let patch = HostFirmwareConfigPatch {
            ordering: vec![FirmwareComponentType::Bmc, FirmwareComponentType::Uefi],
            ..test_patch(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.30",
                        true,
                        "https://firmware.example.invalid/bmc-7.30.bin",
                    )],
                ),
            )]))
        };

        assert!(merge_host_firmware_config_patch(None, patch).is_err());
    }

    #[test]
    fn merge_host_firmware_config_rejects_duplicate_ordering_entries() {
        let patch = HostFirmwareConfigPatch {
            ordering: vec![FirmwareComponentType::Bmc, FirmwareComponentType::Bmc],
            ..test_patch(HashMap::from([(
                FirmwareComponentType::Bmc,
                test_component(
                    "^FW_BMC_0$",
                    vec![test_entry(
                        "7.30",
                        true,
                        "https://firmware.example.invalid/bmc-7.30.bin",
                    )],
                ),
            )]))
        };

        assert!(merge_host_firmware_config_patch(None, patch).is_err());
    }

    fn test_version_config(version: &str, default: bool) -> rpc::HostFirmwareVersionConfig {
        rpc::HostFirmwareVersionConfig {
            version: version.to_string(),
            default,
            artifacts: vec![rpc::HostFirmwareArtifact {
                url: format!("https://firmware.example.invalid/{version}/fw.bin"),
                sha256: None,
            }],
            install_only_specified: false,
            power_drains_needed: None,
            pre_update_resets: false,
            preingestion_exclusive_config: None,
        }
    }

    fn test_firmware(
        components: HashMap<FirmwareComponentType, FirmwareComponent>,
    ) -> HostFirmwareConfig {
        let mut ordering: Vec<_> = components.keys().copied().collect();
        ordering.sort();

        HostFirmwareConfig {
            vendor: bmc_vendor::BMCVendor::Nvidia,
            model: "DGXH100".to_string(),
            components,
            explicit_start_needed: Some(false),
            ordering,
        }
    }

    fn test_patch(
        components: HashMap<FirmwareComponentType, FirmwareComponent>,
    ) -> HostFirmwareConfigPatch {
        HostFirmwareConfigPatch {
            vendor: bmc_vendor::BMCVendor::Nvidia,
            model: "DGXH100".to_string(),
            components: components
                .into_iter()
                .map(|(component_type, component)| {
                    (component_type, test_component_patch(component))
                })
                .collect(),
            explicit_start_needed: None,
            ordering: Vec::new(),
        }
    }

    fn test_component(regex: &str, known_firmware: Vec<FirmwareEntry>) -> FirmwareComponent {
        FirmwareComponent {
            current_version_reported_as: Some(Regex::new(regex).unwrap()),
            preingest_upgrade_when_below: None,
            known_firmware,
        }
    }

    fn test_component_patch(component: FirmwareComponent) -> FirmwareComponentPatch {
        let firmware = component
            .known_firmware
            .into_iter()
            .map(|entry| {
                let preingestion_exclusive_config = Some(entry.preingestion_exclusive_config);
                FirmwareEntryPatch {
                    entry,
                    preingestion_exclusive_config,
                }
            })
            .collect();

        FirmwareComponentPatch {
            current_version_reported_as: component.current_version_reported_as.unwrap(),
            preingest_upgrade_when_below: component.preingest_upgrade_when_below,
            known_firmware: firmware,
        }
    }

    fn test_component_patch_with_preingestion_config(
        regex: &str,
        entries: Vec<(FirmwareEntry, Option<bool>)>,
    ) -> FirmwareComponentPatch {
        FirmwareComponentPatch {
            current_version_reported_as: Regex::new(regex).unwrap(),
            preingest_upgrade_when_below: None,
            known_firmware: entries
                .into_iter()
                .map(
                    |(entry, preingestion_exclusive_config)| FirmwareEntryPatch {
                        entry,
                        preingestion_exclusive_config,
                    },
                )
                .collect(),
        }
    }

    fn test_entry(version: &str, default: bool, url: &str) -> FirmwareEntry {
        FirmwareEntry {
            version: version.to_string(),
            default,
            files: vec![FirmwareFileArtifact {
                filename: None,
                url: Some(url.to_string()),
                sha256: String::new(),
            }],
            ..Default::default()
        }
    }
}
