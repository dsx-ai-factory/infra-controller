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
 * gRPC handlers for measurement journal related API calls.
 */

use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use db::measured_boot::interface::journal::{
    get_measurement_journal_records, get_measurement_journal_records_for_machine_id,
};
use rpc::protos::measured_boot::{
    DeleteMeasurementJournalRequest, DeleteMeasurementJournalResponse,
    ListMeasurementJournalRequest, ListMeasurementJournalResponse, MeasurementJournalRecordPb,
    ShowMeasurementJournalRequest, ShowMeasurementJournalResponse, ShowMeasurementJournalsRequest,
    ShowMeasurementJournalsResponse, list_measurement_journal_request,
    show_measurement_journal_request,
};
use tonic::Status;

use crate::api::Api;
use crate::errors::CarbideError;

/// handle_delete_measurement_journal handles the DeleteMeasurementJournal
/// API endpoint.
pub(crate) async fn handle_delete_measurement_journal(
    api: &Api,
    req: DeleteMeasurementJournalRequest,
) -> Result<DeleteMeasurementJournalResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let journal = db::measured_boot::journal::delete_where_id(
        &mut txn,
        req.journal_id
            .ok_or(CarbideError::MissingArgument("journal_id"))?,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("failed to delete journal: {e}"),
    })?
    .ok_or(CarbideError::NotFoundError {
        kind: "journal",
        id: "unknown".into(),
    })?;

    txn.commit().await?;
    Ok(DeleteMeasurementJournalResponse {
        journal: Some(journal.into()),
    })
}

/// handle_show_measurement_journal handles the ShowMeasurementJournal
/// API endpoint.
pub(crate) async fn handle_show_measurement_journal(
    api: &Api,
    req: ShowMeasurementJournalRequest,
) -> Result<ShowMeasurementJournalResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let journal = match req.selector {
        Some(selector) => match selector {
            show_measurement_journal_request::Selector::JournalId(journal_id) => {
                db::measured_boot::journal::from_id(&mut txn, journal_id)
                    .await
                    .map_err(|e| CarbideError::Internal {
                        message: format!("{e}"),
                    })?
            }
            show_measurement_journal_request::Selector::LatestForMachineId(machine_id) => {
                match db::measured_boot::journal::get_latest_journal_for_id(
                    &mut txn,
                    MachineId::from_str(&machine_id).map_err(|e| {
                        CarbideError::InvalidArgument(format!("could not parse MachineId: {e}"))
                    })?,
                )
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("{e}"),
                })? {
                    Some(journal) => journal,
                    None => {
                        return Ok(ShowMeasurementJournalResponse { journal: None });
                    }
                }
            }
        },
        None => {
            return Err(
                CarbideError::InvalidArgument("selector must be provided".to_string()).into(),
            );
        }
    };

    txn.commit().await?;

    Ok(ShowMeasurementJournalResponse {
        journal: Some(journal.into()),
    })
}

/// handle_show_measurement_journals handles the ShowMeasurementJournals
/// API endpoint.
pub(crate) async fn handle_show_measurement_journals(
    api: &Api,
    _req: ShowMeasurementJournalsRequest,
) -> Result<ShowMeasurementJournalsResponse, Status> {
    Ok(ShowMeasurementJournalsResponse {
        journals: db::measured_boot::journal::get_all(&api.database_connection)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to fetch journals: {e}"),
            })?
            .drain(..)
            .map(|journal| journal.into())
            .collect(),
    })
}

/// handle_list_measurement_journal handles the ListMeasurementJournal
/// API endpoint.
pub(crate) async fn handle_list_measurement_journal(
    api: &Api,
    req: ListMeasurementJournalRequest,
) -> Result<ListMeasurementJournalResponse, Status> {
    let mut txn = api.txn_begin().await?;

    let journals: Vec<MeasurementJournalRecordPb> = match &req.selector {
        Some(list_measurement_journal_request::Selector::MachineId(machine_id)) => {
            let machine_id =
                MachineId::from_str(machine_id).map_err(|e| CarbideError::Internal {
                    message: format!("failed to fetch journals for machine: {e}"),
                })?;

            get_measurement_journal_records_for_machine_id(&mut txn, machine_id)
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("failed to fetch journals for machine: {e}"),
                })?
                .drain(..)
                .map(|journal| journal.into())
                .collect()
        }
        None => get_measurement_journal_records(&mut txn)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to fetch journals: {e}"),
            })?
            .drain(..)
            .map(|journal| journal.into())
            .collect(),
    };

    txn.commit().await?;

    Ok(ListMeasurementJournalResponse { journals })
}
