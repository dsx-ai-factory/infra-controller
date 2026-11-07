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

/*!
 * gRPC handlers for measured boot mock-machine related API calls.
 */

use std::str::FromStr;

use ::rpc::errors::RpcDataConversionError;
use carbide_uuid::machine::MachineId;
use db::measured_boot::interface::machine::get_candidate_machine_records;
use rpc::protos::measured_boot::{
    AttestCandidateMachineRequest, AttestCandidateMachineResponse, ListCandidateMachinesRequest,
    ListCandidateMachinesResponse, ShowCandidateMachineRequest, ShowCandidateMachineResponse,
    ShowCandidateMachinesRequest, ShowCandidateMachinesResponse, show_candidate_machine_request,
};
use tonic::Status;

use crate::CarbideError;
use crate::api::Api;
use crate::measured_boot::convert_vec;

/// handle_attest_candidate_machine handles the AttestCandidateMachine API endpoint.
pub(crate) async fn handle_attest_candidate_machine(
    api: &Api,
    req: AttestCandidateMachineRequest,
) -> Result<AttestCandidateMachineResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let report = db::measured_boot::report::new(
        &mut txn,
        MachineId::from_str(&req.machine_id).map_err(|_| {
            CarbideError::from(RpcDataConversionError::InvalidMachineId(req.machine_id))
        })?,
        &convert_vec(req.pcr_values),
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("failed saving measurements: {e}"),
    })?;

    txn.commit().await?;
    Ok(AttestCandidateMachineResponse {
        report: Some(report.into()),
    })
}

/// handle_show_candidate_machine handles the ShowCandidateMachine API endpoint.
pub(crate) async fn handle_show_candidate_machine(
    api: &Api,
    req: ShowCandidateMachineRequest,
) -> Result<ShowCandidateMachineResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let machine = match req.selector {
        // Show a machine with the given ID.
        Some(show_candidate_machine_request::Selector::MachineId(machine_uuid)) => {
            db::measured_boot::machine::from_id(
                &mut txn,
                MachineId::from_str(&machine_uuid).map_err(|_| {
                    CarbideError::from(RpcDataConversionError::InvalidMachineId(machine_uuid))
                })?,
            )
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
        }
        // Show all system profiles.
        None => return Err(CarbideError::InvalidArgument("selector required".to_string()).into()),
    };

    txn.commit().await?;

    Ok(ShowCandidateMachineResponse {
        machine: Some(machine.into()),
    })
}

/// handle_show_candidate_machines handles the ShowCandidateMachines API endpoint.
pub(crate) async fn handle_show_candidate_machines(
    api: &Api,
    _req: ShowCandidateMachinesRequest,
) -> Result<ShowCandidateMachinesResponse, Status> {
    Ok(ShowCandidateMachinesResponse {
        machines: db::measured_boot::machine::get_all(&mut api.db_reader())
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .into_iter()
            .map(|machine| machine.into())
            .collect(),
    })
}

/// handle_list_candidate_machines handles the ListCandidateMachine API endpoint.
pub(crate) async fn handle_list_candidate_machines(
    api: &Api,
    _req: ListCandidateMachinesRequest,
) -> Result<ListCandidateMachinesResponse, Status> {
    Ok(ListCandidateMachinesResponse {
        machines: get_candidate_machine_records(&api.database_connection)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to read records: {e}"),
            })?
            .into_iter()
            .map(
                |record| rpc::protos::measured_boot::CandidateMachineSummaryPb {
                    machine_id: record.machine_id.to_string(),
                    ts: Some(record.created.into()),
                },
            )
            .collect(),
    })
}
