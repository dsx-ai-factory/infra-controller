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
 *  Code for working the measuremment_journal and measurement_journal_values
 *  tables in the database, leveraging the journal-specific record types.
 */

use carbide_uuid::DbTable;
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{
    MeasurementBundleId, MeasurementJournalId, MeasurementReportId, MeasurementSystemProfileId,
};
use measured_boot::records::{MeasurementJournalRecord, MeasurementMachineState};
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::measured_boot::interface::common;

/// insert_measurement_journal_record is a very basic insert of a
/// new row into the measurement_journals table. Is it expected that
/// this is wrapped by a more formal call (where a txn is initialized)
/// to also set corresponding value records.
pub async fn insert_measurement_journal_record(
    txn: &mut PgConnection,
    machine_id: MachineId,
    report_id: MeasurementReportId,
    profile_id: Option<MeasurementSystemProfileId>,
    bundle_id: Option<MeasurementBundleId>,
    state: MeasurementMachineState,
) -> Result<MeasurementJournalRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_journal(machine_id, report_id, profile_id, bundle_id, state) values($1, $2, $3, $4, $5) returning {}",
        MeasurementJournalRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(machine_id)
        .bind(report_id)
        .bind(profile_id)
        .bind(bundle_id)
        .bind(state)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_measurement_journal_record", e))
}

/// this is used to remove the duplication of reports and records
/// we update everything except for machine id and report id as
/// those things are not supposed to change
pub async fn update_measurement_journal_record(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    profile_id: Option<MeasurementSystemProfileId>,
    bundle_id: Option<MeasurementBundleId>,
    state: MeasurementMachineState,
) -> Result<MeasurementJournalRecord, DatabaseError> {
    let query = format!(
        "update measurement_journal set profile_id = $1, bundle_id = $2, state = $3, ts = $4 where report_id = $5 returning {}",
        MeasurementJournalRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(profile_id)
        .bind(bundle_id)
        .bind(state)
        .bind(chrono::Utc::now())
        .bind(report_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update_measurement_journal_record", e))
}

/// delete_journal_where_id deletes a journal record.
pub async fn delete_journal_where_id(
    txn: &mut PgConnection,
    journal_id: MeasurementJournalId,
) -> Result<Option<MeasurementJournalRecord>, DatabaseError> {
    common::delete_object_where_id(txn, journal_id)
        .await
        .map_err(|e| e.with_op_name("delete_journal_where_id"))
}

/// get_measurement_journal_record_by_id returns a populated
/// MeasurementJournalRecord for the given `journal_id`,
/// if it exists. This leverages the generic get_object_for_id
/// function since its a simple/common pattern.
pub async fn get_measurement_journal_record_by_id(
    txn: &mut PgConnection,
    journal_id: MeasurementJournalId,
) -> Result<Option<MeasurementJournalRecord>, DatabaseError> {
    common::get_object_for_id(txn, journal_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_journal_record_by_id"))
}

/// get_measurement_journal_record_by_report_id returns a populated
/// MeasurementJournalRecord for the given `report_id`,
/// if it exists. This leverages the generic get_object_for_id
/// function since its a simple/common pattern.
pub async fn get_measurement_journal_record_by_report_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> Result<Option<MeasurementJournalRecord>, DatabaseError> {
    common::get_object_for_id(txn, report_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_journal_record_by_report_id"))
}

/// get_measurement_journal_records returns all MeasurementJournalRecord
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
pub async fn get_measurement_journal_records(
    txn: &mut PgConnection,
) -> Result<Vec<MeasurementJournalRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_measurement_journal_records"))
}

/// get_measurement_journal_records_for_machine_id returns all journal
/// records for a given machine ID, which is used by the `journal list`
/// CLI option.
pub async fn get_measurement_journal_records_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> Result<Vec<MeasurementJournalRecord>, DatabaseError> {
    common::get_objects_where_id(txn, machine_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_journal_records_for_machine_id"))
}
