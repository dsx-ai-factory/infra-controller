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

use ::rpc::errors::RpcDataConversionError;
use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use model::machine::status::managed_dpu_machine_ids;
use model::nic_firmware::{
    NicFirmwareCardSelection, NicFirmwareHardware, NicFirmwareProfileConfig, NicFirmwareProfileId,
    NicFirmwareSelections, resolve_nic_firmware,
};
use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data_redacted};
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateNicFirmwareProfileRequest>,
) -> Result<Response<rpc::NicFirmwareProfileResponse>, Status> {
    let request = request.into_inner();
    // Exclude artifact sources, including rejected URLs, from request logs.
    log_request_data_redacted(format!(
        "id: {}, config_present: {}, entries: {}",
        request.id.escape_default(),
        request.config.is_some(),
        request
            .config
            .as_ref()
            .map_or(0, |config| config.entries.len()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let config = required_config(request.config)?;
    let mut txn = api.txn_begin().await?;
    let profile = db::nic_firmware::create(&mut txn, &id, &config)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, config_version = %profile.version, "Created NIC firmware profile");
    Ok(Response::new(rpc::NicFirmwareProfileResponse {
        profile: Some(profile.into()),
    }))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateNicFirmwareProfileRequest>,
) -> Result<Response<rpc::NicFirmwareProfileResponse>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "id: {}, if_version_match: {:?}, config_present: {}, entries: {}",
        request.id.escape_default(),
        request
            .if_version_match
            .as_deref()
            .map(|version| version.escape_default().to_string()),
        request.config.is_some(),
        request
            .config
            .as_ref()
            .map_or(0, |config| config.entries.len()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let config = required_config(request.config)?;
    let expected_version = required_version(request.if_version_match)?;
    let mut txn = api.txn_begin().await?;
    let profile = db::nic_firmware::update(&mut txn, &id, &config, expected_version)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, config_version = %profile.version, "Updated NIC firmware profile");
    Ok(Response::new(rpc::NicFirmwareProfileResponse {
        profile: Some(profile.into()),
    }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteNicFirmwareProfileRequest>,
) -> Result<Response<()>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "id: {}, if_version_match: {:?}",
        request.id.escape_default(),
        request
            .if_version_match
            .as_deref()
            .map(|version| version.escape_default().to_string()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let expected_version = required_version(request.if_version_match)?;
    let mut txn = api.txn_begin().await?;
    db::nic_firmware::delete(&mut txn, &id, expected_version)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, "Deleted NIC firmware profile");
    Ok(Response::new(()))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareProfileIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareProfileIdsResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let hardware = request
        .into_inner()
        .hardware
        .map(NicFirmwareHardware::try_from)
        .transpose()
        .map_err(CarbideError::from)?;
    let ids = match hardware {
        Some(hardware) => db::nic_firmware::find_compatible_ids(api.pg_pool(), &hardware).await,
        None => db::nic_firmware::find_ids(api.pg_pool()).await,
    }
    .map_err(CarbideError::from)?;
    Ok(Response::new(rpc::FindNicFirmwareProfileIdsResponse {
        profile_ids: ids.into_iter().map(|id| id.to_string()).collect(),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareProfilesByIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareProfilesByIdsResponse>, Status> {
    let ids = request.into_inner().profile_ids;
    log_request_data_redacted(format!("profile_ids count: {}", ids.len()));
    let limit = api.runtime_config.max_find_by_ids as usize;
    if ids.is_empty() || ids.len() > limit {
        return Err(CarbideError::InvalidArgument(format!(
            "between 1 and {limit} profile IDs must be provided"
        ))
        .into());
    }
    let ids = ids
        .iter()
        .map(|id| id.parse())
        .collect::<Result<Vec<NicFirmwareProfileId>, _>>()
        .map_err(CarbideError::from)?;
    let profiles = db::nic_firmware::find_by_ids(api.pg_pool(), &ids)
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(rpc::FindNicFirmwareProfilesByIdsResponse {
        profiles: profiles.into_iter().map(Into::into).collect(),
    }))
}

/// `set_site_selection` creates, replaces or explicitly clears one site selection.
/// It never schedules firmware or changes a machine's state.
pub(crate) async fn set_site_selection(
    api: &Api,
    request: Request<rpc::SetNicFirmwareSiteSelectionRequest>,
) -> Result<Response<rpc::NicFirmwareSiteSelectionResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let request = request.into_inner();
    let hardware = NicFirmwareHardware::try_from(
        request
            .hardware
            .ok_or(CarbideError::MissingArgument("hardware"))?,
    )
    .map_err(CarbideError::from)?;
    let version = request
        .if_version_match
        .map(|version| required_version(Some(version)))
        .transpose()?;
    let profile_id: Option<NicFirmwareProfileId> = match request
        .action
        .ok_or(CarbideError::MissingArgument("action"))?
    {
        rpc::set_nic_firmware_site_selection_request::Action::ProfileId(id) => {
            Some(id.parse().map_err(CarbideError::from)?)
        }
        rpc::set_nic_firmware_site_selection_request::Action::Clear(()) => None,
    };
    let mut txn = api.txn_begin().await.map_err(CarbideError::from)?;
    let site_selection = match (profile_id, version) {
        (Some(id), version) => Some(
            db::nic_firmware::set_site_selection(&mut txn, &hardware, &id, version)
                .await
                .map_err(CarbideError::from)?,
        ),
        (None, Some(version)) => {
            db::nic_firmware::clear_site_selection(&mut txn, &hardware, version)
                .await
                .map_err(CarbideError::from)?;
            None
        }
        (None, None) => return Err(CarbideError::MissingArgument("if_version_match").into()),
    };
    txn.commit().await.map_err(CarbideError::from)?;
    Ok(Response::new(rpc::NicFirmwareSiteSelectionResponse {
        site_selection: site_selection.map(Into::into),
    }))
}

/// `find_site_selection_ids` lists hardware keys, optionally selected by profile.
pub(crate) async fn find_site_selection_ids(
    api: &Api,
    request: Request<rpc::NicFirmwareSiteSelectionSearchFilter>,
) -> Result<Response<rpc::FindNicFirmwareSiteSelectionIdsResponse>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "profile_id: {:?}",
        request
            .profile_id
            .as_deref()
            .map(|id| id.escape_default().to_string()),
    ));
    let profile_id: Option<NicFirmwareProfileId> = request
        .profile_id
        .map(|id| id.parse())
        .transpose()
        .map_err(CarbideError::from)?;
    let hardware = db::nic_firmware::find_site_selection_ids(api.pg_pool(), profile_id.as_ref())
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(
        rpc::FindNicFirmwareSiteSelectionIdsResponse {
            hardware: hardware.into_iter().map(Into::into).collect(),
        },
    ))
}

/// `find_site_selections_by_ids` reads site selections for a bounded hardware batch.
pub(crate) async fn find_site_selections_by_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareSiteSelectionsByIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareSiteSelectionsByIdsResponse>, Status> {
    let hardware = request.into_inner().hardware;
    log_request_data_redacted(format!("hardware count: {}", hardware.len()));
    let limit = api.runtime_config.max_find_by_ids as usize;
    if hardware.is_empty() || hardware.len() > limit {
        return Err(CarbideError::InvalidArgument(format!(
            "between 1 and {limit} hardware pairs must be provided"
        ))
        .into());
    }
    let hardware = hardware
        .into_iter()
        .map(NicFirmwareHardware::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(CarbideError::from)?;
    let site_selections = db::nic_firmware::find_site_selections_by_ids(api.pg_pool(), &hardware)
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(
        rpc::FindNicFirmwareSiteSelectionsByIdsResponse {
            site_selections: site_selections.into_iter().map(Into::into).collect(),
        },
    ))
}

/// `set_machine_config` edits machine and card profile selections together.
/// An omitted card list preserves saved selections; an empty list clears them.
pub(crate) async fn set_machine_config(
    api: &Api,
    request: Request<rpc::SetMachineNicFirmwareConfigRequest>,
) -> Result<Response<()>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let request = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let expected_version = required_version(request.if_version_match)?;
    let profile_id: Option<NicFirmwareProfileId> = match request
        .action
        .ok_or(CarbideError::MissingArgument("action"))?
    {
        rpc::set_machine_nic_firmware_config_request::Action::ProfileId(id) => {
            Some(id.parse().map_err(CarbideError::from)?)
        }
        rpc::set_machine_nic_firmware_config_request::Action::Clear(()) => None,
    };
    let card_selections = request
        .card_selections
        .map(|cards| {
            if cards.selections.len() > api.runtime_config.max_find_by_ids as usize {
                return Err(CarbideError::InvalidArgument(format!(
                    "too many NIC firmware card selections; maximum is {}",
                    api.runtime_config.max_find_by_ids
                )));
            }
            cards
                .selections
                .into_iter()
                .enumerate()
                .map(|(index, card)| {
                    NicFirmwareCardSelection::try_from(card).map_err(|error| {
                        CarbideError::InvalidArgument(format!("card_selections[{index}]: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut txn = api.txn_begin().await.map_err(CarbideError::from)?;
    db::nic_firmware::set_machine_config(
        &mut txn,
        &machine_id,
        profile_id.as_ref(),
        card_selections.as_deref(),
        expected_version,
    )
    .await
    .map_err(CarbideError::from)?;
    txn.commit().await.map_err(CarbideError::from)?;
    Ok(Response::new(()))
}

/// `get_desired` selects desired firmware for stored device observations
/// from a consistent catalog snapshot.
/// Per-device errors are returned in the report without failing the request
/// or hiding results for other devices.
/// Saved card selectors with no observed match are returned as warnings.
pub(crate) async fn get_desired(
    api: &Api,
    request: Request<rpc::GetDesiredNicFirmwareRequest>,
) -> Result<Response<rpc::GetDesiredNicFirmwareResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let request = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let allocation: Option<NicFirmwareProfileId> = request
        .allocation_profile_id
        .map(|id| id.parse())
        .transpose()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await.map_err(CarbideError::from)?;
    // Read machine settings, observations, site selections and profiles together
    // so a concurrent edit cannot mix revisions.
    let query = "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY";
    sqlx::query(query)
        .execute(&mut **txn)
        .await
        .map_err(|error| CarbideError::from(db::DatabaseError::query(query, error)))?;
    let machine = db::machine::find_one(&mut txn, &machine_id, Default::default())
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    let site_selections = db::nic_firmware::find_site_selections(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    let profile_ids: Vec<_> = site_selections
        .iter()
        .map(|site_selection| site_selection.profile_id.clone())
        .chain(allocation.iter().cloned())
        .chain(machine.config.nic_firmware_profile_id.iter().cloned())
        .chain(
            machine
                .config
                .nic_firmware_card_selections
                .iter()
                .map(|card| card.profile_id.clone()),
        )
        .collect();
    let profiles = db::nic_firmware::find_by_ids(&mut txn, &profile_ids)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await.map_err(CarbideError::from)?;
    let mut response = rpc::GetDesiredNicFirmwareResponse::default();
    let devices = machine
        .status
        .mlx_device_observation
        .as_ref()
        .map(|observation| observation.devices.as_slice())
        .unwrap_or_default();
    for card in &machine.config.nic_firmware_card_selections {
        if !devices.iter().any(|device| card.selector.matches(device)) {
            response.warnings.push(format!(
                "NIC firmware card selection for {} uses profile {} but matches no observed device",
                card.selector, card.profile_id
            ));
        }
    }
    if let Some(observation) = &machine.status.mlx_device_observation {
        response.observed_at = Some(observation.observed_at.into());
        for device in &observation.devices {
            let managed = managed_dpu_machine_ids(device.base_mac, &machine.status.interfaces);
            let mut target = rpc::NicFirmwareDeviceTarget {
                identity: Some(::rpc::protos::mlx_device::MlxDeviceIdentity {
                    device_info: Some(device.clone().into()),
                    managed_dpu_machine_ids: managed.iter().copied().map(Into::into).collect(),
                }),
                ..Default::default()
            };
            let resolution = (|| {
                if !managed.is_empty() {
                    return Err(
                        "managed-DPU association excludes this device from NIC firmware selection"
                            .to_string(),
                    );
                }
                let hardware = NicFirmwareHardware {
                    part_number: device
                        .part_number
                        .clone()
                        .ok_or("device report is missing a part number")?,
                    psid: device
                        .psid
                        .clone()
                        .ok_or("device report is missing a PSID")?,
                }
                .validate()
                .map_err(|error| error.to_string())?;
                let site_selection = site_selections
                    .iter()
                    .find(|site_selection| site_selection.hardware == hardware);
                target.site_selection_version =
                    site_selection.map(|site_selection| site_selection.version.to_string());
                let mut cards = machine
                    .config
                    .nic_firmware_card_selections
                    .iter()
                    .filter(|card| card.selector.matches(device));
                let card_selection = cards.next();
                if cards.next().is_some() {
                    return Err(
                        "multiple NIC firmware card selections match this device".to_string()
                    );
                }
                if let Some(card) = card_selection {
                    // A later report can make a previously unique selector
                    // ambiguous. Do not select one of those devices arbitrarily.
                    card.selector
                        .device_index(devices)
                        .map_err(|error| error.to_string())?;
                }
                resolve_nic_firmware(
                    &hardware,
                    &profiles,
                    NicFirmwareSelections {
                        card: card_selection.map(|card| &card.profile_id),
                        machine: machine.config.nic_firmware_profile_id.as_ref(),
                        site: site_selection.map(|site_selection| &site_selection.profile_id),
                    },
                    allocation.as_ref(),
                )
                .map_err(|error| error.to_string())
            })();
            target.result = match resolution {
                Ok(resolution) => resolution.map(|firmware| {
                    rpc::nic_firmware_device_target::Result::Firmware(firmware.into())
                }),
                Err(error) => Some(rpc::nic_firmware_device_target::Result::Error(error)),
            };
            response.targets.push(target);
        }
    }
    Ok(Response::new(response))
}

fn required_version(version: Option<String>) -> Result<ConfigVersion, CarbideError> {
    let version = version.ok_or(CarbideError::MissingArgument("if_version_match"))?;
    version
        .parse()
        .map_err(|_| RpcDataConversionError::InvalidConfigVersion(version).into())
}

fn required_config(
    config: Option<rpc::NicFirmwareProfileConfig>,
) -> Result<NicFirmwareProfileConfig, CarbideError> {
    config
        .ok_or(CarbideError::MissingArgument("config"))?
        .try_into()
        .map_err(CarbideError::from)
}
