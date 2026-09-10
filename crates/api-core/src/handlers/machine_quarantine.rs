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

use ::rpc::forge as rpc;
use carbide_uuid::machine::HostMachineId;
use health_report::HealthReport;
use model::machine::network::ManagedHostQuarantineState;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn set_managed_host_quarantine_state(
    api: &Api,
    request: Request<rpc::SetManagedHostQuarantineStateRequest>,
) -> Result<Response<rpc::SetManagedHostQuarantineStateResponse>, Status> {
    log_request_data(&request);
    let rpc::SetManagedHostQuarantineStateRequest {
        quarantine_state,
        machine_id,
    } = request.into_inner();
    let machine_id = convert_and_log_machine_id(machine_id.as_ref())?;
    let Some(quarantine_state) = quarantine_state else {
        return Err(CarbideError::MissingArgument("quarantine_state").into());
    };
    let quarantine_state: ManagedHostQuarantineState =
        quarantine_state.try_into().map_err(CarbideError::from)?;

    let message = quarantine_state.reason.clone().unwrap_or_default();

    let mut txn = api.txn_begin().await?;

    let prior_quarantine_state =
        db::machine::set_quarantine_state(&mut txn, &machine_id, quarantine_state)
            .await?
            .map(Into::into);

    match db::machine::remove_health_report(
        &mut txn,
        &machine_id,
        health_report::HealthReportApplyMode::Merge,
        HealthReport::QUARANTINE_SOURCE,
    )
    .await
    .map_err(CarbideError::from)
    {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    };

    let report = HealthReport::quarantine_report(message);
    db::machine::insert_health_report(
        &mut txn,
        &machine_id,
        health_report::HealthReportApplyMode::Merge,
        &report,
        false,
    )
    .await?;

    txn.commit().await?;

    Ok(Response::new(rpc::SetManagedHostQuarantineStateResponse {
        prior_quarantine_state,
    }))
}

pub(crate) async fn get_managed_host_quarantine_state(
    api: &Api,
    request: Request<rpc::GetManagedHostQuarantineStateRequest>,
) -> Result<Response<rpc::GetManagedHostQuarantineStateResponse>, Status> {
    log_request_data(&request);
    let rpc::GetManagedHostQuarantineStateRequest { machine_id } = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(machine_id.as_ref())?;

    let quarantine_state = db::machine::get_quarantine_state(&api.database_connection, &machine_id)
        .await?
        .map(Into::into);

    Ok(Response::new(rpc::GetManagedHostQuarantineStateResponse {
        quarantine_state,
    }))
}

pub(crate) async fn clear_managed_host_quarantine_state(
    api: &Api,
    request: Request<rpc::ClearManagedHostQuarantineStateRequest>,
) -> Result<Response<rpc::ClearManagedHostQuarantineStateResponse>, Status> {
    log_request_data(&request);

    let rpc::ClearManagedHostQuarantineStateRequest { machine_id } = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    let prior_quarantine_state = db::machine::clear_quarantine_state(&mut txn, &machine_id)
        .await?
        .map(Into::into);

    match db::machine::remove_health_report(
        &mut txn,
        &machine_id,
        health_report::HealthReportApplyMode::Merge,
        HealthReport::QUARANTINE_SOURCE,
    )
    .await
    .map_err(CarbideError::from)
    {
        // For older implementation, this override is not set yet.
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    };

    txn.commit().await?;

    Ok(tonic::Response::new(
        rpc::ClearManagedHostQuarantineStateResponse {
            prior_quarantine_state,
        },
    ))
}
