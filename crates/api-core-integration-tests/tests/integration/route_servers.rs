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
use std::str::FromStr;

use carbide_test_harness::prelude::*;
use itertools::Itertools;
use model::route_server::{RouteServer, RouteServerSourceType};
use rpc::forge::{RouteServerSourceType as RouteServerSourceTypePb, RouteServers};

#[sqlx_test]
async fn test_add_route_servers(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let expected_servers = [
        IpAddr::from_str("1.2.3.4")?,
        IpAddr::from_str("2.3.4.5")?,
        IpAddr::from_str("3.4.5.6")?,
    ];

    let request = tonic::Request::new(RouteServers {
        route_servers: expected_servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().add_route_servers(request).await?;

    let mut txn = env.db_txn().await;
    let query = r#"SELECT * from route_servers;"#;
    let actual_servers: Vec<IpAddr> = sqlx::query_as::<_, RouteServer>(query)
        .fetch_all(&mut *txn)
        .await?
        .into_iter()
        .map(|rs| rs.address)
        .collect();

    assert_eq!(actual_servers, expected_servers);
    txn.rollback().await?;
    Ok(())
}

#[sqlx_test]
async fn test_remove_route_servers(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let mut expected_servers = vec![
        IpAddr::from_str("1.2.3.4")?,
        IpAddr::from_str("2.3.4.5")?,
        IpAddr::from_str("3.4.5.6")?,
    ];

    let request: tonic::Request<RouteServers> = tonic::Request::new(RouteServers {
        route_servers: expected_servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().add_route_servers(request).await?;

    let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.db_txn().await;
    let query = r#"SELECT * from route_servers;"#;
    let actual_servers: Vec<IpAddr> = sqlx::query_as::<_, RouteServer>(query)
        .fetch_all(&mut *txn)
        .await?
        .into_iter()
        .map(|rs| rs.address)
        .collect();

    assert_eq!(actual_servers, expected_servers);
    txn.rollback().await?;

    let removed_servers = [expected_servers.pop().unwrap()];
    let request: tonic::Request<RouteServers> = tonic::Request::new(RouteServers {
        route_servers: removed_servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().remove_route_servers(request).await?;
    let mut txn = env.db_txn().await;
    let query = r#"SELECT * from route_servers;"#;
    let actual_servers: Vec<IpAddr> = sqlx::query_as::<_, RouteServer>(query)
        .fetch_all(&mut *txn)
        .await?
        .into_iter()
        .map(|rs| rs.address)
        .collect();

    assert_eq!(actual_servers, expected_servers);
    txn.rollback().await?;

    let request: tonic::Request<RouteServers> = tonic::Request::new(RouteServers {
        route_servers: expected_servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().remove_route_servers(request).await?;
    let mut txn = env.db_txn().await;
    let query = r#"SELECT * from route_servers;"#;
    let actual_servers: Vec<IpAddr> = sqlx::query_as::<_, RouteServer>(query)
        .fetch_all(&mut *txn)
        .await?
        .into_iter()
        .map(|rs| rs.address)
        .collect();

    assert!(actual_servers.is_empty());
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn test_remove_route_servers_wrong_source_type(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let servers = [IpAddr::from_str("1.2.3.4")?];

    // Add a server with ConfigFile source type
    let request = tonic::Request::new(RouteServers {
        route_servers: servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::ConfigFile as i32,
    });
    env.api().add_route_servers(request).await?;

    // Try to remove it with AdminApi source type — should return not_found
    let request = tonic::Request::new(RouteServers {
        route_servers: servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });
    let result = env.api().remove_route_servers(request).await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), tonic::Code::NotFound);

    // The row must still be present after the failed removal
    let response = env.api().get_route_servers(tonic::Request::new(())).await?;
    assert_eq!(response.into_inner().route_servers.len(), 1);

    // Removing with the correct source type must succeed and leave the table empty
    let request = tonic::Request::new(RouteServers {
        route_servers: servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::ConfigFile as i32,
    });
    env.api().remove_route_servers(request).await?;

    let response = env.api().get_route_servers(tonic::Request::new(())).await?;
    assert!(response.into_inner().route_servers.is_empty());

    Ok(())
}

#[sqlx_test]
async fn test_initial_set(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let expected_servers = [
        IpAddr::from_str("1.2.3.4")?,
        IpAddr::from_str("2.3.4.5")?,
        IpAddr::from_str("3.4.5.6")?,
    ];

    let set_request = tonic::Request::new(RouteServers {
        route_servers: expected_servers.iter().map(ToString::to_string).collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().replace_route_servers(set_request).await?;

    let mut txn = env.db_txn().await;
    let query = r#"SELECT * from route_servers;"#;
    let actual_servers: Vec<IpAddr> = sqlx::query_as::<_, RouteServer>(query)
        .fetch_all(&mut *txn)
        .await?
        .into_iter()
        .map(|rs| rs.address)
        .collect();

    assert_eq!(actual_servers, expected_servers);
    txn.rollback().await?;
    Ok(())
}

#[sqlx_test]
async fn test_subsequent_replace(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    // Initial test data
    let admin_api_servers = vec![
        IpAddr::from_str("1.2.3.4")?,
        IpAddr::from_str("2.3.4.5")?,
        IpAddr::from_str("3.4.5.6")?,
    ];
    let config_file_servers = vec![
        IpAddr::from_str("7.8.9.10")?,
        IpAddr::from_str("11.12.13.14")?,
    ];

    // Insert initial data
    let mut txn = env.db_txn().await;
    let query = "INSERT INTO route_servers (address, source_type) VALUES ($1, $2)";

    for server in &admin_api_servers {
        sqlx::query(query)
            .bind(server)
            .bind(RouteServerSourceType::AdminApi)
            .execute(&mut *txn)
            .await?;
    }

    for server in &config_file_servers {
        sqlx::query(query)
            .bind(server)
            .bind(RouteServerSourceType::ConfigFile)
            .execute(&mut *txn)
            .await?;
    }

    txn.commit().await?;

    // New AdminApi servers to replace the old ones
    let updated_admin_api_servers = [
        IpAddr::from_str("99.100.101.102")?,
        IpAddr::from_str("103.104.105.106")?,
        IpAddr::from_str("107.108.109.110")?,
    ];

    // Replace only the AdminApi servers
    let replace_request = tonic::Request::new(RouteServers {
        route_servers: updated_admin_api_servers
            .iter()
            .map(ToString::to_string)
            .collect(),
        source_type: RouteServerSourceTypePb::AdminApi as i32,
    });

    env.api().replace_route_servers(replace_request).await?;

    // Check the results
    let response = env.api().get_route_servers(tonic::Request::new(())).await?;
    let actual_servers = response.into_inner().route_servers;

    // Expected addresses should be updated AdminApi + unchanged ConfigFile
    let expected_addresses: Vec<String> = updated_admin_api_servers
        .iter()
        .chain(&config_file_servers)
        .map(ToString::to_string)
        .sorted()
        .collect();

    let actual_addresses: Vec<String> = actual_servers
        .iter()
        .map(|s| s.address.to_string())
        .sorted()
        .collect();

    assert_eq!(actual_addresses, expected_addresses);

    // Verify source types are correct
    let actual_source_types: Vec<i32> = actual_servers
        .iter()
        .map(|s| s.source_type)
        .sorted()
        .collect();

    let expected_source_types: Vec<i32> =
        vec![RouteServerSourceTypePb::AdminApi as i32; updated_admin_api_servers.len()]
            .into_iter()
            .chain(vec![
                RouteServerSourceTypePb::ConfigFile as i32;
                config_file_servers.len()
            ])
            .sorted()
            .collect();

    assert_eq!(actual_source_types, expected_source_types);

    Ok(())
}

#[sqlx_test]
async fn test_get(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    // Test data setup
    let admin_api_servers = vec![
        IpAddr::from_str("1.2.3.4")?,
        IpAddr::from_str("2.3.4.5")?,
        IpAddr::from_str("3.4.5.6")?,
    ];
    let config_file_servers = vec![
        IpAddr::from_str("7.8.9.10")?,
        IpAddr::from_str("11.12.13.14")?,
    ];

    // Insert test data
    let mut txn = env.db_txn().await;
    let query = "INSERT INTO route_servers (address, source_type) VALUES ($1, $2)";

    for server in &admin_api_servers {
        sqlx::query(query)
            .bind(server)
            .bind(RouteServerSourceType::AdminApi)
            .execute(&mut *txn)
            .await?;
    }

    for server in &config_file_servers {
        sqlx::query(query)
            .bind(server)
            .bind(RouteServerSourceType::ConfigFile)
            .execute(&mut *txn)
            .await?;
    }

    txn.commit().await?;

    // Test the API
    let response = env.api().get_route_servers(tonic::Request::new(())).await?;
    let actual_servers = response.into_inner().route_servers;

    // Verify addresses (sorted)
    let actual_addresses: Vec<String> = actual_servers
        .iter()
        .map(|s| s.address.clone())
        .sorted()
        .collect();

    let expected_addresses: Vec<String> = admin_api_servers
        .iter()
        .chain(&config_file_servers)
        .map(ToString::to_string)
        .sorted()
        .collect();

    assert_eq!(actual_addresses, expected_addresses);

    // Verify source types (sorted to match API response order)
    let actual_source_types: Vec<i32> = actual_servers
        .iter()
        .map(|s| s.source_type)
        .sorted()
        .collect();

    let expected_source_types: Vec<i32> =
        vec![RouteServerSourceTypePb::AdminApi as i32; admin_api_servers.len()]
            .into_iter()
            .chain(vec![
                RouteServerSourceTypePb::ConfigFile as i32;
                config_file_servers.len()
            ])
            .sorted()
            .collect();

    assert_eq!(actual_source_types, expected_source_types);

    Ok(())
}
