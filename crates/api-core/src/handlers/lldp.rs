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

use ::rpc::forge as rpc;
use carbide_uuid::machine::MachineId;
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

    let request = request.into_inner();
    let machine_id = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let report = request
        .report
        .ok_or(CarbideError::MissingArgument("report"))?;

    handle_lldp_report(api, &machine_id, &report).await?;
    Ok(Response::new(()))
}

// Nothing is awaited yet because the handler only classifies and logs; the persistence PR adds
// the database writes.
#[expect(clippy::unused_async, reason = "persistence lands in the next PR")]
pub(crate) async fn handle_lldp_report(
    _api: &Api,
    machine_id: &MachineId,
    report: &rpc::LldpReport,
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
            tracing::debug!(
                %machine_id,
                interfaces = report.interfaces.len(),
                "Received LLDP neighbor report (not persisted yet)"
            );
            // TODO Add handling logic in the next PR
            Ok(())
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
        rpc::LldpReportResult::CollectionFailed => {
            tracing::warn!(%machine_id, "Reporter could not collect LLDP neighbors");
            Ok(())
        }
    }
}
