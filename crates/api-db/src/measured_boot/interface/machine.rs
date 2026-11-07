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
 *  Code for working the machine_topologies table in the
 *  database, leveraging the machine-specific record types.
 */

use carbide_uuid::DbTable;
use carbide_uuid::machine::MachineId;
use measured_boot::records::{MeasurementJournalRecord, MeasurementMachineState};
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::db_read::DbReader;
use crate::measured_boot::interface::common;
use crate::measured_boot::machine::CandidateMachineRecord;

/// get_candidate_machine_state figures out the current state of the given
/// machine ID by checking its most recent bundle (or lack thereof), and
/// using that result to give it a corresponding MeasurementMachineState.
pub async fn get_candidate_machine_state(
    txn: impl DbReader<'_>,
    machine_id: MachineId,
) -> Result<MeasurementMachineState, DatabaseError> {
    Ok(match get_latest_journal_for_id(txn, machine_id).await? {
        Some(record) => record.state,
        None => MeasurementMachineState::Discovered,
    })
}

/// get_latest_journal_for_id returns the latest journal record for the
/// provided machine ID.
pub async fn get_latest_journal_for_id(
    txn: impl DbReader<'_>,
    machine_id: MachineId,
) -> Result<Option<MeasurementJournalRecord>, DatabaseError> {
    let query = format!(
        "select distinct on (machine_id) {} from measurement_journal where machine_id = $1 order by machine_id,ts desc",
        MeasurementJournalRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("get_latest_journal_for_id", e))
}

/// get_candidate_machine_record_by_id returns a CandidateMachineRecord row.
pub async fn get_candidate_machine_record_by_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> Result<Option<CandidateMachineRecord>, DatabaseError> {
    common::get_object_for_id(txn, machine_id)
        .await
        .map_err(|e| e.with_op_name("get_candidate_machine_record_by_id"))
}

/// get_candidate_machine_records returns all MockMachineRecord rows,
/// primarily for the purpose of `mock-machine list`.
pub async fn get_candidate_machine_records(
    txn: impl DbReader<'_>,
) -> Result<Vec<CandidateMachineRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_candidate_machine_records"))
}
