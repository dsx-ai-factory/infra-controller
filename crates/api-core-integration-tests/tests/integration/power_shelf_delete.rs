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
use rpc::forge::{AdminForceDeletePowerShelfRequest, PowerShelfDeletionRequest, PowerShelfQuery};
use tonic::Code;

use crate::power_shelf::create_custom_power_shelf;

#[sqlx_test]
async fn test_delete_power_shelf_success(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Delete Test Power Shelf", Some(5000), Some(240)).await?;

    env.api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(power_shelf_id),
        }))
        .await?;

    // Verify deletion was successful
    // The deletion result is empty, so we just check it doesn't error

    // Soft-deleted power shelves are still returned by this endpoint, with
    // `deleted` populated.
    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await;
    assert!(find_result.is_ok());
    let power_shelf_list = find_result.unwrap().into_inner();

    assert_eq!(
        power_shelf_list.power_shelves.len(),
        1,
        "expected exactly one power shelf after delete"
    );
    let power_shelf = &power_shelf_list.power_shelves[0];
    assert!(
        power_shelf.deleted.is_some(),
        "Power shelf should have a deleted timestamp"
    );

    Ok(())
}

#[sqlx_test]
async fn test_delete_power_shelf_not_found(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let result = env
        .api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(non_existent_id),
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_success(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "ForceDelete Power Shelf", Some(5000), Some(240)).await?;

    // Power shelf state history is retained even when the shelf row itself is
    // hard-deleted.
    let mut txn = env.db_txn().await;
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &power_shelf_id,
        &"retained-before-force-delete",
        config_version::ConfigVersion::initial(),
    )
    .await?;
    txn.commit().await?;

    // Force delete without deleting interfaces.
    let response = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.power_shelf_id, power_shelf_id.to_string());
    assert_eq!(response.interfaces_deleted, 0);

    // Verify the power shelf is completely gone (not just soft-deleted).
    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.power_shelves.is_empty(),
        "Power shelf should be hard-deleted"
    );

    let mut conn = pool.acquire().await?;
    let history = db::state_history::for_object(
        &mut conn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &power_shelf_id,
    )
    .await?;
    assert!(
        history
            .iter()
            .any(|record| record.state == r#""retained-before-force-delete""#),
        "Power shelf state history should be retained",
    );

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_not_found(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let non_existent_id = PowerShelfId::from(uuid::Uuid::new_v4());
    let result = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(non_existent_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_force_delete_power_shelf_already_soft_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "SoftDeleted Power Shelf", Some(3000), Some(120)).await?;

    env.api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(power_shelf_id),
        }))
        .await?;

    let response = env
        .api()
        .admin_force_delete_power_shelf(tonic::Request::new(AdminForceDeletePowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
            delete_interfaces: false,
            delete_bmc_suppressions: false,
        }))
        .await?
        .into_inner();

    assert_eq!(response.power_shelf_id, power_shelf_id.to_string());

    let find_result = env
        .api()
        .find_power_shelves(tonic::Request::new(PowerShelfQuery {
            name: None,
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?
        .into_inner();

    assert!(
        find_result.power_shelves.is_empty(),
        "Power shelf should be hard-deleted after force delete"
    );

    Ok(())
}
