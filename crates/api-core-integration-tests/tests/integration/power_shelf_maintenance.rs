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
use model::power_shelf::PowerShelfMaintenanceOperation as ModelPowerShelfMaintenanceOperation;
use rpc::forge::{
    PowerShelfDeletionRequest, PowerShelfMaintenanceOperation as RpcPowerShelfMaintenanceOperation,
    PowerShelfMaintenanceRequest,
};
use tonic::Code;

use crate::power_shelf::create_custom_power_shelf;

#[sqlx_test]
async fn test_set_power_shelf_maintenance_power_on_persists_request(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Maintenance PowerOn Shelf", None, None).await?;

    env.api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOn as i32,
            reference: None,
        }))
        .await?;

    let mut conn = pool.acquire().await?;
    let shelf = db::power_shelf::find_by_id(conn.as_mut(), &power_shelf_id)
        .await?
        .expect("power shelf should still exist");
    let req = shelf
        .power_shelf_maintenance_requested
        .expect("maintenance request should be persisted");
    assert_eq!(req.operation, ModelPowerShelfMaintenanceOperation::PowerOn);
    assert_eq!(
        req.initiator, "admin-cli",
        "no AuthContext / no `reference` should default initiator to admin-cli"
    );

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_power_off_persists_request_with_reference(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Maintenance PowerOff Shelf", None, None).await?;

    env.api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOff as i32,
            reference: Some("https://issues.example.com/TICKET-42".to_string()),
        }))
        .await?;

    let mut conn = pool.acquire().await?;
    let shelf = db::power_shelf::find_by_id(conn.as_mut(), &power_shelf_id)
        .await?
        .expect("power shelf should still exist");
    let req = shelf
        .power_shelf_maintenance_requested
        .expect("maintenance request should be persisted");
    assert_eq!(req.operation, ModelPowerShelfMaintenanceOperation::PowerOff);
    assert_eq!(req.initiator, "https://issues.example.com/TICKET-42");

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_multi_shelf(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let id1 = create_custom_power_shelf(&env, "Multi Shelf 1", None, None).await?;
    let id2 = create_custom_power_shelf(&env, "Multi Shelf 2", None, None).await?;

    env.api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![id1, id2],
            operation: RpcPowerShelfMaintenanceOperation::PowerOn as i32,
            reference: Some("multi-shelf-ref".to_string()),
        }))
        .await?;

    let mut conn = pool.acquire().await?;
    for shelf_id in [id1, id2] {
        let shelf = db::power_shelf::find_by_id(conn.as_mut(), &shelf_id)
            .await?
            .expect("power shelf should still exist");
        let req = shelf
            .power_shelf_maintenance_requested
            .expect("maintenance request should be persisted on every shelf");
        assert_eq!(req.operation, ModelPowerShelfMaintenanceOperation::PowerOn);
        assert_eq!(req.initiator, "multi-shelf-ref");
    }

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_rejects_empty_id_list(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;

    let result = env
        .api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![],
            operation: RpcPowerShelfMaintenanceOperation::PowerOn as i32,
            reference: None,
        }))
        .await;

    let status = result.expect_err("empty id list must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_rejects_unspecified_operation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Unspecified Op Shelf", None, None).await?;

    let result = env
        .api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::Unspecified as i32,
            reference: None,
        }))
        .await;

    let status = result.expect_err("unspecified operation must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    let mut conn = pool.acquire().await?;
    let shelf = db::power_shelf::find_by_id(conn.as_mut(), &power_shelf_id)
        .await?
        .expect("power shelf should still exist");
    assert!(
        shelf.power_shelf_maintenance_requested.is_none(),
        "no maintenance request should be persisted on rejected calls"
    );

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_rejects_unknown_id(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let missing_id = PowerShelfId::from(uuid::Uuid::new_v4());

    let result = env
        .api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![missing_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOff as i32,
            reference: None,
        }))
        .await;

    let status = result.expect_err("unknown id must be rejected");
    assert_eq!(status.code(), Code::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_rejects_deleted_shelf(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Deleted Maintenance Shelf", None, None).await?;

    env.api()
        .delete_power_shelf(tonic::Request::new(PowerShelfDeletionRequest {
            id: Some(power_shelf_id),
        }))
        .await?;

    let result = env
        .api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOn as i32,
            reference: None,
        }))
        .await;

    let status = result.expect_err("deleted power shelf must be rejected");
    assert_eq!(status.code(), Code::InvalidArgument);

    Ok(())
}

#[sqlx_test]
async fn test_set_power_shelf_maintenance_overwrites_previous_request(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let power_shelf_id =
        create_custom_power_shelf(&env, "Overwrite Maintenance Shelf", None, None).await?;

    env.api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOn as i32,
            reference: Some("first".to_string()),
        }))
        .await?;

    env.api()
        .set_power_shelf_maintenance(tonic::Request::new(PowerShelfMaintenanceRequest {
            power_shelf_ids: vec![power_shelf_id],
            operation: RpcPowerShelfMaintenanceOperation::PowerOff as i32,
            reference: Some("second".to_string()),
        }))
        .await?;

    let mut conn = pool.acquire().await?;
    let shelf = db::power_shelf::find_by_id(conn.as_mut(), &power_shelf_id)
        .await?
        .expect("power shelf should still exist");
    let req = shelf
        .power_shelf_maintenance_requested
        .expect("expected the second maintenance request to be persisted");
    assert_eq!(req.operation, ModelPowerShelfMaintenanceOperation::PowerOff);
    assert_eq!(req.initiator, "second");

    Ok(())
}
