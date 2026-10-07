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
use std::net::IpAddr;

use model::route_server::{RouteServer, RouteServerSourceType};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{BIND_LIMIT, DatabaseError, DatabaseResult};

// replace will replace all addresses for the given source type
// in the database with whatever new addresses are provided.
pub async fn replace(
    txn: &mut PgConnection,
    addresses: &[IpAddr],
    source_type: RouteServerSourceType,
) -> DatabaseResult<()> {
    // len * 2 since we're going to be binding address + source_type
    // for each address when we do an insert below.
    if addresses.len() * 2 > BIND_LIMIT {
        return Err(DatabaseError::InvalidArgument(format!(
            "super::replace: {} addresses exceeds bind limit ({})",
            addresses.len(),
            BIND_LIMIT
        )));
    }

    // Delete all existing entries for this source type
    sqlx::query(r#"DELETE FROM route_servers WHERE source_type = $1"#)
        .bind(source_type)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::new("super::replace delete", e))?;

    // Insert all new entries (if any)
    if !addresses.is_empty() {
        let query = r#"INSERT INTO route_servers (address, source_type) "#;
        let mut qb = sqlx::QueryBuilder::new(query);

        qb.push_values(addresses.iter(), |mut b, v| {
            b.push_bind(v).push_bind(source_type);
        });

        qb.build()
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::new("super::replace insert", e))?;
    }

    Ok(())
}

// get returns all RouteServer entries, which include the
// IP address and source_type of the entry.
pub async fn get(txn: impl DbReader<'_>) -> DatabaseResult<Vec<RouteServer>> {
    let query = r#"SELECT address, source_type FROM route_servers;"#;
    sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

// find_by_address returns a RouteServer entry matching
// the provided address, if it exists.
pub async fn find_by_address(
    txn: impl DbReader<'_>,
    address: IpAddr,
) -> DatabaseResult<Option<RouteServer>> {
    let query = r#"SELECT address, source_type FROM route_servers where address=$1;"#;
    sqlx::query_as(query)
        .bind(address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

// add will insert a list of route server IP addresses
// for the specified source_type.
pub async fn add(
    txn: &mut PgConnection,
    addresses: &[IpAddr],
    source_type: RouteServerSourceType,
) -> DatabaseResult<()> {
    // len * 2 since we're going to be binding address + source_type
    // for each address when we do an insert below.
    if addresses.len() * 2 > BIND_LIMIT {
        return Err(DatabaseError::InvalidArgument(format!(
            "super::add: {} addresses exceeds bind limit ({})",
            addresses.len(),
            BIND_LIMIT
        )));
    } else if !addresses.is_empty() {
        let query = r#"INSERT INTO route_servers (address, source_type) "#;
        let mut qb = sqlx::QueryBuilder::new(query);

        qb.push_values(addresses.iter(), |mut b, v| {
            b.push_bind(v).push_bind(source_type);
        });
        let query = qb.build();

        query
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::new("super::add", e))?;
    }
    Ok(())
}

// remove deletes route server addresses matching the input
// list of IP addresses for the given input source_type.
// Returns the number of rows deleted.
pub async fn remove(
    txn: &mut PgConnection,
    addresses: &[IpAddr],
    source_type: RouteServerSourceType,
) -> DatabaseResult<u64> {
    // len + 1 since we're going to be binding all addresses
    // plus the source_type when we do the delete below.
    if addresses.len() + 1 > BIND_LIMIT {
        return Err(DatabaseError::InvalidArgument(format!(
            "super::remove: {} addresses exceeds bind limit ({})",
            addresses.len(),
            BIND_LIMIT
        )));
    } else if !addresses.is_empty() {
        let query = r#"DELETE FROM route_servers WHERE address = ANY($1) AND source_type = $2;"#;
        let result = sqlx::query(query)
            .bind(addresses)
            .bind(source_type)
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::new("super::remove", e))?;
        return Ok(result.rows_affected());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::Connection;

    use super::*;

    // test_ips is a helper function to create test IP addresses,
    // instead of listing all of the IPs to test in a vec, since
    // that happens a lot throughout the tests.
    fn test_ips(count: usize) -> Vec<IpAddr> {
        (1..=count)
            .map(|i| IpAddr::from_str(&format!("192.168.1.{i}")).unwrap())
            .collect()
    }

    // too_many_ips is a helper function to create a large array
    // of IPs exceeding BIND_LIMIT, since most of the calls in
    // this module check BIND_LIMIT before doing things with
    // the input addresses + query builder.
    fn too_many_ips() -> Vec<IpAddr> {
        (1..=BIND_LIMIT / 2 + 1)
            .map(|i| (i as u32).to_be_bytes())
            .map(std::net::Ipv4Addr::from)
            .map(IpAddr::V4)
            .collect()
    }

    // test_sync_with_addresses tests to make sure sync
    // properly adds new addresses for a given source type.
    #[crate::sqlx_test]
    async fn test_sync_with_addresses(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let addresses = test_ips(3);

        // Initial sync
        super::replace(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await?;

        let result = super::get(txn.as_mut()).await?;
        assert_eq!(result.len(), 3);

        let result_addresses: Vec<IpAddr> = result.iter().map(|rs| rs.address).collect();
        assert_eq!(result_addresses, addresses);

        // All should have ConfigFile source type
        assert!(
            result
                .iter()
                .all(|rs| rs.source_type == RouteServerSourceType::ConfigFile)
        );

        Ok(())
    }

    // test_sync_without_addresses_wipes_source_type tests to make
    // sure syncing with an empty array removes all entries for that
    // source type while preserving other source types.
    #[crate::sqlx_test]
    async fn test_sync_without_addresses_wipes_source_type(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Add some addresses for both source types
        let config_addresses = test_ips(3);
        let admin_addresses = vec![IpAddr::from_str("10.0.0.1")?, IpAddr::from_str("10.0.0.2")?];

        super::replace(
            &mut txn,
            &config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;
        super::replace(&mut txn, &admin_addresses, RouteServerSourceType::AdminApi).await?;

        // Verify we have 5 total entries
        let all_entries = super::get(txn.as_mut()).await?;
        assert_eq!(all_entries.len(), 5);

        // Sync empty array for ConfigFile - should only remove ConfigFile entries
        super::replace(&mut txn, &[], RouteServerSourceType::ConfigFile).await?;

        let remaining_entries = super::get(txn.as_mut()).await?;
        assert_eq!(remaining_entries.len(), 2);

        // All remaining should be AdminApi
        assert!(
            remaining_entries
                .iter()
                .all(|rs| rs.source_type == RouteServerSourceType::AdminApi)
        );

        Ok(())
    }

    // test_sync_with_too_many_addresses tests to make sure sync
    // rejects input arrays that exceed the BIND_LIMIT.
    #[crate::sqlx_test]
    async fn test_sync_with_too_many_addresses(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let too_many = too_many_ips();

        let result = super::replace(&mut txn, &too_many, RouteServerSourceType::ConfigFile).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            DatabaseError::InvalidArgument(msg) => {
                assert!(msg.contains("exceeds bind limit"));
            }
            _ => panic!("Expected InvalidArgument error"),
        }

        Ok(())
    }

    // test_add tests to make sure add properly inserts new
    // route server addresses with the correct source type.
    #[crate::sqlx_test]
    async fn test_add(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let addresses = test_ips(3);

        super::add(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await?;

        let result = super::get(txn.as_mut()).await?;
        assert_eq!(result.len(), 3);

        let result_addresses: Vec<IpAddr> = result.iter().map(|rs| rs.address).collect();
        assert_eq!(result_addresses, addresses);

        Ok(())
    }

    // test_duplicate_add_same_source_type tests to make sure
    // adding duplicate addresses for the same source type fails
    // with a database constraint error.
    #[crate::sqlx_test]
    async fn test_duplicate_add_same_source_type(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let addresses = test_ips(2);

        // First add
        super::add(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await?;

        // Second add with same addresses and source type - should fail
        let result = super::add(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await;
        assert!(result.is_err());

        Ok(())
    }

    // test_duplicate_add_different_source_type tests to make
    // sure adding duplicate addresses across different source
    // types fails due to unique address constraint.
    #[crate::sqlx_test]
    async fn test_duplicate_add_different_source_type(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let addresses = test_ips(2);

        // Add with ConfigFile source type
        super::add(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await?;

        // Try to add same addresses with AdminApi source type - should fail due to unique constraint on address
        let result = super::add(&mut txn, &addresses, RouteServerSourceType::AdminApi).await;
        assert!(result.is_err());

        Ok(())
    }

    // test_remove tests to make sure remove only deletes
    // the specified addresses for the given source type
    // without affecting other entries.
    #[crate::sqlx_test]
    async fn test_remove(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Add addresses for both source types
        let config_addresses = test_ips(3);
        let admin_addresses = vec![IpAddr::from_str("10.0.0.1")?, IpAddr::from_str("10.0.0.2")?];

        super::add(
            &mut txn,
            &config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;
        super::add(&mut txn, &admin_addresses, RouteServerSourceType::AdminApi).await?;

        // Remove some ConfigFile addresses
        let to_remove = vec![config_addresses[0], config_addresses[1]];
        super::remove(&mut txn, &to_remove, RouteServerSourceType::ConfigFile).await?;

        let remaining = super::get(txn.as_mut()).await?;
        assert_eq!(remaining.len(), 3); // 1 ConfigFile + 2 AdminApi

        // Verify correct entries were removed
        let remaining_addresses: Vec<IpAddr> = remaining.iter().map(|rs| rs.address).collect();
        assert!(remaining_addresses.contains(&config_addresses[2])); // Last ConfigFile entry should remain
        assert!(remaining_addresses.contains(&admin_addresses[0])); // AdminApi entries should remain
        assert!(remaining_addresses.contains(&admin_addresses[1]));
        assert!(!remaining_addresses.contains(&config_addresses[0])); // Should be removed
        assert!(!remaining_addresses.contains(&config_addresses[1])); // Should be removed

        Ok(())
    }

    // test_get tests to make sure get returns all route
    // server entries from all source types, including when
    // the table is empty.
    #[crate::sqlx_test]
    async fn test_get(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Test get when empty
        let empty_result = super::get(txn.as_mut()).await?;
        assert_eq!(empty_result.len(), 0);

        // Add some data
        let config_addresses = test_ips(2);
        let admin_addresses = vec![IpAddr::from_str("10.0.0.1")?];

        super::add(
            &mut txn,
            &config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;
        super::add(&mut txn, &admin_addresses, RouteServerSourceType::AdminApi).await?;

        // Test get returns all entries
        let result = super::get(txn.as_mut()).await?;
        assert_eq!(result.len(), 3);

        // Verify source types are correct
        let config_count = result
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::ConfigFile)
            .count();
        let admin_count = result
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::AdminApi)
            .count();
        assert_eq!(config_count, 2);
        assert_eq!(admin_count, 1);

        Ok(())
    }

    // test_sync_with_duplicate_addresses_in_input pins the contract that
    // `replace` does not de-duplicate its input: addresses are handed straight
    // to a single batched INSERT, so a repeated address trips the
    // `address inet NOT NULL UNIQUE` constraint and the whole call is rejected.
    // Callers are responsible for passing a unique address set.
    #[crate::sqlx_test]
    async fn test_sync_with_duplicate_addresses_in_input(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Input carrying the same address twice.
        let mut addresses = test_ips(3);
        addresses.push(addresses[0]);

        let result = super::replace(&mut txn, &addresses, RouteServerSourceType::ConfigFile).await;

        // The duplicate must trip the UNIQUE constraint specifically, not merely
        // fail for some unrelated reason.
        match result {
            Err(DatabaseError::Sqlx(e)) if matches!(&e.source, sqlx::Error::Database(db) if db.is_unique_violation()) =>
                {}
            other => panic!(
                "replace should reject a duplicate address with a unique-violation error, got: {other:?}"
            ),
        }

        Ok(())
    }

    // test_multi_source_isolation tests to make sure operations
    // on one source type do not affect entries from other source types.
    #[crate::sqlx_test]
    async fn test_multi_source_isolation(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let config_addresses = test_ips(3);
        let admin_addresses = vec![IpAddr::from_str("10.0.0.1")?, IpAddr::from_str("10.0.0.2")?];

        // Add for both source types
        super::replace(
            &mut txn,
            &config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;
        super::replace(&mut txn, &admin_addresses, RouteServerSourceType::AdminApi).await?;

        // Sync ConfigFile with new addresses - should not affect AdminApi
        let new_config_addresses = vec![
            IpAddr::from_str("172.16.0.1")?,
            IpAddr::from_str("172.16.0.2")?,
        ];
        super::replace(
            &mut txn,
            &new_config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;

        let all_entries = super::get(txn.as_mut()).await?;

        // Should have 2 new ConfigFile + 2 AdminApi = 4 total
        assert_eq!(all_entries.len(), 4);

        let config_entries: Vec<_> = all_entries
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::ConfigFile)
            .collect();
        let admin_entries: Vec<_> = all_entries
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::AdminApi)
            .collect();

        assert_eq!(config_entries.len(), 2);
        assert_eq!(admin_entries.len(), 2);

        // Verify AdminApi entries are unchanged
        let admin_addresses_result: Vec<IpAddr> =
            admin_entries.iter().map(|rs| rs.address).collect();
        assert_eq!(admin_addresses_result, admin_addresses);

        Ok(())
    }

    // test_add_too_many_addresses tests to make sure add
    // rejects input arrays that exceed the BIND_LIMIT.
    #[crate::sqlx_test]
    async fn test_add_too_many_addresses(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let too_many = too_many_ips();

        let result = super::add(&mut txn, &too_many, RouteServerSourceType::ConfigFile).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            DatabaseError::InvalidArgument(msg) => {
                assert!(msg.contains("exceeds bind limit"));
            }
            _ => panic!("Expected InvalidArgument error"),
        }

        Ok(())
    }

    // test_remove_only_affects_specified_source_type tests to
    // make sure remove operations are isolated to the specified
    // source type and don't affect other source types.
    #[crate::sqlx_test]
    async fn test_remove_only_affects_specified_source_type(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Note: Since addresses must be unique across source types, we use different IPs
        let config_addresses = test_ips(3);
        let admin_addresses = vec![IpAddr::from_str("10.0.0.1")?, IpAddr::from_str("10.0.0.2")?];

        super::add(
            &mut txn,
            &config_addresses,
            RouteServerSourceType::ConfigFile,
        )
        .await?;
        super::add(&mut txn, &admin_addresses, RouteServerSourceType::AdminApi).await?;

        // Try to remove an AdminApi address using ConfigFile source type - should not remove anything
        let to_remove = vec![admin_addresses[0]];
        let deleted =
            super::remove(&mut txn, &to_remove, RouteServerSourceType::ConfigFile).await?;
        assert_eq!(deleted, 0);

        let remaining = super::get(txn.as_mut()).await?;
        assert_eq!(remaining.len(), 5); // All entries should remain

        // Now remove with correct source type
        let deleted = super::remove(&mut txn, &to_remove, RouteServerSourceType::AdminApi).await?;
        assert_eq!(deleted, 1);

        let remaining = super::get(txn.as_mut()).await?;
        assert_eq!(remaining.len(), 4); // One entry should be removed

        Ok(())
    }

    // test_data_integrity_after_multiple_operations tests to make sure
    // the database maintains correct state after a complex sequence of
    // sync, add, and remove operations.
    #[crate::sqlx_test]
    async fn test_data_integrity_after_multiple_operations(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        // Complex sequence of operations
        let initial_config = test_ips(3);
        let initial_admin = vec![IpAddr::from_str("10.0.0.1")?];

        // Initial setup
        super::replace(&mut txn, &initial_config, RouteServerSourceType::ConfigFile).await?;
        super::replace(&mut txn, &initial_admin, RouteServerSourceType::AdminApi).await?;

        // Modify ConfigFile
        let new_config = vec![
            initial_config[0],               // Keep one
            IpAddr::from_str("172.16.0.1")?, // Add new one
        ];
        super::replace(&mut txn, &new_config, RouteServerSourceType::ConfigFile).await?;

        // Add more AdminApi
        let additional_admin = vec![IpAddr::from_str("10.0.0.2")?];
        super::add(&mut txn, &additional_admin, RouteServerSourceType::AdminApi).await?;

        // Remove one AdminApi
        super::remove(&mut txn, &initial_admin, RouteServerSourceType::AdminApi).await?;

        // Final verification
        let final_entries = super::get(txn.as_mut()).await?;
        assert_eq!(final_entries.len(), 3); // 2 ConfigFile + 1 AdminApi

        let config_final: Vec<IpAddr> = final_entries
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::ConfigFile)
            .map(|rs| rs.address)
            .collect();
        let admin_final: Vec<IpAddr> = final_entries
            .iter()
            .filter(|rs| rs.source_type == RouteServerSourceType::AdminApi)
            .map(|rs| rs.address)
            .collect();

        assert_eq!(config_final.len(), 2);
        assert_eq!(admin_final, additional_admin);
        assert!(config_final.contains(&initial_config[0]));
        assert!(config_final.contains(&IpAddr::from_str("172.16.0.1")?));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn route_server_queries_survive_added_columns(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut api_connection = pool.acquire().await?;
        let entries = [
            ("192.0.2.78".parse()?, RouteServerSourceType::ConfigFile),
            ("2001:db8::78".parse()?, RouteServerSourceType::AdminApi),
        ];
        let mut txn = api_connection.begin().await?;
        for (address, source_type) in entries {
            add(txn.as_mut(), &[address], source_type).await?;
        }
        txn.commit().await?;

        assert_route_server_queries(&mut api_connection, &entries).await?;
        assert!(api_connection.cached_statements_size() > 0);

        // Keep the prepared readers alive while an unrelated column is added.
        let mut migration = pool.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL lock_timeout = '5s';
             ALTER TABLE route_servers ADD COLUMN test_added_column text;",
        )
        .execute(&mut *migration)
        .await?;
        migration.commit().await?;

        assert_route_server_queries(&mut api_connection, &entries).await?;
        Ok(())
    }

    async fn assert_route_server_queries(
        connection: &mut PgConnection,
        expected: &[(IpAddr, RouteServerSourceType)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let found = get(&mut *connection).await?;
        assert_eq!(found.len(), expected.len());
        for entry in found {
            assert!(expected.contains(&(entry.address, entry.source_type)));
        }
        for (address, source_type) in expected {
            let found = find_by_address(&mut *connection, *address)
                .await?
                .expect("the committed route server is found");
            assert_eq!(found.address, *address);
            assert_eq!(found.source_type, *source_type);
        }
        Ok(())
    }
}
