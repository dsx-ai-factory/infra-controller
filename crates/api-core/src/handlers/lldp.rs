// SPDX-FileCopyrightText: Copyright (c) 2025-2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::str::FromStr;

use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineId;
use model::lldp::LldpNeighbor;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// Receive a periodic LLDP neighbor report from a running scout.
///
/// The DPU agent does not use this RPC; it attaches its report to `RecordDpuNetworkStatus` instead
pub(crate) async fn report_lldp_neighbors(
    api: &Api,
    request: Request<rpc::LldpNeighborReport>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let machine_id = convert_and_log_machine_id(request.get_ref().machine_id.as_ref())?;
    if !api.runtime_config.bypass_rbac {
        let id_str = request
            .extensions()
            .get::<crate::auth::AuthContext>()
            .and_then(|ctx| ctx.get_spiffe_machine_id())
            .ok_or_else(|| {
                CarbideError::ClientCertificateMissingInformation(
                    "LLDP report must include a valid machine SPIFFE certificate".into(),
                )
            })?;
        let caller_machine_id = MachineId::from_str(id_str).map_err(|_| {
            CarbideError::ClientCertificateMissingInformation(
                "machine ID in SPIFFE certificate is invalid".into(),
            )
        })?;
        if caller_machine_id != machine_id {
            return Err(CarbideError::PermissionDeniedError(format!(
                "machine {caller_machine_id} cannot report LLDP neighbors for machine {machine_id}"
            ))
            .into());
        }
    }
    let report = request
        .into_inner()
        .report
        .ok_or(CarbideError::MissingArgument("report"))?;

    let mut txn = api.txn_begin().await?;
    handle_lldp_report(&mut txn, &machine_id, report).await?;
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn handle_lldp_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    report: rpc::LldpReport,
) -> Result<(), CarbideError> {
    let result = rpc::LldpReportResult::try_from(report.result).map_err(|_| {
        CarbideError::InvalidArgument(format!("unknown LLDP report result {}", report.result))
    })?;

    match result {
        // Treating the zero value as a fresh snapshot would reconcile the machine's neighbors
        // away, so a report that names no result is rejected rather than guessed at.
        rpc::LldpReportResult::Unspecified => Err(CarbideError::InvalidArgument(
            "LLDP report result is unspecified".to_string(),
        )),
        rpc::LldpReportResult::Updated => {
            let neighbors = report
                .interfaces
                .into_iter()
                .map(LldpNeighbor::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(CarbideError::from)?;
            store_neighbors(txn, machine_id, &neighbors).await
        }
        // The reporter has already confirmed nothing changed, so what nico-api
        // holds is still current.
        rpc::LldpReportResult::Unchanged => {
            tracing::debug!(%machine_id, "LLDP neighbors unchanged");
            Ok(())
        }
        // The machine could not read its neighbors. This repeats on every poll while
        // collection keeps failing, and the reporter re-sends its full snapshot on
        // recovery.
        // TODO: decide whether stored neighbors should be cleared or marked stale after a
        // failed collection instead of being kept as the last known topology.
        rpc::LldpReportResult::CollectionFailed => {
            tracing::warn!(%machine_id, "Reporter could not collect LLDP neighbors");
            Ok(())
        }
    }
}

/// Replace the machine's stored neighbors with new ones.
async fn store_neighbors(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    neighbors: &[LldpNeighbor],
) -> Result<(), CarbideError> {
    db::machine_lldp_neighbor::replace_all(txn, machine_id, neighbors).await?;
    tracing::debug!(%machine_id, neighbors = neighbors.len(), "Stored LLDP neighbors");
    Ok(())
}
