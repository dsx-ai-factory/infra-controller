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

use carbide_test_harness::prelude::*;
use carbide_uuid::power_shelf::PowerShelfId;
use model::test_support::power_shelf_config;
use rpc::forge::PowerShelfQuery;

use crate::power_shelf::create_custom_power_shelf;

#[sqlx_test]
async fn test_find_power_shelf_ids_and_by_ids(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestPowerShelf { id: ps_id1 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 1"))
        .await;
    let TestPowerShelf { id: ps_id2 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 2"))
        .await;

    // FindPowerShelfIds should return both power shelves
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(power_shelf_ids.contains(&ps_id1));
    assert!(power_shelf_ids.contains(&ps_id2));

    // FindPowerShelvesByIds should return the requested power shelf
    let power_shelves = env
        .api()
        .find_power_shelves_by_ids(tonic::Request::new(rpc::forge::PowerShelvesByIdsRequest {
            power_shelf_ids: vec![ps_id1],
        }))
        .await?
        .into_inner()
        .power_shelves;
    assert_eq!(power_shelves.len(), 1);
    assert_eq!(power_shelves[0].id, Some(ps_id1));

    // FindPowerShelvesByIds should return both when requested
    let power_shelves = env
        .api()
        .find_power_shelves_by_ids(tonic::Request::new(rpc::forge::PowerShelvesByIdsRequest {
            power_shelf_ids: vec![ps_id1, ps_id2],
        }))
        .await?
        .into_inner()
        .power_shelves;
    assert_eq!(power_shelves.len(), 2);

    Ok(())
}

// The empty-list and over-max guards for `find_power_shelves_by_ids` are shared
// API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.

#[sqlx_test]
async fn test_find_power_shelf_ids_excludes_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestPowerShelf { id: ps_id1 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 1"))
        .await;
    let TestPowerShelf { id: ps_id2 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 2"))
        .await;

    // Delete ps2
    env.api()
        .delete_power_shelf(tonic::Request::new(rpc::forge::PowerShelfDeletionRequest {
            id: Some(ps_id2),
        }))
        .await?;

    // FindPowerShelfIds should only return the non-deleted power shelf
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(power_shelf_ids.contains(&ps_id1));
    assert!(!power_shelf_ids.contains(&ps_id2));

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelf_ids_deleted_only(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestPowerShelf { id: ps_id1 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 1"))
        .await;
    let TestPowerShelf { id: ps_id2 } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf 2"))
        .await;

    env.api()
        .delete_power_shelf(tonic::Request::new(rpc::forge::PowerShelfDeletionRequest {
            id: Some(ps_id2),
        }))
        .await?;

    // DELETED_FILTER_ONLY (1) should return only the deleted power shelf
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            deleted: 1,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(!power_shelf_ids.contains(&ps_id1));
    assert!(power_shelf_ids.contains(&ps_id2));

    // DELETED_FILTER_INCLUDE (2) should return both
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            deleted: 2,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(power_shelf_ids.contains(&ps_id1));
    assert!(power_shelf_ids.contains(&ps_id2));

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelf_ids_by_controller_state(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestPowerShelf { id: ps_id } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf"))
        .await;

    // New power shelves start in "initializing" state
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            controller_state: Some("initializing".to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(power_shelf_ids.contains(&ps_id));

    // Filter for a state that doesn't match
    let power_shelf_ids = env
        .api()
        .find_power_shelf_ids(tonic::Request::new(rpc::forge::PowerShelfSearchFilter {
            controller_state: Some("ready".to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner()
        .ids;
    assert!(!power_shelf_ids.contains(&ps_id));

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelves_by_ids_response_fields(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let TestPowerShelf { id: ps_id } = env
        .create_power_shelf(power_shelf_config("Test Power Shelf"))
        .await;

    let power_shelves = env
        .api()
        .find_power_shelves_by_ids(tonic::Request::new(rpc::forge::PowerShelvesByIdsRequest {
            power_shelf_ids: vec![ps_id],
        }))
        .await?
        .into_inner()
        .power_shelves;
    assert_eq!(power_shelves.len(), 1);

    let ps = &power_shelves[0];

    // controller_state should be populated both on the top-level and in status
    assert!(!ps.controller_state.is_empty());
    let status = ps.status.as_ref().expect("status should be present");
    assert_eq!(
        status.controller_state.as_deref(),
        Some(ps.controller_state.as_str()),
    );

    // state_version should be populated
    assert!(!ps.state_version.is_empty());

    // bmc_info is None when no machine_interface discovery data exists
    assert!(
        ps.bmc_info.is_none(),
        "bmc_info should be None when no discovery data exists"
    );

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelf_by_id(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Find Test Power Shelf", None, None).await?;

    let find_response = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?;

    let power_shelf_list = find_response.into_inner();
    assert_eq!(power_shelf_list.power_shelves.len(), 1);

    let found_power_shelf = &power_shelf_list.power_shelves[0];
    assert_eq!(
        found_power_shelf.id.as_ref().unwrap().to_string(),
        power_shelf_id.to_string()
    );
    assert_eq!(
        found_power_shelf.config.as_ref().unwrap().name,
        "Find Test Power Shelf"
    );

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelf_not_found(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let find_response = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(non_existent_id),
        }))
        .await?;

    let power_shelf_list = find_response.into_inner();
    assert_eq!(power_shelf_list.power_shelves.len(), 0);

    Ok(())
}

#[sqlx_test]
async fn test_find_power_shelf_all(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    for (name, capacity, voltage) in [
        ("Power Shelf 1", 5000, 240),
        ("Power Shelf 2", 3000, 120),
        ("Power Shelf 3", 4000, 208),
    ] {
        create_custom_power_shelf(&env, name, Some(capacity), Some(voltage)).await?;
    }

    let find_response = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: None,
        }))
        .await?;

    let power_shelf_list = find_response.into_inner();
    assert_eq!(power_shelf_list.power_shelves.len(), 3);

    let names: Vec<String> = power_shelf_list
        .power_shelves
        .iter()
        .map(|ps| ps.config.as_ref().unwrap().name.clone())
        .collect();

    assert!(names.contains(&"Power Shelf 1".to_string()));
    assert!(names.contains(&"Power Shelf 2".to_string()));
    assert!(names.contains(&"Power Shelf 3".to_string()));

    Ok(())
}
