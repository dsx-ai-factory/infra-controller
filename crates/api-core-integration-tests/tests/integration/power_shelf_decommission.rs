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
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use model::power_shelf::{PowerShelf, PowerShelfControllerState};
use model::rack::RackConfig;
use model::test_support::{TEST_RMS_RACK_PROFILE_ID, power_shelf_config};
use rpc::forge::DecommissionPowerShelfRequest;
use sqlx::PgConnection;
use tonic::{Code, Request};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn load_power_shelf(pool: &PgPool, id: PowerShelfId) -> TestResult<PowerShelf> {
    let mut conn = pool.acquire().await?;
    Ok(db::power_shelf::find_by_id(&mut conn, &id)
        .await?
        .expect("power shelf should exist"))
}

async fn set_power_shelf_controller_state(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    state: PowerShelfControllerState,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE power_shelves SET controller_state = $1 WHERE id = $2")
        .bind(serde_json::to_value(state).unwrap())
        .bind(power_shelf_id)
        .execute(txn)
        .await?;

    Ok(())
}

#[sqlx_test]
async fn decommission_requires_ready_power_shelf(pool: PgPool) -> TestResult {
    let env = TestHarness::builder(pool).build().await;

    let error = env
        .api()
        .decommission_power_shelf(Request::new(DecommissionPowerShelfRequest::default()))
        .await
        .expect_err("a missing power shelf ID must be rejected");
    assert_eq!(error.code(), Code::InvalidArgument);

    let TestPowerShelf { id: power_shelf_id } = env
        .create_power_shelf(power_shelf_config("Decommission precondition"))
        .await;

    let error = env
        .api()
        .decommission_power_shelf(Request::new(DecommissionPowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
        }))
        .await
        .expect_err("an initializing power shelf must be rejected");
    assert_eq!(error.code(), Code::FailedPrecondition);
    Ok(())
}

#[sqlx_test]
async fn decommission_rejects_instance_assigned_host_in_power_shelf_rack(
    pool: PgPool,
) -> TestResult {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestPowerShelf { id: power_shelf_id } = env
        .create_power_shelf(power_shelf_config("Assigned host preflight"))
        .await;
    let rack_id = RackId::new("power-shelf-decommission-rack");
    let rack_profile_id = RackProfileId::new(TEST_RMS_RACK_PROFILE_ID);
    let host_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x45; 32],
        MachineType::Host,
    );

    let mut txn = pool.begin().await?;
    db::rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&rack_profile_id),
        &RackConfig::default(),
        None,
    )
    .await?;
    sqlx::query("UPDATE power_shelves SET rack_id = $1 WHERE id = $2")
        .bind(&rack_id)
        .bind(power_shelf_id)
        .execute(txn.as_mut())
        .await?;
    set_power_shelf_controller_state(
        txn.as_mut(),
        &power_shelf_id,
        PowerShelfControllerState::Ready,
    )
    .await?;
    sqlx::query("INSERT INTO machines (id, dpf, rack_id) VALUES ($1, '{}'::jsonb, $2)")
        .bind(host_id)
        .bind(&rack_id)
        .execute(txn.as_mut())
        .await?;
    sqlx::query("INSERT INTO instances (machine_id) VALUES ($1)")
        .bind(host_id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;

    let error = env
        .api()
        .decommission_power_shelf(Request::new(DecommissionPowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
        }))
        .await
        .expect_err("a power shelf with an assigned host in its rack must be rejected");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains("are assigned to instances"));
    assert!(
        !load_power_shelf(&pool, power_shelf_id)
            .await?
            .decommission_requested
    );

    sqlx::query("DELETE FROM instances WHERE machine_id = $1")
        .bind(host_id)
        .execute(&pool)
        .await?;
    env.api()
        .decommission_power_shelf(Request::new(DecommissionPowerShelfRequest {
            power_shelf_id: Some(power_shelf_id),
        }))
        .await?;
    assert!(
        load_power_shelf(&pool, power_shelf_id)
            .await?
            .decommission_requested
    );
    Ok(())
}
