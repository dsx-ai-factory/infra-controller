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

//! Tests for power-shelf rack firmware reprovisioning.

use carbide_test_harness::prelude::*;
use carbide_uuid::power_shelf::PowerShelfId;
use db::power_shelf as db_power_shelf;
use model::power_shelf::{PowerShelfControllerState, ReProvisioningState};
use model::rack::MaintenanceActivity;
use model::test_support::power_shelf_config;
use sqlx::PgConnection;
use state_controller::state_handler::StateHandlerOutcome;

use crate::common::{
    load_power_shelf, run_handler, services_without_component_manager,
    set_power_shelf_controller_state,
};

fn firmware_only_activities() -> Vec<MaintenanceActivity> {
    vec![MaintenanceActivity::FirmwareUpgrade {
        firmware_version: None,
        components: vec![],
        force_update: false,
    }]
}

async fn commit_outcome(mut outcome: StateHandlerOutcome<PowerShelfControllerState>) {
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await.unwrap();
    }
}

async fn park_ready(txn: &mut PgConnection, power_shelf_id: &PowerShelfId) {
    set_power_shelf_controller_state(txn, power_shelf_id, PowerShelfControllerState::Ready)
        .await
        .expect("set Ready");
}

#[sqlx_test]
async fn test_ready_clears_reprovision_request_when_flag_disabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestPowerShelf { id: power_shelf_id } = env
        .create_power_shelf(power_shelf_config("Reprovision request disabled"))
        .await;

    let mut txn = pool.begin().await?;
    park_ready(txn.as_mut(), &power_shelf_id).await;
    db_power_shelf::set_power_shelf_reprovisioning_requested(
        txn.as_mut(),
        power_shelf_id,
        "rack-test",
        firmware_only_activities(),
    )
    .await?;
    txn.commit().await?;

    let mut state = load_power_shelf(&pool, &power_shelf_id).await;
    let mut services = services_without_component_manager(&pool, false);
    let outcome = run_handler(&mut services, &mut state).await;
    assert!(matches!(outcome, StateHandlerOutcome::DoNothing { .. }));
    commit_outcome(outcome).await;

    let state = load_power_shelf(&pool, &power_shelf_id).await;
    assert!(state.power_shelf_reprovisioning_requested.is_none());
    assert!(matches!(
        state.controller_state.value,
        PowerShelfControllerState::Ready
    ));
    Ok(())
}

#[sqlx_test]
async fn test_ready_enters_waiting_for_rack_firmware_when_flag_enabled(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestPowerShelf { id: power_shelf_id } = env
        .create_power_shelf(power_shelf_config("Reprovision request enabled"))
        .await;

    let mut txn = pool.begin().await?;
    park_ready(txn.as_mut(), &power_shelf_id).await;
    db_power_shelf::set_power_shelf_reprovisioning_requested(
        txn.as_mut(),
        power_shelf_id,
        "rack-test",
        firmware_only_activities(),
    )
    .await?;
    txn.commit().await?;

    let mut state = load_power_shelf(&pool, &power_shelf_id).await;
    let mut services = services_without_component_manager(&pool, true);
    let outcome = run_handler(&mut services, &mut state).await;
    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: PowerShelfControllerState::ReProvisioning {
                reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
            },
            ..
        }
    ));
    Ok(())
}

#[sqlx_test]
async fn test_waiting_for_rack_firmware_completes_to_ready(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool.clone()).build().await;
    let TestPowerShelf { id: power_shelf_id } = env
        .create_power_shelf(power_shelf_config("Reprovision request completed"))
        .await;

    let mut txn = pool.begin().await?;
    db_power_shelf::set_power_shelf_reprovisioning_requested(
        txn.as_mut(),
        power_shelf_id,
        "rack-test",
        firmware_only_activities(),
    )
    .await?;
    let power_shelf = db_power_shelf::find_by_id(txn.as_mut(), &power_shelf_id)
        .await?
        .expect("power shelf should exist");
    let requested_at = power_shelf
        .power_shelf_reprovisioning_requested
        .as_ref()
        .expect("request should exist")
        .requested_at;
    set_power_shelf_controller_state(
        txn.as_mut(),
        &power_shelf_id,
        PowerShelfControllerState::ReProvisioning {
            reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
        },
    )
    .await?;
    db_power_shelf::update_firmware_upgrade_status(
        txn.as_mut(),
        power_shelf_id,
        Some(&model::rack::RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status: model::rack::RackFirmwareUpgradeState::Completed,
            started_at: Some(requested_at),
            ended_at: Some(requested_at + chrono::Duration::seconds(1)),
        }),
    )
    .await?;
    txn.commit().await?;

    let mut state = load_power_shelf(&pool, &power_shelf_id).await;
    let mut services = services_without_component_manager(&pool, true);
    let outcome = run_handler(&mut services, &mut state).await;
    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: PowerShelfControllerState::Ready,
            ..
        }
    ));
    commit_outcome(outcome).await;

    let state = load_power_shelf(&pool, &power_shelf_id).await;
    assert!(state.power_shelf_reprovisioning_requested.is_none());
    Ok(())
}
