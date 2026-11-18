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
 *  Code for working the measuremment_reports and measurement_reports_values
 *  tables in the database, leveraging the report-specific record types.
 */

use carbide_uuid::DbTable;
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementReportId;
use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::{MeasurementReportRecord, MeasurementReportValueRecord};
use sqlx::{PgConnection, Postgres, QueryBuilder};

use crate::DatabaseError;
use crate::db_read::DbReader;
use crate::measured_boot::interface::common;

/// match_latest_reports takes a list of PcrRegisterValues (i.e. register:shaXXX)
/// and returns all latest matching report entries for it.
///
/// The intent is bundle operations can call this to see what reports
/// match the bundle.
pub fn where_pcr_pairs(query: &mut QueryBuilder<Postgres>, values: &[PcrRegisterValue]) {
    query.push("where (pcr_register, sha_any) in (");
    for (pair_index, value) in values.iter().enumerate() {
        query.push("(");
        query.push_bind(value.pcr_register);
        query.push(",");
        query.push_bind(value.sha_any.clone());
        query.push(")");
        if pair_index < values.len() - 1 {
            query.push(", ");
        }
    }
    query.push(") ");
}

pub async fn match_latest_reports(
    txn: impl DbReader<'_>,
    values: &[PcrRegisterValue],
) -> Result<Vec<MeasurementReportRecord>, DatabaseError> {
    if values.is_empty() {
        return Err(DatabaseError::new(
            "match_latest_reports",
            sqlx::Error::Protocol(String::from("empty values list")),
        ));
    }

    let pcr_register_len = values.len();

    let mut query: QueryBuilder<Postgres> = QueryBuilder::new(
        "select measurement_reports.report_id, measurement_reports.machine_id, measurement_reports.ts
        from measurement_reports
        join
            measurement_reports_values
                on measurement_reports.report_id=measurement_reports_values.report_id
        join
            (select distinct on (machine_id) * from measurement_reports order by machine_id,ts desc) as latest_reports
                on measurement_reports_values.report_id=latest_reports.report_id ");
    where_pcr_pairs(&mut query, values);

    query.push("group by measurement_reports.report_id ");
    query.push("having count(*) = ");
    query.push_bind(pcr_register_len as i16);

    let prepared = query.build_query_as();

    prepared
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("match_latest_reports", e))
}

/// insert_measurement_report_record is a very basic insert of a
/// new row into the measurement_reports table. Is it expected that
/// this is wrapped by a more formal call (where a txn is initialized)
/// to also set corresponding value records.
pub async fn insert_measurement_report_record(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> Result<MeasurementReportRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_reports(machine_id) values($1) returning {}",
        MeasurementReportRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_measurement_report_record", e))
}

/// insert_measurement_report_value_records takes a vec of
/// Strings and subsequently calls an individual insert
/// for each value. It is assumed this is called by a parent
/// wrapper where a transaction is created.
pub async fn insert_measurement_report_value_records(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    values: &[PcrRegisterValue],
) -> Result<Vec<MeasurementReportValueRecord>, DatabaseError> {
    if values.is_empty() {
        return Err(DatabaseError::new(
            "match_latest_reports",
            sqlx::Error::Protocol(String::from("empty PcrRegisterValues list")),
        ));
    }

    let mut records: Vec<MeasurementReportValueRecord> = Vec::new();
    for value in values.iter() {
        records.push(insert_measurement_report_value_record(txn, report_id, value).await?);
    }
    Ok(records)
}

/// insert_measurement_report_value_record inserts a single report value.
async fn insert_measurement_report_value_record(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    value: &PcrRegisterValue,
) -> Result<MeasurementReportValueRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_reports_values(report_id, pcr_register, sha_any) values($1, $2, $3) returning {}",
        MeasurementReportValueRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(report_id)
        .bind(value.pcr_register)
        .bind(&value.sha_any)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_measurement_report_value_record", e))
}

/// get_all_measurement_report_records returns all MeasurementReportRecord
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
pub async fn get_all_measurement_report_records(
    txn: &mut PgConnection,
) -> Result<Vec<MeasurementReportRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_all_measurement_report_records"))
}

/// get_measurement_report_record_by_id returns a populated
/// MeasurementReportRecord for the given `report_id`,
/// if it exists. This leverages the generic get_object_for_id
/// function since its a simple/common pattern.
pub async fn get_measurement_report_record_by_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> Result<Option<MeasurementReportRecord>, DatabaseError> {
    common::get_object_for_id(txn, report_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_report_record_by_id"))
}

/// get_measurement_report_records_for_machine_id returns all report
/// records for a given machine ID, which is used by the `report list`
/// CLI option.
pub async fn get_measurement_report_records_for_machine_id(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> Result<Vec<MeasurementReportRecord>, DatabaseError> {
    common::get_objects_where_id(txn, machine_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_report_records_for_machine_id"))
}

/// get_measurement_report_values_for_report_id returns
/// all of the measurement values associated with a given
/// `report_id`. This call leverages the generic
/// get_objects_where_id, allowing a caller to get a list
/// of multiple objects matching a given PgUuid, where
/// the PgUuid is probably a reference/foreign key.
pub async fn get_measurement_report_values_for_report_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> Result<Vec<MeasurementReportValueRecord>, DatabaseError> {
    common::get_objects_where_id(txn, report_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_report_values_for_report_id"))
}

/// delete_report_for_id deletes a report record.
pub async fn delete_report_for_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> Result<Option<MeasurementReportRecord>, DatabaseError> {
    common::delete_object_where_id(txn, report_id).await
}

/// delete_report_values_for_id deletes all report
/// value records for a report.
pub async fn delete_report_values_for_id(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
) -> Result<Vec<MeasurementReportValueRecord>, DatabaseError> {
    common::delete_objects_where_id(txn, report_id)
        .await
        .map_err(|e| e.with_op_name("delete_report_values_for_id"))
}

pub(crate) async fn update_report_tstamp(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<MeasurementReportRecord, DatabaseError> {
    let query = format!(
        "UPDATE measurement_reports SET ts = $1 WHERE report_id = $2 returning {}",
        MeasurementReportRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(ts)
        .bind(report_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update_report_tstamp", e))
}

pub(crate) async fn update_report_values_tstamp(
    txn: &mut PgConnection,
    report_id: MeasurementReportId,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<MeasurementReportValueRecord>, DatabaseError> {
    let query = format!(
        "UPDATE measurement_reports_values SET ts = $1 WHERE report_id = $2 returning {}",
        MeasurementReportValueRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(ts)
        .bind(report_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("update_report_values_tstamp", e))
}

/// get_all_measurement_report_value_records returns all
/// MeasurementReportValueRecord instances in the database. This leverages
/// the generic get_all_objects function since its a simple/common pattern.
pub async fn get_all_measurement_report_value_records(
    txn: &mut PgConnection,
) -> Result<Vec<MeasurementReportValueRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_all_measurement_report_value_records"))
}
