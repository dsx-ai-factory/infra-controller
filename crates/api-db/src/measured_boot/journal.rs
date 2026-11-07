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
use measured_boot::journal::MeasurementJournal;
use measured_boot::records::{MeasurementJournalRecord, MeasurementMachineState};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::measured_boot::interface::common;
use crate::measured_boot::interface::journal::{
    delete_journal_where_id, get_measurement_journal_record_by_id,
    get_measurement_journal_record_by_report_id, insert_measurement_journal_record,
    update_measurement_journal_record,
};
use crate::{DatabaseError, DatabaseResult};

pub async fn new(
    txn: &mut PgConnection,
    machine_id: MachineId,
    report_id: MeasurementReportId,
    profile_id: Option<MeasurementSystemProfileId>,
    bundle_id: Option<MeasurementBundleId>,
    state: MeasurementMachineState,
) -> DatabaseResult<MeasurementJournal> {
    create_measurement_journal(txn, machine_id, report_id, profile_id, bundle_id, state).await
}

/// from_id populates an existing MeasurementJournal
/// instance from data in the database for the given
/// journal ID.
pub async fn from_id(
    txn: &mut PgConnection,
    journal_id: MeasurementJournalId,
) -> DatabaseResult<MeasurementJournal> {
    get_measurement_journal_by_id(txn, journal_id).await
}

pub async fn delete_where_id(
    txn: &mut PgConnection,
    journal_id: MeasurementJournalId,
) -> DatabaseResult<Option<MeasurementJournal>> {
    let info = delete_journal_where_id(txn, journal_id).await?;
    match info {
        None => Ok(None),
        Some(info) => Ok(Some(MeasurementJournal {
            journal_id: info.journal_id,
            machine_id: info.machine_id,
            report_id: info.report_id,
            profile_id: info.profile_id,
            bundle_id: info.bundle_id,
            state: info.state,
            ts: info.ts,
        })),
    }
}

pub async fn get_all(txn: impl DbReader<'_>) -> DatabaseResult<Vec<MeasurementJournal>> {
    get_measurement_journals(txn).await
}

/// create_measurement_journal handles the work of creating a new
/// measurement journal record as well as all associated value records.
async fn create_measurement_journal(
    txn: &mut PgConnection,
    machine_id: MachineId,
    report_id: MeasurementReportId,
    profile_id: Option<MeasurementSystemProfileId>,
    bundle_id: Option<MeasurementBundleId>,
    state: MeasurementMachineState,
) -> DatabaseResult<MeasurementJournal> {
    let info =
        insert_measurement_journal_record(txn, machine_id, report_id, profile_id, bundle_id, state)
            .await?;

    Ok(MeasurementJournal {
        journal_id: info.journal_id,
        machine_id: info.machine_id,
        report_id: info.report_id,
        profile_id: info.profile_id,
        bundle_id: info.bundle_id,
        state: info.state,
        ts: info.ts,
    })
}

pub(crate) async fn update_measurement_journal(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    profile_id: Option<MeasurementSystemProfileId>,
    bundle_id: Option<MeasurementBundleId>,
    state: MeasurementMachineState,
) -> DatabaseResult<MeasurementJournal> {
    let info =
        update_measurement_journal_record(txn, report_id, profile_id, bundle_id, state).await?;

    Ok(MeasurementJournal {
        journal_id: info.journal_id,
        machine_id: info.machine_id,
        report_id: info.report_id,
        profile_id: info.profile_id,
        bundle_id: info.bundle_id,
        state: info.state,
        ts: info.ts,
    })
}

/// get_measurement_journal_by_id does the work of populating a full
/// MeasurementJournal instance, with values and all.
async fn get_measurement_journal_by_id(
    txn: &mut PgConnection,
    journal_id: MeasurementJournalId,
) -> DatabaseResult<MeasurementJournal> {
    match get_measurement_journal_record_by_id(txn, journal_id).await? {
        Some(info) => Ok(MeasurementJournal {
            journal_id: info.journal_id,
            machine_id: info.machine_id,
            report_id: info.report_id,
            profile_id: info.profile_id,
            bundle_id: info.bundle_id,
            state: info.state,
            ts: info.ts,
        }),
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementJournal",
            id: journal_id.to_string(),
        }),
    }
}

pub async fn get_journal_for_report_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> DatabaseResult<MeasurementJournal> {
    match get_measurement_journal_record_by_report_id(txn, report_id).await? {
        Some(info) => Ok(MeasurementJournal {
            journal_id: info.journal_id,
            machine_id: info.machine_id,
            report_id: info.report_id,
            profile_id: info.profile_id,
            bundle_id: info.bundle_id,
            state: info.state,
            ts: info.ts,
        }),
        None => Err(DatabaseError::NotFoundError {
            kind: "MeasurementJournal",
            id: report_id.to_string(),
        }),
    }
}

/// get_measurement_journals returns all MeasurementJournal
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
async fn get_measurement_journals(
    txn: impl DbReader<'_>,
) -> DatabaseResult<Vec<MeasurementJournal>> {
    let journal_records: Vec<MeasurementJournalRecord> = common::get_all_objects(txn).await?;
    let res: Vec<MeasurementJournal> = journal_records
        .iter()
        .map(|record| MeasurementJournal {
            journal_id: record.journal_id,
            machine_id: record.machine_id,
            report_id: record.report_id,
            profile_id: record.profile_id,
            bundle_id: record.bundle_id,
            state: record.state,
            ts: record.ts,
        })
        .collect();
    Ok(res)
}

/// get_latest_journal_for_id returns the latest journal record for the
/// provided machine ID.
pub async fn get_latest_journal_for_id(
    txn: impl DbReader<'_>,
    machine_id: MachineId,
) -> DatabaseResult<Option<MeasurementJournal>> {
    let query = format!(
        "select distinct on (machine_id) {} from measurement_journal where machine_id = $1 order by machine_id,ts desc",
        MeasurementJournalRecord::db_table_columns(),
    );
    match sqlx::query_as::<_, MeasurementJournalRecord>(sqlx::AssertSqlSafe(query))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("get_latest_journal_for_id", e))?
    {
        Some(info) => Ok(Some(MeasurementJournal {
            journal_id: info.journal_id,
            machine_id: info.machine_id,
            report_id: info.report_id,
            profile_id: info.profile_id,
            bundle_id: info.bundle_id,
            state: info.state,
            ts: info.ts,
        })),
        None => Ok(None),
    }
}

pub async fn get_all_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<Vec<MeasurementJournal>> {
    get_measurement_journals_for_machine_id(txn, machine_id).await
}

pub async fn get_latest_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<Option<MeasurementJournal>> {
    get_latest_journal_for_id(txn, machine_id).await
}

/// get_measurement_journals_for_machine_id returns all fully populated
/// journal instances for a given machine ID, which is used by the
/// `journal show` CLI option.
async fn get_measurement_journals_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<Vec<MeasurementJournal>> {
    let records =
        crate::measured_boot::interface::journal::get_measurement_journal_records_for_machine_id(
            txn, machine_id,
        )
        .await?;
    Ok(records
        .iter()
        .map(|record| MeasurementJournal {
            journal_id: record.journal_id,
            machine_id: record.machine_id,
            report_id: record.report_id,
            profile_id: record.profile_id,
            bundle_id: record.bundle_id,
            state: record.state,
            ts: record.ts,
        })
        .collect())
}
