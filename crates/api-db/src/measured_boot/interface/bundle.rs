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
 *  Code for working the measurement_bundles and measurement_bundles_values
 *  tables in the database, leveraging the bundle-specific record types.
 */

use carbide_uuid::DbTable;
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{MeasurementBundleId, MeasurementSystemProfileId};
use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::{
    MeasurementBundleRecord, MeasurementBundleState, MeasurementBundleValueRecord,
};
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::db_read::DbReader;
use crate::measured_boot::interface::common;

/// insert_measurement_bundle_record is a very basic insert of a
/// new row into the measurement_bundles table, where only a profile_id
/// needs to be provided. Is it expected that this is wrapped by
/// a more formal call (where a transaction is initialized).
pub async fn insert_measurement_bundle_record(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
    name: String,
    state: Option<MeasurementBundleState>,
) -> Result<MeasurementBundleRecord, sqlx::Error> {
    match state {
        Some(set_state) => {
            let query = format!(
                "insert into measurement_bundles(profile_id, name, state) values($1, $2, $3) returning {}",
                MeasurementBundleRecord::db_table_columns(),
            );
            sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(profile_id)
                .bind(&name)
                .bind(set_state)
                .fetch_one(txn)
                .await
        }
        None => {
            let query = format!(
                "insert into measurement_bundles(profile_id, name) values($1, $2) returning {}",
                MeasurementBundleRecord::db_table_columns(),
            );
            sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(profile_id)
                .bind(&name)
                .fetch_one(txn)
                .await
        }
    }
}

/// insert_measurement_values takes a vec of PcrRegisterValues and
/// subsequently calls an individual insert for each. It is assumed this is
/// called by a parent wrapper where a transaction was created.
pub async fn insert_measurement_bundle_value_records(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
    values: &[PcrRegisterValue],
) -> Result<Vec<MeasurementBundleValueRecord>, DatabaseError> {
    let mut records: Vec<MeasurementBundleValueRecord> = Vec::new();
    for value in values.iter() {
        records.push(
            insert_measurement_bundle_value_record(
                txn,
                bundle_id,
                value.pcr_register,
                &value.sha_any,
            )
            .await?,
        );
    }
    Ok(records)
}

/// insert_measurement_value inserts a single bundle value, returning the
/// complete inserted record, which includes its new UUID and insert timestamp.
pub async fn insert_measurement_bundle_value_record(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
    pcr_register: i16,
    value: &String,
) -> Result<MeasurementBundleValueRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_bundles_values(bundle_id, pcr_register, sha_any) values($1, $2, $3) returning {}",
        MeasurementBundleValueRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(bundle_id)
        .bind(pcr_register)
        .bind(value)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_measurement_bundle_value_record", e))
}

/// rename_bundle_for_bundle_id renames a bundle based on its bundle ID.
pub async fn rename_bundle_for_bundle_id(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
    new_bundle_name: String,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    let query = format!(
        "update measurement_bundles set name = $1 where bundle_id = $2 returning {}",
        MeasurementBundleRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(new_bundle_name)
        .bind(bundle_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("rename_bundle_for_bundle_id", e))
}

/// rename_bundle_for_bundle_name renames a bundle based on its bundle name.
pub async fn rename_bundle_for_bundle_name(
    txn: &mut PgConnection,
    old_bundle_name: String,
    new_bundle_name: String,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    let query = format!(
        "update measurement_bundles set name = $1 where name = $2 returning {}",
        MeasurementBundleRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(new_bundle_name)
        .bind(old_bundle_name)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("rename_bundle_for_bundle_name", e))
}

/// update_state_for_bundle_id updates the state for a given bundle ID.
///
/// This is the last line of defense to make sure a bundle cant move
/// out of the revoked state.
pub async fn update_state_for_bundle_id(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
    state: MeasurementBundleState,
    allow_from_revoked: bool,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    match allow_from_revoked {
        true => {
            let query = format!(
                "update measurement_bundles set state = $1 where bundle_id = $2 and state != $3 returning {}",
                MeasurementBundleRecord::db_table_columns(),
            );

            sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(state)
                .bind(bundle_id)
                .bind(MeasurementBundleState::Revoked)
                .fetch_optional(txn)
                .await
                .map_err(|e| DatabaseError::new("update_state_for_bundle_id", e))
        }
        false => {
            let query = format!(
                "update measurement_bundles set state = $1 where bundle_id = $2 returning {}",
                MeasurementBundleRecord::db_table_columns(),
            );

            sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(state)
                .bind(bundle_id)
                .fetch_optional(txn)
                .await
                .map_err(|e| DatabaseError::new("update_state_for_bundle_id", e))
        }
    }
}

/// get_measurement_bundle_by_id returns a populated MeasurementBundleRecord
/// for the given `bundle_id`, if it exists. This leverages the generic
/// get_object_for_id function since its a simple/common pattern.
pub async fn get_measurement_bundle_by_id(
    txn: impl DbReader<'_>,
    bundle_id: MeasurementBundleId,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    common::get_object_for_id(txn, bundle_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundle_by_id"))
}

/// get_measurement_bundle_for_name returns a populated MeasurementBundleRecord
/// for the given `bundle_name`, if it exists. This leverages the generic
/// get_object_for_id function since its a simple/common pattern.
pub async fn get_measurement_bundle_for_name(
    txn: &mut PgConnection,
    bundle_name: String,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    common::get_object_for_unique_column(txn, "name", bundle_name.clone())
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundle_for_name"))
}

/// get_measurement_bundle_records returns all MeasurementBundleRecord
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
pub async fn get_measurement_bundle_records(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementBundleRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundle_records"))
}

/// get_measurement_bundle_records_for_profile_id returns all
/// MeasurementBundleRecord instances in the database with the given profile
/// ID.
pub async fn get_measurement_bundle_records_for_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Vec<MeasurementBundleRecord>, DatabaseError> {
    common::get_objects_where_id(txn, profile_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundle_records_for_profile_id"))
}

/// get_measurement_bundles_values returns all MeasurementBundleValueRecord
/// instances in the database. This leverages the generic get_all_objects
/// function since its a simple/common pattern.
pub async fn get_measurement_bundles_values(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementBundleValueRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundles_values"))
}

/// get_measurement_bundle_values_for_bundle_id returns
/// all of the measurement values associated with a given
/// `bundle_id`, where there should be PCR_VALUE_LENGTH
/// values returned. This call leverages the generic
/// get_objects_where_id, allowing a caller to get a list
/// of multiple objects matching a given PgUuid, where
/// the PgUuid is probably a reference/foreign key.
pub async fn get_measurement_bundle_values_for_bundle_id(
    txn: impl DbReader<'_>,
    bundle_id: MeasurementBundleId,
) -> Result<Vec<MeasurementBundleValueRecord>, DatabaseError> {
    common::get_objects_where_id(txn, bundle_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_bundle_values_for_bundle_id"))
}

/// get_machines_for_bundle_id returns a unique list of
/// all MachineId that leverage the given bundle.
pub async fn get_machines_for_bundle_id(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = "select distinct machine_id from measurement_journal where bundle_id = $1 order by machine_id";
    sqlx::query_as(query)
        .bind(bundle_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_machines_for_bundle_id", e))
}

/// get_machines_for_bundle_name returns a unique list of all CandidateMachineId
/// that leverage the given profile.
///
/// This is specifically used by the `bundle list machines by-name` CLI call.
pub async fn get_machines_for_bundle_name(
    txn: &mut PgConnection,
    bundle_name: String,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = "select distinct machine_id from measurement_journal,measurement_bundles where measurement_journal.bundle_id=measurement_bundles.bundle_id and measurement_bundles.name = $1 order by machine_id";
    sqlx::query_as(query)
        .bind(bundle_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_machines_for_bundle_name", e))
}

/// delete_bundle_for_id deletes a bundle record.
pub async fn delete_bundle_for_id(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
) -> Result<Option<MeasurementBundleRecord>, DatabaseError> {
    common::delete_object_where_id(txn, bundle_id)
        .await
        .map_err(|e| e.with_op_name("delete_bundle_for_id"))
}

/// delete_bundle_values_for_id deletes all bundle
/// value records for a bundle.
pub async fn delete_bundle_values_for_id(
    txn: &mut PgConnection,
    bundle_id: MeasurementBundleId,
) -> Result<Vec<MeasurementBundleValueRecord>, DatabaseError> {
    common::delete_objects_where_id(txn, bundle_id)
        .await
        .map_err(|e| e.with_op_name("delete_bundle_values_for_id"))
}

/// import_measurement_bundles is intended for doing "full site" imports,
/// taking a list of all measurement bundle records from one site, and
/// inserting them verbatim in another.
///
/// This should happen before import_measurement_bundles_values, since the
/// parent bundles must exist first.
pub async fn import_measurement_bundles(
    txn: &mut PgConnection,
    bundles: &[MeasurementBundleRecord],
) -> Result<Vec<MeasurementBundleRecord>, DatabaseError> {
    let mut committed = Vec::<MeasurementBundleRecord>::new();
    for bundle in bundles.iter() {
        committed.push(import_measurement_bundle(&mut *txn, bundle).await?);
    }
    Ok(committed)
}

/// import_measurement_bundle takes a fully populated MeasurementBundleRecord
/// and inserts it into the measurement bundles table.
pub async fn import_measurement_bundle(
    txn: &mut PgConnection,
    bundle: &MeasurementBundleRecord,
) -> Result<MeasurementBundleRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_bundles(bundle_id, profile_id, name, ts, state) values($1, $2, $3, $4, $5) returning {}",
        MeasurementBundleRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(bundle.bundle_id)
        .bind(bundle.profile_id)
        .bind(&bundle.name)
        .bind(bundle.ts)
        .bind(bundle.state)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("import_measurement_bundle", e))
}

/// import_measurement_bundles_values is intended for doing "full site"
/// imports, taking a list of all measurement bundles from one site, and
/// inserting them verbatim in another.
///
/// This should happen after import_measurement_bundles, since the
/// parent bundles must exist first.
pub async fn import_measurement_bundles_values(
    txn: &mut PgConnection,
    records: &[MeasurementBundleValueRecord],
) -> Result<Vec<MeasurementBundleValueRecord>, DatabaseError> {
    let mut committed = Vec::<MeasurementBundleValueRecord>::new();
    for record in records.iter() {
        committed.push(import_measurement_bundles_value(&mut *txn, record).await?);
    }
    Ok(committed)
}

/// import_measurement_bundles_value takes a fully populated
/// MeasurementBundleValueRecord and inserts it into the measurement bundles
/// values table.
pub async fn import_measurement_bundles_value(
    txn: &mut PgConnection,
    bundle: &MeasurementBundleValueRecord,
) -> Result<MeasurementBundleValueRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_bundles_values(value_id, bundle_id, pcr_register, sha_any, ts) values($1, $2, $3, $4, $5) returning {}",
        MeasurementBundleValueRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(bundle.value_id)
        .bind(bundle.bundle_id)
        .bind(bundle.pcr_register)
        .bind(&bundle.sha_any)
        .bind(bundle.ts)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("import_measurement_bundles_value", e))
}
