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

//! Direct-invocation tests for the PowerShelf `Maintenance` state handler.
//!
//! These tests construct a `PowerShelfStateHandler` and a real
//! `StateHandlerContext`, then drive
//! `handle_object_state` against a power shelf that has been parked in
//! `Maintenance { PowerOn | PowerOff }`. The tests assert on:
//!
//! - the resulting controller state (Ready / Error after the txn commits),
//! - whether `power_shelf_maintenance_requested` was cleared,
//! - and the requests actually sent to RMS via the queue/inspect helpers
//!   on `RmsSim`.
//!
//! Successful round-trips against real RMS require a fully populated
//! `machine_interfaces` row for the power shelf so the BMC IP lookup
//! returns. Setting that up from this layer is non-trivial; instead we
//! exercise the handler's many *precondition* failure paths, which still
//! cover the full PowerOn / PowerOff dispatch matrix and assert on
//! initiator / cleared-request behavior the user can observe.

use std::sync::Arc;

use carbide_power_shelf_controller::context::PowerShelfStateHandlerServices;
use carbide_secrets::credentials::Credentials;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::compute_tray_manager::Backend as ComputeBackend;
use component_manager::config::ComponentManagerConfig;
use component_manager::nv_switch_manager::Backend as NvSwitchBackend;
use component_manager::power_shelf_manager::Backend as PowerShelfBackend;
use db::{expected_power_shelf as db_expected_power_shelf, power_shelf as db_power_shelf};
use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::metadata::Metadata;
use model::power_shelf::{PowerShelfControllerState, PowerShelfMaintenanceOperation};
use model::test_support::{TEST_RMS_RACK_PROFILE_ID, power_shelf_config};
use rpc::common::SystemPowerControl;
use rpc::forge::component_power_control_request::Target;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ComponentPowerControlRequest, PowerShelfIdList};
use sqlx::PgConnection;
use state_controller::state_handler::StateHandlerOutcome;
use tonic::Request;

use crate::common::{
    ControllerEnv, extract_transition, load_power_shelf, run_handler,
    set_power_shelf_controller_state,
};

const TEST_BMC_USER: &str = "root";
const TEST_BMC_PASSWORD: &str = "password";

fn cm_power_action(operation: PowerShelfMaintenanceOperation) -> SystemPowerControl {
    match operation {
        PowerShelfMaintenanceOperation::PowerOn => SystemPowerControl::On,
        PowerShelfMaintenanceOperation::PowerOff => SystemPowerControl::ForceOff,
    }
}

async fn commit_and_extract_transition(
    mut outcome: StateHandlerOutcome<PowerShelfControllerState>,
) -> Option<PowerShelfControllerState> {
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await.unwrap();
    }
    extract_transition(outcome)
}

async fn request_power_shelf_maintenance_via_cm(
    env: &ControllerEnv,
    power_shelf_id: &PowerShelfId,
    operation: PowerShelfMaintenanceOperation,
) {
    env.harness
        .api()
        .component_power_control(Request::new(ComponentPowerControlRequest {
            target: Some(Target::PowerShelfIds(PowerShelfIdList {
                ids: vec![*power_shelf_id],
            })),
            action: cm_power_action(operation) as i32,
            bypass_state_controller: false,
        }))
        .await
        .expect("component_power_control should succeed");
}

/// Build a `PowerShelfStateHandlerServices` whose `component_manager` may be
/// cleared (to exercise the "component manager not configured" path) while
/// preserving the rest of the test environment's services.
fn services_with_component_manager(
    env: &ControllerEnv,
    component_manager: Option<Arc<component_manager::component_manager::ComponentManager>>,
) -> PowerShelfStateHandlerServices {
    PowerShelfStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager,
        // Force a credential manager that always resolves BMC creds via the
        // site-wide fallback. This avoids relying on whatever the test-env
        // happens to be seeded with for BMC creds.
        credential_manager: Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: TEST_BMC_USER.into(),
            password: TEST_BMC_PASSWORD.into(),
        })),
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

async fn build_test_component_manager(
    env: &ControllerEnv,
    rms_client: Option<Arc<dyn librms::RmsApi>>,
) -> Option<Arc<component_manager::component_manager::ComponentManager>> {
    let config = ComponentManagerConfig {
        nv_switch_backend: NvSwitchBackend::Mock,
        power_shelf_backend: if rms_client.is_some() {
            PowerShelfBackend::Rms
        } else {
            PowerShelfBackend::Mock
        },
        compute_tray_backend: ComputeBackend::Mock,
        power_shelf_use_state_controller: true,
        ..Default::default()
    };
    let component_manager = component_manager::component_manager::build_component_manager(
        &config,
        env.rack_profiles.clone(),
        rms_client,
        None,
        Some(env.pool.clone()),
        None,
    )
    .await
    .expect("test component manager should build");

    Some(Arc::new(component_manager))
}

/// Drive a power shelf into `Maintenance { operation }` with a maintenance
/// request set, exactly as if the gRPC handler had just persisted one.
async fn enter_maintenance(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    operation: PowerShelfMaintenanceOperation,
) {
    db_power_shelf::set_power_shelf_maintenance_requested(
        txn,
        *power_shelf_id,
        "test-initiator",
        operation,
    )
    .await
    .unwrap();
    set_power_shelf_controller_state(
        txn,
        power_shelf_id,
        PowerShelfControllerState::Maintenance { operation },
    )
    .await
    .unwrap();
}

/// Set the power shelf's BMC MAC and rack association directly. The
/// shared power shelf fixture leaves both as `None`, but the
/// handler's `batch_set_power_state` path needs both before it
/// will even attempt the BMC IP lookup.
///
/// `power_shelves.bmc_mac_address` is a FK into `expected_power_shelves`,
/// so when a `bmc_mac` is requested we first seed a matching expected row.
async fn set_power_shelf_rack_and_bmc(
    txn: &mut PgConnection,
    power_shelf_id: &PowerShelfId,
    rack_id: Option<&RackId>,
    bmc_mac: Option<MacAddress>,
) {
    if let Some(mac) = bmc_mac {
        db_expected_power_shelf::create(
            txn,
            ExpectedPowerShelf {
                expected_power_shelf_id: None,
                bmc_mac_address: mac,
                bmc_username: TEST_BMC_USER.into(),
                bmc_password: TEST_BMC_PASSWORD.into(),
                serial_number: format!("EPS-{mac}"),
                bmc_ip_address: None,
                metadata: Metadata::default(),
                rack_id: None,
                bmc_retain_credentials: None,
            },
        )
        .await
        .unwrap();
    }

    sqlx::query("UPDATE power_shelves SET rack_id = $1, bmc_mac_address = $2 WHERE id = $3")
        .bind(rack_id)
        .bind(bmc_mac)
        .bind(power_shelf_id)
        .execute(txn)
        .await
        .unwrap();
}

fn assert_error_with_substring(state: &PowerShelfControllerState, expected_substring: &str) {
    match state {
        PowerShelfControllerState::Error { cause } => {
            assert!(
                cause.contains(expected_substring),
                "Error cause '{}' did not contain expected substring '{}'",
                cause,
                expected_substring,
            );
        }
        other => panic!(
            "expected Error transition, got {}",
            serde_json::to_string(other).unwrap_or_else(|_| "<unserializable>".into()),
        ),
    }
}

// ── PowerOn ─────────────────────────────────────────────────────────────────

#[sqlx_test]
async fn ready_transitions_to_maintenance_when_request_is_set_via_component_power_control(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("CM power-off"))
        .await
        .id;

    request_power_shelf_maintenance_via_cm(
        &env,
        &power_shelf_id,
        PowerShelfMaintenanceOperation::PowerOff,
    )
    .await;

    {
        let mut txn = pool.acquire().await?;
        set_power_shelf_controller_state(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfControllerState::Ready,
        )
        .await?;
    }

    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let outcome = run_handler(&mut services, &mut shelf).await;

    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: PowerShelfControllerState::Maintenance {
                operation: PowerShelfMaintenanceOperation::PowerOff,
            },
            ..
        }
    ));

    Ok(())
}

/// PowerOn with a fully wired-up power shelf still fails because the
/// underlay machine_interface is not provisioned in this layer's fixtures.
/// The handler must surface a precise `no BMC IP found` error, must not
/// invoke RMS, and must clear the maintenance request.
#[sqlx_test]
async fn power_on_transitions_to_error_when_bmc_ip_unresolvable(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOn no-ip"))
        .await
        .id;

    let rack_id = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await
        .id;
    let bmc_mac: MacAddress = "AA:BB:CC:DD:EE:01".parse().unwrap();

    // Queue a success response so we can assert it was *not* consumed.
    env.rms_sim
        .queue_batch_set_power_state_response(Ok(rms::BatchSetPowerStateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                ..Default::default()
            }),
        }))
        .await;

    {
        let mut txn = pool.acquire().await?;
        set_power_shelf_rack_and_bmc(txn.as_mut(), &power_shelf_id, Some(&rack_id), Some(bmc_mac))
            .await;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await;
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "no BMC IP found for power shelf");

    let reloaded = load_power_shelf(&pool, &power_shelf_id).await;
    assert!(
        reloaded.power_shelf_maintenance_requested.is_none(),
        "maintenance request should be cleared on error transition"
    );

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(
        calls.is_empty(),
        "RMS should not be invoked when BMC IP cannot be resolved, got: {} calls",
        calls.len()
    );

    Ok(())
}

#[sqlx_test]
async fn power_on_transitions_to_error_when_component_manager_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOn no-rms"))
        .await
        .id;
    {
        let mut txn = pool.acquire().await?;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await;
    }

    let mut services = services_with_component_manager(&env, None);
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "component manager not configured");

    let reloaded = load_power_shelf(&pool, &power_shelf_id).await;
    assert!(reloaded.power_shelf_maintenance_requested.is_none());

    Ok(())
}

#[sqlx_test]
async fn power_on_transitions_to_error_when_rack_id_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOn no-rack"))
        .await
        .id;
    let bmc_mac: MacAddress = "AA:BB:CC:DD:EE:02".parse().unwrap();

    {
        let mut txn = pool.acquire().await?;
        // bmc_mac set, rack_id deliberately left as None.
        set_power_shelf_rack_and_bmc(txn.as_mut(), &power_shelf_id, None, Some(bmc_mac)).await;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await;
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "no rack association");

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(calls.is_empty(), "RMS must not be called without a rack_id");

    Ok(())
}

#[sqlx_test]
async fn power_on_transitions_to_error_when_bmc_mac_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOn no-mac"))
        .await
        .id;
    let rack_id = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        // rack_id set, bmc_mac deliberately left as None.
        set_power_shelf_rack_and_bmc(txn.as_mut(), &power_shelf_id, Some(&rack_id), None).await;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOn,
        )
        .await;
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "has no BMC MAC address recorded");

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(
        calls.is_empty(),
        "RMS must not be called without a BMC MAC address"
    );

    Ok(())
}

// ── PowerOff ───────────────────────────────────────────────────────────────

#[sqlx_test]
async fn power_off_transitions_to_error_when_component_manager_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOff no-rms"))
        .await
        .id;
    {
        let mut txn = pool.acquire().await?;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await;
    }

    let mut services = services_with_component_manager(&env, None);
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "component manager not configured");
    // The error cause must mention the operation we tried.
    if let PowerShelfControllerState::Error { cause } = &transition {
        assert!(
            cause.contains("PowerOff"),
            "PowerOff error cause should mention the operation: {cause}"
        );
    } else {
        unreachable!("assertion above guarantees Error variant");
    }

    let reloaded = load_power_shelf(&pool, &power_shelf_id).await;
    assert!(reloaded.power_shelf_maintenance_requested.is_none());

    Ok(())
}

#[sqlx_test]
async fn power_off_transitions_to_error_when_rack_id_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOff no-rack"))
        .await
        .id;
    let bmc_mac: MacAddress = "AA:BB:CC:DD:EE:03".parse().unwrap();

    {
        let mut txn = pool.acquire().await?;
        set_power_shelf_rack_and_bmc(txn.as_mut(), &power_shelf_id, None, Some(bmc_mac)).await;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await;
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "no rack association");
    if let PowerShelfControllerState::Error { cause } = &transition {
        assert!(cause.contains("PowerOff"));
    }

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(calls.is_empty());

    Ok(())
}

#[sqlx_test]
async fn power_off_transitions_to_error_when_bmc_mac_missing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("PowerOff no-mac"))
        .await
        .id;
    let rack_id = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await
        .id;

    {
        let mut txn = pool.acquire().await?;
        set_power_shelf_rack_and_bmc(txn.as_mut(), &power_shelf_id, Some(&rack_id), None).await;
        enter_maintenance(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfMaintenanceOperation::PowerOff,
        )
        .await;
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    let transition = commit_and_extract_transition(outcome).await.unwrap();
    assert_error_with_substring(&transition, "has no BMC MAC address recorded");

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(calls.is_empty());

    Ok(())
}

// ── Sanity: non-Maintenance state never reaches the by-device-list path ────

/// `Ready` should never invoke `batch_set_power_state`. This guards
/// against accidentally wiring the maintenance dispatch into other states.
#[sqlx_test]
async fn ready_state_does_not_invoke_rms_set_power_state(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let power_shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("Ready no-rms-call"))
        .await
        .id;
    {
        let mut txn = pool.acquire().await?;
        set_power_shelf_controller_state(
            txn.as_mut(),
            &power_shelf_id,
            PowerShelfControllerState::Ready,
        )
        .await
        .unwrap();
    }

    let mut services = services_with_component_manager(
        &env,
        build_test_component_manager(&env, env.rms_sim.as_rms_client()).await,
    );
    let mut shelf = load_power_shelf(&pool, &power_shelf_id).await;
    let outcome = run_handler(&mut services, &mut shelf).await;
    // We don't care about the outcome here, just that no by-device-list
    // call was made.
    let _ = commit_and_extract_transition(outcome).await;

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(
        calls.is_empty(),
        "Ready state must not call batch_set_power_state"
    );

    Ok(())
}
