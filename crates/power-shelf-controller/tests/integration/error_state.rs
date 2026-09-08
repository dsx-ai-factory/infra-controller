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

//! Tests for the PowerShelf `Error` state handler.

use std::sync::Arc;

use carbide_power_shelf_controller::context::PowerShelfStateHandlerServices;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_uuid::power_shelf::PowerShelfId;
use component_manager::compute_tray_manager::Backend as ComputeBackend;
use component_manager::config::ComponentManagerConfig;
use component_manager::nv_switch_manager::Backend as NvSwitchBackend;
use component_manager::power_shelf_manager::Backend as PowerShelfBackend;
use db::power_shelf as db_power_shelf;
use model::power_shelf::{PowerShelfControllerState, PowerShelfMaintenanceOperation};
use model::test_support::power_shelf_config;
use sqlx::PgConnection;
use state_controller::state_handler::StateHandlerOutcome;

use crate::common::{
    ControllerEnv, extract_transition, load_power_shelf, mark_power_shelf_as_deleted, run_handler,
    set_power_shelf_controller_state,
};

const TEST_ERROR_CAUSE: &str = "test error";

async fn services(env: &ControllerEnv) -> PowerShelfStateHandlerServices {
    let config = ComponentManagerConfig {
        nv_switch_backend: NvSwitchBackend::Mock,
        power_shelf_backend: PowerShelfBackend::Rms,
        compute_tray_backend: ComputeBackend::Mock,
        ..Default::default()
    };
    let component_manager = component_manager::component_manager::build_component_manager(
        &config,
        env.rack_profiles.clone(),
        env.rms_sim.as_rms_client(),
        None,
        Some(env.pool.clone()),
        None,
    )
    .await
    .expect("test component manager should build");
    let component_manager = Some(Arc::new(component_manager));

    PowerShelfStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager,
        credential_manager: Arc::new(TestCredentialManager::default()),
        per_object_metrics_registry: env.per_object_metrics_registry.clone(),
        rack_firmware_reprovisioning_enabled: false,
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    }
}

async fn park_in_error(txn: &mut PgConnection, power_shelf_id: &PowerShelfId) {
    set_power_shelf_controller_state(
        txn,
        power_shelf_id,
        PowerShelfControllerState::Error {
            cause: TEST_ERROR_CAUSE.into(),
        },
    )
    .await
    .unwrap();
}

#[sqlx_test]
async fn error_with_power_on_maintenance_request_transitions_to_maintenance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("Error->Maintenance PowerOn"))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        park_in_error(txn.as_mut(), &power_shelf_id).await;
        db_power_shelf::set_power_shelf_maintenance_requested(
            txn.as_mut(),
            power_shelf_id,
            "test-initiator",
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await?;
    }

    let mut services = services(&env).await;
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = extract_transition(outcome).expect("should transition out of Error");

    assert!(
        matches!(
            transition,
            PowerShelfControllerState::Maintenance {
                operation: PowerShelfMaintenanceOperation::PowerOn,
            }
        ),
        "expected transition to Maintenance {{ PowerOn }}, got {:?}",
        transition,
    );
    Ok(())
}

#[sqlx_test]
async fn error_with_power_off_maintenance_request_transitions_to_maintenance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("Error->Maintenance PowerOff"))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        park_in_error(txn.as_mut(), &power_shelf_id).await;
        db_power_shelf::set_power_shelf_maintenance_requested(
            txn.as_mut(),
            power_shelf_id,
            "test-initiator",
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await?;
    }

    let mut services = services(&env).await;
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = extract_transition(outcome).expect("should transition out of Error");

    assert!(
        matches!(
            transition,
            PowerShelfControllerState::Maintenance {
                operation: PowerShelfMaintenanceOperation::PowerOff,
            }
        ),
        "expected transition to Maintenance {{ PowerOff }}, got {:?}",
        transition,
    );
    Ok(())
}

#[sqlx_test]
async fn error_without_maintenance_request_holds_in_error(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("Error stays in Error"))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        park_in_error(txn.as_mut(), &power_shelf_id).await;
    }

    let mut services = services(&env).await;
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;

    assert!(
        !matches!(outcome, StateHandlerOutcome::Transition { .. }),
        "Error state without maintenance request must not transition",
    );
    Ok(())
}

#[sqlx_test]
async fn error_with_deletion_takes_precedence_over_maintenance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("Error deletion wins over maintenance"))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        park_in_error(txn.as_mut(), &power_shelf_id).await;
        db_power_shelf::set_power_shelf_maintenance_requested(
            txn.as_mut(),
            power_shelf_id,
            "test-initiator",
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await?;
        mark_power_shelf_as_deleted(txn.as_mut(), &power_shelf_id).await?;
    }

    let mut services = services(&env).await;
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = extract_transition(outcome).expect("should transition out of Error");

    assert!(
        matches!(transition, PowerShelfControllerState::Deleting),
        "deletion must win over maintenance, got {:?}",
        transition,
    );
    Ok(())
}
