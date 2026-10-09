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
 *  Common code for working with the database. Provides constants and generics
 *  for making boilerplate copy-pasta code handled in a common way.
 */

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::DerefMut;
use std::vec::Vec;

use carbide_uuid::{DbPrimaryUuid, DbTable};
use measured_boot::pcr::PcrRegisterValue;
use sqlx::postgres::PgRow;
use sqlx::{Encode, PgConnection, PgTransaction, Postgres};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

// DISCOVERY_PROFILE_ATTRS are the attributes we pull
// from DiscoveryInfo for a given machine when
// auto-generating a SystemProfile. Obviously the actual
// data is buried somewhere in the JSON payload, but
// those values will be pulled and put into a HashMap
// with the following keys leveraging the below
// filter_machine_discovery_attrs function.
pub const DISCOVERY_PROFILE_ATTRS: [&str; 3] = ["sys_vendor", "product_name", "bios_version"];

/// filter_machine_discovery_attrs is used for taking
/// an input DiscoveryInfo and filtering the data out
/// into a HashMap keyed by DISCOVERY_PROFILE_ATTRS.
///
/// If you come across this before it's in production,
/// you'll notice it's mocked to take a HashMap as
/// input, and not a DiscoveryInfo, because I'm just
/// pulling values from the mock attributes table.
pub fn filter_machine_discovery_attrs(
    attrs: &HashMap<String, String>,
) -> DatabaseResult<HashMap<String, String>> {
    let filtered: HashMap<String, String> = attrs
        .iter()
        .filter_map(|(k, v)| {
            if DISCOVERY_PROFILE_ATTRS.contains(&k.as_str()) {
                Some((k.clone(), v.clone()))
            } else {
                None
            }
        })
        .collect();
    Ok(filtered)
}

/// generate_name generates a unique name for the purpose
/// of auto-generated {profile, bundle} names.
pub fn generate_name() -> DatabaseResult<String> {
    let mut generate = names::Generator::default();
    Ok(generate.next().unwrap())
}

/// get_object_for_id provides a generic for getting a fully populated
/// struct (which derives sqlx::FromRow) for a given "ID", where an ID is a
/// struct which derives from a sqlx::types::Uuid (type = UUID).
///
/// See db/model/keys.rs for all of the UUIDs, and db/model/records for all
/// of the structs which derive FromRow.
///
/// And to reduce string literal use even further, both of those implement
/// the DbPrimaryUuid and DbTable traits (which are traits defined in this
/// crate) to build the query.
pub async fn get_object_for_id<T, R>(
    txn: impl DbReader<'_>,
    id: T,
) -> Result<Option<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres> + DbPrimaryUuid,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    get_object_for_unique_column(txn, T::db_primary_uuid_name(), id)
        .await
        .map_err(|e| e.with_op_name("get_object_for_id"))
}

/// get_object_for_unique_column provides a generic for getting a fully
/// populated struct (which derives sqlx::FromRow) for a given uniquely
/// constrained column, where the value derives from an sqlx::types::*.
//
/// And to reduce string literal use even further, both of those implement
/// the DbPrimaryUuid and DbTable traits (which are traits defined in this
/// crate) to build the query.
pub async fn get_object_for_unique_column<T, R>(
    txn: impl DbReader<'_>,
    col_name: &str,
    value: T,
) -> Result<Option<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres>,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    let mut query = sqlx::QueryBuilder::new("select ");
    query.push(R::db_table_columns());
    query.push(" from ");
    query.push(R::db_table_name());
    query.push(" where ");
    query.push(col_name);
    query.push(" = ");
    query.push_bind(value);
    let result = query
        .build_query_as::<R>()
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("get_object_for_unique_column", e))?;
    Ok(result)
}

/// get_objects_where_id returns a vector of records who share a similar
/// `id` in common. The idea here is there is a child table with a foreign key
/// containing a UUID, and we want to get all records mapping back to that
/// UUID in the parent table.
///
/// Similar to get_object_for_id above, this leverages a mixture of sqlx-based
/// derive + traits to reduce the use of copy-pasta code + string literals.
pub async fn get_objects_where_id<T, R>(
    txn: impl DbReader<'_>,
    id: T,
) -> Result<Vec<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres> + DbPrimaryUuid,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    let mut query = sqlx::QueryBuilder::new("select ");
    query.push(R::db_table_columns());
    query.push(" from ");
    query.push(R::db_table_name());
    query.push(" where ");
    query.push(T::db_primary_uuid_name());
    query.push(" = ");
    query.push_bind(id);
    let result = query
        .build_query_as::<R>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_objects_where_id", e))?;
    Ok(result)
}

/// get_all_objects provides a generic way to return populated structs
/// for all records of the given type. Similar to the comments in the two
/// above generics, this leverages structs deriving sqlx::FromRow and
/// implementing the crate-specific DbName trait to make this possible,
/// with the idea of reducing very boilerplate copy-pasta code and string
/// literals.
pub async fn get_all_objects<R>(txn: impl DbReader<'_>) -> Result<Vec<R>, DatabaseError>
where
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    let mut query = sqlx::QueryBuilder::new("select ");
    query.push(R::db_table_columns());
    query.push(" from ");
    query.push(R::db_table_name());
    let result = query
        .build_query_as::<R>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get_all_objects", e))?;
    Ok(result)
}

/// delete_objects_where_id provides a generic way to delete one or more
/// records of a given type, based on a key, and will return the record(s) of
/// what was deleted.
///
/// Similar to the comments in the two above generics, this leverages structs
/// deriving sqlx::FromRow and implementing the crate-specific DbName trait to
/// make this possible, with the idea of reducing very boilerplate copy-pasta
/// code and string literals.
pub async fn delete_objects_where_id<T, R>(
    txn: &mut PgConnection,
    id: T,
) -> Result<Vec<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres> + DbPrimaryUuid,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    delete_objects_where_unique_column(txn, T::db_primary_uuid_name(), id)
        .await
        .map_err(|e| e.with_op_name("delete_objects_where_id"))
}

pub async fn delete_objects_where_unique_column<T, R>(
    txn: &mut PgConnection,
    col_name: &str,
    value: T,
) -> Result<Vec<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres>,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    let mut query = sqlx::QueryBuilder::new("delete from ");
    query.push(R::db_table_name());
    query.push(" where ");
    query.push(col_name);
    query.push(" = ");
    query.push_bind(value);
    query.push(" returning ");
    query.push(R::db_table_columns());
    let result = query
        .build_query_as::<R>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("delete_objects_where_unique_column", e))?;
    Ok(result)
}

/// delete_object_where_id is used for cases where only a single record
/// is expected to be deleted.
pub async fn delete_object_where_id<T, R>(
    txn: &mut PgConnection,
    id: T,
) -> Result<Option<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres> + DbPrimaryUuid,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    delete_object_where_unique_column(txn, T::db_primary_uuid_name(), id)
        .await
        .map_err(|e| e.with_op_name("delete_object_where_id"))
}

pub async fn delete_object_where_unique_column<T, R>(
    txn: &mut PgConnection,
    col_name: &str,
    value: T,
) -> Result<Option<R>, DatabaseError>
where
    T: for<'t> Encode<'t, Postgres> + Send + sqlx::Type<sqlx::Postgres>,
    R: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin + DbTable,
{
    let mut query = sqlx::QueryBuilder::new("delete from ");
    query.push(R::db_table_name());
    query.push(" where ");
    query.push(col_name);
    query.push(" = ");
    query.push_bind(value);
    query.push(" returning ");
    query.push(R::db_table_columns());
    let result = query
        .build_query_as::<R>()
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("delete_object_where_unique_column", e))?;
    Ok(result)
}

/// acquire_advisory_txn_lock acquires, as you'd expect,
/// an advisory lock, which is primarily used for the
/// purpose of ensuring unique/atomic creation of both
/// measurement profiles and measurement bundles. Since
/// bundles and profiles are comprised of multiple rows,
/// we can't really do unique constraints, and I didn't
/// really want to lock the entire table(s), so this is
/// a nice option. The code here is generic, so we could
/// also use it in other places that end up needing it.
///
/// This will block if the lock is currently held, and
/// will wait until it it is released. If you don't want
/// blocking behavior, use try_advisory_lock instead.
///
/// This will also automatically release at the end of
/// the transaction (either commit or rollback), so you
/// don't need to explicitly release the lock when
/// you're done. If you want more control, you can
/// use acquire_advisory_lock + release_advisory_lock.
pub async fn acquire_advisory_txn_lock(
    txn: &mut PgTransaction<'_>,
    key: &str,
) -> Result<(), DatabaseError> {
    let hash_key = advisory_lock_key_to_hash(key);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(hash_key)
        .execute(txn.deref_mut())
        .await
        .map_err(|e| DatabaseError::new("acquire_advisory_txn_lock", e))?;
    Ok(())
}

/// advisory_lock_key_to_hash takes an advisory lock key and
/// converts it into an i64 for the purpose of acquiring or
/// releasing an advisory lock.
fn advisory_lock_key_to_hash(key: &str) -> i64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() as i64
}

pub fn pcr_register_values_to_map(
    values: &[PcrRegisterValue],
) -> DatabaseResult<HashMap<i16, PcrRegisterValue>> {
    let total_values = values.len();
    let value_map: HashMap<i16, PcrRegisterValue> = values
        .iter()
        .map(|rec| (rec.pcr_register, rec.clone()))
        .collect();
    if total_values != value_map.len() {
        return Err(DatabaseError::internal(String::from(
            "detected pcr_register collision in input bundle values",
        )));
    }
    Ok(value_map)
}
