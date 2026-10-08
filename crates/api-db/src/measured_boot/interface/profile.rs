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
 *  Code for working the measurement_system_profiles and measurement_system_profiles_attrs
 *  tables in the database, leveraging the profile-specific record types.
 */

use std::collections::HashMap;

use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::{MeasurementBundleId, MeasurementSystemProfileId};
use carbide_uuid::{DbPrimaryUuid, DbTable};
use measured_boot::records::{MeasurementSystemProfileAttrRecord, MeasurementSystemProfileRecord};
use sqlx::query_builder::QueryBuilder;
use sqlx::{PgConnection, Postgres};

use crate::DatabaseError;
use crate::db_read::DbReader;
use crate::measured_boot::interface::common;

/// insert_measurement_profile_record is a very basic insert of a
/// new row into the measurement_system_profiles table, where only a name
/// needs to be provided.
///
/// Is it expected that this is wrapped by
/// a more formal call (where a transaction is initialized).
pub async fn insert_measurement_profile_record(
    txn: &mut PgConnection,
    name: String,
) -> Result<MeasurementSystemProfileRecord, sqlx::Error> {
    let query = format!(
        "insert into measurement_system_profiles(name) values($1) returning {}",
        MeasurementSystemProfileRecord::db_table_columns(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(&name)
        .fetch_one(txn)
        .await
}

/// insert_measurement_profile_attr_records takes a hashmap of
/// k/v attributes and subsequently calls an individual insert
/// for each pair. It is assumed this is called by a parent
/// wrapper where a transaction is created.
pub async fn insert_measurement_profile_attr_records(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
    attrs: &HashMap<String, String>,
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    let mut attributes: Vec<MeasurementSystemProfileAttrRecord> = Vec::new();
    for (key, value) in attrs.iter() {
        attributes.push(insert_measurement_profile_attr_record(txn, profile_id, key, value).await?);
    }
    Ok(attributes)
}

/// insert_measurement_profile_attr_record inserts a single
/// profile attribute (k/v) pair.
async fn insert_measurement_profile_attr_record(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
    key: &String,
    value: &String,
) -> Result<MeasurementSystemProfileAttrRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_system_profiles_attrs(profile_id, key, value) values($1, $2, $3) returning {}",
        MeasurementSystemProfileAttrRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(profile_id)
        .bind(key)
        .bind(value)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("insert_measurement_profile_attr_record", e))
}

/// rename_profile_for_profile_id renames a profile based on its profile ID.
pub async fn rename_profile_for_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
    new_profile_name: String,
) -> Result<MeasurementSystemProfileRecord, DatabaseError> {
    let query = format!(
        "update measurement_system_profiles set name = $1 where profile_id = $2 returning {}",
        MeasurementSystemProfileRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(new_profile_name)
        .bind(profile_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("rename_profile_for_profile_id", e))
}

/// rename_profile_for_profile_name renames a profile based
/// on its profile name.
pub async fn rename_profile_for_profile_name(
    txn: &mut PgConnection,
    old_profile_name: String,
    new_profile_name: String,
) -> Result<MeasurementSystemProfileRecord, DatabaseError> {
    let query = format!(
        "update measurement_system_profiles set name = $1 where name = $2 returning {}",
        MeasurementSystemProfileRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(new_profile_name)
        .bind(old_profile_name)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("rename_profile_for_profile_name", e))
}

/// get_all_measurement_profile_records gets all system profile records.
pub async fn get_all_measurement_profile_records(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementSystemProfileRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_all_measurement_profile_records"))
}

/// get_measurement_profile_record_by_id returns a populated
/// MeasurementSystemProfileRecord for the given `profile_id`,
/// if it exists.
pub async fn get_measurement_profile_record_by_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Option<MeasurementSystemProfileRecord>, DatabaseError> {
    common::get_object_for_id(txn, profile_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_profile_record_by_id"))
}

/// get_measurement_profile_record_by_name returns a populated
/// MeasurementSystemProfileRecord for the given `name`,
/// if it exists.
pub async fn get_measurement_profile_record_by_name(
    txn: &mut PgConnection,
    val: String,
) -> Result<Option<MeasurementSystemProfileRecord>, DatabaseError> {
    common::get_object_for_unique_column(txn, "name", val)
        .await
        .map_err(|e| e.with_op_name("get_measurement_profile_record_by_name"))
}

/// delete_profile_record_for_id deletes a profile record
/// with the given profile_id.
pub async fn delete_profile_record_for_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Option<MeasurementSystemProfileRecord>, DatabaseError> {
    common::delete_object_where_id(txn, profile_id)
        .await
        .map_err(|e| e.with_op_name("delete_profile_record_for_id"))
}

/// delete_profile_attr_records_for_id deletes all profile
/// attribute records for a given profile ID.
pub async fn delete_profile_attr_records_for_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    common::delete_objects_where_id(txn, profile_id)
        .await
        .map_err(|e| e.with_op_name("delete_profile_attr_records_for_id"))
}

/// get_measurement_profile_record_by_attrs will attempt to get a single
/// MeasurementSystemProfileRecord for the given attrs.
pub async fn get_measurement_profile_record_by_attrs(
    txn: &mut PgConnection,
    attrs: &HashMap<String, String>,
) -> Result<Option<MeasurementSystemProfileRecord>, DatabaseError> {
    match get_measurement_profile_id_by_attrs(txn, attrs).await? {
        Some(profile_id) => get_measurement_profile_record_by_id(txn, profile_id).await,
        None => Ok(None),
    }
}

/// get_measurement_profile_id_by_attrs attempts to return a profile
/// whose attributes match the input attributes.
///
/// It ultimately looks like this:
///
/// SELECT {t1}.{join_id}
/// FROM {t1}
/// JOIN (
///    SELECT {join_id}
///    FROM (
///      SELECT {join_id}, COUNT(DISTINCT key) AS key_count, COUNT(DISTINCT value) AS value_count
///      FROM {t1}
///      WHERE (key, value) IN ({attr_pairs})
///      GROUP BY {join_id}
///    ) AS possible_ids
///    WHERE key_count = {attrs_len} AND value_count = {attrs_len}
/// ) AS {t2} ON {t1}.{join_id} = {t2}.{join_id}
/// GROUP BY {t1}}.{join_id}
/// HAVING COUNT(*) = {attrs_len}"
pub async fn get_measurement_profile_id_by_attrs(
    txn: &mut PgConnection,
    attrs: &HashMap<String, String>,
) -> Result<Option<MeasurementSystemProfileId>, DatabaseError> {
    let t1 = "measurement_system_profiles_attrs";
    let t2 = "matched_ids";
    let join_id = MeasurementSystemProfileId::db_primary_uuid_name();
    let attrs_len = attrs.len() as i32;

    // if attrs_len == 0 {
    //     return Err(eyre::eyre!("cannot get by attrs when no attrs provided"));
    // }

    let mut query: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "
    SELECT {t1}.{join_id}
    FROM {t1}
    JOIN (
        SELECT {join_id}
        FROM (
            SELECT {join_id}, COUNT(DISTINCT key) AS key_count, COUNT(DISTINCT value) AS value_count
            FROM {t1} "
    ));
    where_attr_pairs(&mut query, attrs);

    query.push(format!(
        "
            GROUP BY {join_id}
        ) AS possible_ids ",
    ));

    query.push("WHERE key_count = ");
    query.push_bind(attrs_len);
    query.push(" AND value_count = ");
    query.push_bind(attrs_len);
    query.push(format!(
        ") AS {t2} ON {t1}.{join_id} = {t2}.{join_id}
    GROUP BY {t1}.{join_id}
    HAVING COUNT(*) = "
    ));
    query.push_bind(attrs_len);

    let query = query.build_query_as();
    let ids = query
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("get_measurement_profile_id_by_attrs", e))?;

    Ok(ids)
}

fn where_attr_pairs(query: &mut QueryBuilder<Postgres>, values: &HashMap<String, String>) {
    query.push("where (key, value) in (");
    for (index, (key, value)) in values.iter().enumerate() {
        query.push("(");
        query.push_bind(key.clone());
        query.push(",");
        query.push_bind(value.clone());
        query.push(")");
        if index < values.len() - 1 {
            query.push(", ");
        }
    }
    query.push(") ");
}

/// get_measurement_profile_attrs_for_profile_id returns all profile attribute
/// records associated with the provided MeasurementSystemProfileId.
pub async fn get_measurement_profile_attrs_for_profile_id(
    txn: impl DbReader<'_>,
    profile_id: MeasurementSystemProfileId,
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    common::get_objects_where_id(txn, profile_id)
        .await
        .map_err(|e| e.with_op_name("get_measurement_profile_attrs_for_profile_id"))
}

/// get_bundles_for_profile_id returns a unique list of all
/// MeasurementBundleId that leverage the given profile.
///
/// This is specifically used by the `profile list bundles for-id` CLI call.
pub async fn get_bundles_for_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Vec<MeasurementBundleId>, DatabaseError> {
    let query = "select distinct bundle_id from measurement_bundles where profile_id = $1 order by bundle_id";
    sqlx::query_as(query)
        .bind(profile_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_bundles_for_profile_id", e))
}

/// get_bundles_for_profile_name returns a unique list of all
/// MeasurementBundleId that leverage the given profile.
///
/// This is specifically used by the `profile list bundles for-name` CLI call.
pub async fn get_bundles_for_profile_name(
    txn: &mut PgConnection,
    profile_name: String,
) -> Result<Vec<MeasurementBundleId>, DatabaseError> {
    let query =
        "select distinct bundle_id from measurement_bundles where name = $1 order by bundle_id";
    sqlx::query_as(query)
        .bind(profile_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_bundles_for_profile_name", e))
}

/// get_machines_for_profile_id returns a unique list of all MachineId
/// that leverage the given profile.
///
/// This is specifically used by the `profile list machines by-id` CLI call.
pub async fn get_machines_for_profile_id(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = "select distinct machine_id from measurement_journal where profile_id = $1 order by machine_id";
    sqlx::query_as(query)
        .bind(profile_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_machines_for_profile_id", e))
}

/// get_machines_for_profile_name returns a unique list of all CandidateMachineId
/// that leverage the given profile.
///
/// This is specifically used by the `profile list machines by-name` CLI call.
pub async fn get_machines_for_profile_name(
    txn: &mut PgConnection,
    profile_name: String,
) -> Result<Vec<MachineId>, DatabaseError> {
    let query = "select distinct machine_id from measurement_journal,measurement_system_profiles where measurement_journal.profile_id=measurement_system_profiles.profile_id and measurement_system_profiles.name = $1 order by machine_id";
    sqlx::query_as(query)
        .bind(profile_name)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_machines_for_profile_name", e))
}

/// import_measurement_system_profiles takes a vector of MeasurementSystemProfileRecord
/// and calls import_measurement_profile for each of them.
///
/// This is used for doing full site imports, and is wrapped in a transaction
/// such that, if any of it fails, none of it will be committed.
pub async fn import_measurement_system_profiles(
    txn: &mut PgConnection,
    records: &[MeasurementSystemProfileRecord],
) -> Result<Vec<MeasurementSystemProfileRecord>, DatabaseError> {
    let mut committed = Vec::<MeasurementSystemProfileRecord>::new();
    for record in records.iter() {
        committed.push(import_measurement_profile(&mut *txn, record).await?);
    }
    Ok(committed)
}

/// import_measurement_profile inserts a single MeasurementSystemProfileRecord.
///
/// This is used for doing full site imports, and the intent is that this
/// is called by import_measurement_system_profiles as part of inserting a bunch
/// of measurement profiles.
///
/// After a MeasurementSystemProfileRecord gets inserted, its clear for having
/// all of its MeasurementSystemProfileAttrRecord records inserted.
pub async fn import_measurement_profile(
    txn: &mut PgConnection,
    profile: &MeasurementSystemProfileRecord,
) -> Result<MeasurementSystemProfileRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_system_profiles(profile_id, name, ts) values($1, $2, $3) returning {}",
        MeasurementSystemProfileRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(profile.profile_id)
        .bind(&profile.name)
        .bind(profile.ts)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("import_measurement_profile", e))
}

/// import_measurement_system_profiles_attrs inserts a bunch of measurement profile
/// attributes as part of doing a site import.
///
/// It is expected that the measurement profile itself (as in, the parent
/// MeasurementSystemProfileRecord) exists before the attributes are added, since
/// it would fail foreign key constraints otherwise.
pub async fn import_measurement_system_profiles_attrs(
    txn: &mut PgConnection,
    records: &[MeasurementSystemProfileAttrRecord],
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    let mut committed = Vec::<MeasurementSystemProfileAttrRecord>::new();
    for record in records.iter() {
        committed.push(import_measurement_system_profiles_attr(&mut *txn, record).await?);
    }
    Ok(committed)
}

/// import_measurement_system_profiles_attr imports a single measurement profile
/// attribute.
///
/// The idea is import_measurement_system_profiles_attrs has all of the attributes,
/// and then calls this for each attribute.
pub async fn import_measurement_system_profiles_attr(
    txn: &mut PgConnection,
    bundle: &MeasurementSystemProfileAttrRecord,
) -> Result<MeasurementSystemProfileAttrRecord, DatabaseError> {
    let query = format!(
        "insert into measurement_system_profiles_attrs(attribute_id, profile_id, key, value, ts) values($1, $2, $3, $4, $5) returning {}",
        MeasurementSystemProfileAttrRecord::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(bundle.attribute_id)
        .bind(bundle.profile_id)
        .bind(&bundle.key)
        .bind(&bundle.value)
        .bind(bundle.ts)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("import_measurement_system_profiles_attr", e))
}

/// export_measurement_profile_records returns all MeasurementSystemProfileRecord
/// instances in the database.
///
/// This is used by the site exporter, as well as for listing all profiles.
pub async fn export_measurement_profile_records(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementSystemProfileRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("export_measurement_profile_records"))
}

/// export_measurement_system_profiles_attrs returns all MeasurementSystemProfileAttrRecord
/// instances in the database.
///
/// This is specifically used by the site exporter, since we simply dump all
/// attributes when doing a site export.
pub async fn export_measurement_system_profiles_attrs(
    txn: impl DbReader<'_>,
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("export_measurement_system_profiles_attrs"))
}

/// get_all_measurement_profile_attr_records gets all system profile
/// attribute records.
pub async fn get_all_measurement_profile_attr_records(
    txn: &mut PgConnection,
) -> Result<Vec<MeasurementSystemProfileAttrRecord>, DatabaseError> {
    common::get_all_objects(txn)
        .await
        .map_err(|e| e.with_op_name("get_all_measurement_profile_attr_records"))
}
