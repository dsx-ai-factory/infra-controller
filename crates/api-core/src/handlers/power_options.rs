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

use std::str::FromStr;

use ::rpc::forge as rpc;
use db::DatabaseError;
use mac_address::MacAddress;
use model::machine::LoadSnapshotOptions;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};

pub(crate) async fn get_power_options(
    api: &Api,
    request: Request<rpc::PowerOptionRequest>,
) -> Result<Response<rpc::PowerOptionResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let mut txn = api.txn_begin().await?;
    let power_options = if req.machine_id.is_empty() {
        db::power_options::get_all(&mut txn).await
    } else {
        db::power_options::get_by_ids(&req.machine_id, &mut txn).await
    }?;

    txn.commit().await?;

    Ok(Response::new(rpc::PowerOptionResponse {
        response: power_options
            .into_iter()
            .map(|x| x.into())
            .collect::<Vec<rpc::PowerOptions>>(),
    }))
}

pub(crate) async fn update_power_option(
    api: &Api,
    request: Request<rpc::PowerOptionUpdateRequest>,
) -> Result<Response<rpc::PowerOptionResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let machine_id = req
        .machine_id
        .ok_or_else(|| CarbideError::InvalidArgument("machine ID is missing".to_string()))?;

    log_machine_id(&machine_id);

    let mut txn = api.txn_begin().await?;

    let current_power_state = db::power_options::get_by_ids(&[machine_id], &mut txn).await?;

    // This should never happen until machine is not forced-deleted or does not exist.
    let Some(current_power_options) = current_power_state.first() else {
        return Err(CarbideError::InvalidArgument("only host id is expected!!".to_string()).into());
    };

    let desired_power_state = req.power_state();

    // if desired_state == Off, maintenance must be set.
    if matches!(desired_power_state, rpc::PowerState::Off) {
        let snapshot = db::managed_host::load_snapshot(
            &mut txn,
            &machine_id,
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: false,
                host_health_config: api.runtime_config.host_health,
            },
        )
        .await?
        .ok_or(CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;

        // Start reprovisioning only if the host has an HostUpdateInProgress health alert
        let update_alert = snapshot
            .aggregate_health
            .alerts
            .iter()
            .find(|a| a.id == health_report::HealthProbeId::internal_maintenance());
        if !update_alert.is_some_and(|alert| {
            alert
                .classifications
                .contains(&health_report::HealthAlertClassification::suppress_external_alerting())
        }) {
            return Err(CarbideError::InvalidArgument(
                "machine must have a 'maintenance' health alert with 'SupressExternalAlerting' classification".into(),
            )
            .into());
        }
    }

    // To avoid unnecessary version increment.
    let desired_power_state = desired_power_state.into();
    if desired_power_state == current_power_options.desired_power_state {
        return Err(CarbideError::InvalidArgument(format!(
            "power state is already set as {desired_power_state:?}. no change is performed"
        ))
        .into());
    }

    let updated_value = db::power_options::update_desired_state(
        &machine_id,
        desired_power_state,
        &current_power_options.desired_power_state_version,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(rpc::PowerOptionResponse {
        response: vec![updated_value.into()],
    }))
}

/// Determines machine ingestion state in relation to the power on gate
/// NotDiscovered - the machine has not been discovered.
/// WaitingForIngestion - the machine is stuck at the gate, will not be powered on yet.
/// IngestionMachineCreated - the machine is past the gate and has been ingested.
/// IngestionMachineNotCreated - the machine is past the gate, but the entry
/// in the DB hasn't been created yet.
#[allow(txn_without_commit)]
pub(crate) async fn determine_machine_ingestion_state(
    api: &Api,
    request: &rpc::BmcEndpointRequest,
) -> Result<tonic::Response<rpc::MachineIngestionStateResponse>, tonic::Status> {
    // 1. Has it been ingested at all?
    //.   Look up machine interface by mac address, then find explored endpoint by ip
    //.   No explored endpoint -> NotDisovered
    // 2. If yes, do we have an entry in the explored_managed_hosts?
    //.   Yes -> Has the machine been created?
    //           Yes -> IngestionMachineCreated
    //           No -> IngestionMachineNotCreated
    //.   Nope -> WaitingForIngestion

    let mac_address = MacAddress::from_str(&request.mac_address.clone().ok_or(
        CarbideError::InvalidArgument("no MAC address suplied".to_string()),
    )?)
    .map_err(CarbideError::MacAddressParseError)?;

    let mut txn = api.database_connection.begin().await.map_err(|e| {
        CarbideError::from(DatabaseError::new(
            "begin determine_machine_ingestion_state",
            e,
        ))
    })?;

    let explored_endpoint = match find_explored_endpoint(&mut txn, &mac_address).await? {
        FindExploredEndpoint::Found(explored_endpoint) => *explored_endpoint,
        FindExploredEndpoint::NotFound => {
            return Ok(tonic::Response::new(rpc::MachineIngestionStateResponse {
                machine_ingestion_state: rpc::MachineIngestionState::NotDiscovered.into(),
            }));
        }
        FindExploredEndpoint::MoreThanOneEndpoint => {
            return Err(CarbideError::Internal {
                message: format!(
                    "More than one explored enpoint found for MAC {}",
                    mac_address
                ),
            }
            .into());
        }
    };

    // now check if we have an entry in explored_managed_hosts
    let explored_managed_hosts =
        db::explored_managed_host::find_by_ips(txn.as_mut(), vec![explored_endpoint.address])
            .await?;

    if !explored_managed_hosts.is_empty() {
        let machine_created = db::machine::find_id_by_bmc_ip(&mut txn, &explored_endpoint.address)
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?
            .is_some();
        if machine_created {
            Ok(tonic::Response::new(rpc::MachineIngestionStateResponse {
                machine_ingestion_state: rpc::MachineIngestionState::IngestionMachineCreated.into(),
            }))
        } else {
            Ok(tonic::Response::new(rpc::MachineIngestionStateResponse {
                machine_ingestion_state: rpc::MachineIngestionState::IngestionMachineNotCreated
                    .into(),
            }))
        }
    } else {
        Ok(tonic::Response::new(rpc::MachineIngestionStateResponse {
            machine_ingestion_state: rpc::MachineIngestionState::WaitingForIngestion.into(),
        }))
    }
}

pub(crate) async fn allow_ingestion_and_power_on(
    api: &Api,
    request: &rpc::BmcEndpointRequest,
) -> Result<tonic::Response<()>, tonic::Status> {
    // flip a flag in explored_endpoints and allow a power on
    let mac_address = MacAddress::from_str(&request.mac_address.clone().ok_or(
        CarbideError::InvalidArgument("no MAC address suplied".to_string()),
    )?)
    .map_err(CarbideError::MacAddressParseError)?;

    let mut txn = api.txn_begin().await?;

    let explored_endpoint = match find_explored_endpoint(&mut txn, &mac_address).await? {
        FindExploredEndpoint::Found(explored_endpoint) => *explored_endpoint,
        FindExploredEndpoint::NotFound => {
            return Err(CarbideError::NotFoundError {
                kind: "ExploredEndpoint",
                id: mac_address.to_string(), //power_on_request.mac_address.clone(),
            }
            .into());
        }
        FindExploredEndpoint::MoreThanOneEndpoint => {
            return Err(CarbideError::Internal {
                message: format!(
                    "More than one explored enpoint found for MAC {}",
                    mac_address
                ),
            }
            .into());
        }
    };

    // flip the flag now
    db::explored_endpoints::set_pause_ingestion_and_poweron(
        explored_endpoint.address,
        false,
        &mut txn,
    )
    .await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

enum FindExploredEndpoint {
    Found(Box<model::site_explorer::ExploredEndpoint>),
    NotFound,
    MoreThanOneEndpoint,
}

async fn find_explored_endpoint(
    txn: &mut sqlx::PgConnection,
    mac_address: &MacAddress,
) -> Result<FindExploredEndpoint, CarbideError> {
    let explored_endpoints = db::explored_endpoints::find_by_mac_address(txn, *mac_address).await?;

    let explored_endpoint = match explored_endpoints.len() {
        0 => return Ok(FindExploredEndpoint::NotFound),
        1 => &explored_endpoints[0],
        _ => return Ok(FindExploredEndpoint::MoreThanOneEndpoint),
    };

    Ok(FindExploredEndpoint::Found(Box::new(
        explored_endpoint.clone(),
    )))
}
