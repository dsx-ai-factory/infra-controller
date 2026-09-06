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

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::handler::RackStateHandler;
use carbide_rack_controller::io::RackStateControllerIO;
use carbide_uuid::rack::{RackId, RackProfileId};
use db::db_read::DbReader;
use db::{self, ObjectColumnFilter, machine as db_machine, rack as db_rack};
use model::expected_machine::ExpectedMachineData;
use model::machine::ManagedHostState;
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack::{
    ConfigureNmxClusterState, FirmwareUpgradeState, MaintenanceScope, Rack, RackConfig,
    RackMaintenanceState, RackState, RackValidationState,
};
use rpc::forge::StateHistoryRecord;
use rpc::forge::forge_server::Forge;
use state_controller::config::IterationConfig;
use state_controller::controller::StateController;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};
use tokio_util::sync::CancellationToken;

use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_test_env, create_test_env_with_overrides,
};

mod fixtures;
mod handler;

use fixtures::rack::set_rack_controller_state;

#[derive(Debug, Default, Clone)]
pub(in crate::tests) struct TestRackStateHandler {
    /// The total count for the handler
    pub(in crate::tests) count: Arc<AtomicUsize>,
    /// We count for every rack ID how often the handler was called
    pub(in crate::tests) counts_per_id: Arc<Mutex<HashMap<String, usize>>>,
}

#[async_trait::async_trait]
impl StateHandler for TestRackStateHandler {
    type State = Rack;
    type ControllerState = RackState;
    type ObjectId = RackId;
    type ContextObjects = RackStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        rack_id: &RackId,
        state: &mut Rack,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        assert_eq!(state.id, *rack_id);
        self.count.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = self.counts_per_id.lock().unwrap();
            *guard.entry(rack_id.to_string()).or_default() += 1;
        }

        // Mirror the real handler: if the rack is marked deleted in DB,
        // transition to Deleting regardless of current state.
        if state.deleted.is_some() && !matches!(controller_state, RackState::Deleting) {
            return Ok(StateHandlerOutcome::transition(RackState::Deleting));
        }

        let state = match controller_state {
            RackState::Created => RackState::Discovering,
            RackState::Discovering => RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::Start,
                },
            },
            RackState::Maintenance { maintenance_state } => match maintenance_state {
                RackMaintenanceState::FirmwareUpgrade { .. } => RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                        configure_nmx_cluster: ConfigureNmxClusterState::Start,
                    },
                },
                RackMaintenanceState::NVOSUpdate { .. } => RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                        configure_nmx_cluster: ConfigureNmxClusterState::Start,
                    },
                },
                RackMaintenanceState::ConfigureNmxCluster { .. } => RackState::Maintenance {
                    maintenance_state: RackMaintenanceState::Completed,
                },
                RackMaintenanceState::Completed => RackState::Validating {
                    validating_state: RackValidationState::Pending,
                },
                _ => return Ok(StateHandlerOutcome::do_nothing()),
            },
            RackState::Validating { validating_state } => match validating_state {
                RackValidationState::Pending => RackState::Validating {
                    validating_state: RackValidationState::InProgress {
                        run_id: "test-run".to_string(),
                    },
                },
                RackValidationState::InProgress { run_id } => RackState::Validating {
                    validating_state: RackValidationState::Partial {
                        run_id: run_id.clone(),
                    },
                },
                RackValidationState::Partial { run_id } => RackState::Validating {
                    validating_state: RackValidationState::Validated {
                        run_id: run_id.clone(),
                    },
                },
                RackValidationState::Validated { .. } => RackState::Ready,
                _ => return Ok(StateHandlerOutcome::do_nothing()),
            },
            RackState::Deleting => {
                // Rack is being deleted
                let mut txn = ctx.services.db_pool.begin().await?;
                db::rack::final_delete(&mut txn, rack_id).await?;
                return Ok(StateHandlerOutcome::deleted().with_txn(txn));
            }
            _ => return Ok(StateHandlerOutcome::do_nothing()),
        };

        Ok(StateHandlerOutcome::transition(state))
    }
}

fn validate_state_change_history(histories: &[StateHistoryRecord], expected: &[&str]) -> bool {
    let parsed_histories = histories
        .iter()
        .filter_map(|history| serde_json::from_str::<serde_json::Value>(&history.state).ok())
        .collect::<Vec<_>>();

    for &state in expected {
        let Ok(expected_state) = serde_json::from_str::<serde_json::Value>(state) else {
            return false;
        };
        if !parsed_histories
            .iter()
            .any(|history| history == &expected_state)
        {
            return false;
        }
    }
    true
}

#[crate::sqlx_test]
async fn test_can_retrieve_rack_state_history_with_real_handler(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides {
            config: Some(handler::config_with_rack_profiles()),
            ..Default::default()
        },
    )
    .await;

    // Create rack and both compute machines before starting the controller.
    // Using "Simple" capabilities (compute=2, switch=0, power_shelf=0) so
    // Created -> Discovering fires once both machines are linked, without
    // requiring switches or power shelves.
    let machine_id_1 = handler::new_machine_id(1);
    let machine_id_2 = handler::new_machine_id(2);
    let mut txn = pool.acquire().await?;
    let rack_id = TestRackDbBuilder::new()
        .with_rack_profile_id("Simple")
        .persist(&mut txn)
        .await?;

    // Insert machine records directly, bypassing the full machine state machine
    // since this test exercises the rack SM only.
    let rack_data = ExpectedMachineData {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    };
    db_machine::create(
        &mut txn,
        None,
        &machine_id_1,
        ManagedHostState::Ready,
        Some(&rack_data),
        2,
    )
    .await?;
    db_machine::create(
        &mut txn,
        None,
        &machine_id_2,
        ManagedHostState::Ready,
        Some(&rack_data),
        2,
    )
    .await?;

    // Run the real handler through the controller.
    let rack_handler = Arc::new(RackStateHandler::default());
    let handler_services = Arc::new(env.rack_state_handler_services());
    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<RackStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), env.api.work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(handler_services)
        .state_handler(rack_handler)
        .build_for_manual_iterations(cancel_token.clone())
        .unwrap();

    //--------------------------------------------------------------------------

    // Iteration 1: Created -> Discovering.
    // Both compute machines are present; Simple caps require no switches or
    // power shelves, so Created transitions immediately.
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(rack.controller_state.value, RackState::Discovering),
        "Expected rack to be in Discovering, got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Iteration 2: Discovering -> Maintenance(FirmwareUpgrade(Start)).
    // Both machines are already Ready (created above).
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(
            rack.controller_state.value,
            RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade { .. }
            }
        ),
        "Expected rack to be in Maintenance(FirmwareUpgrade), got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Iterations 3-7: FirmwareUpgrade(Start) -> Validating(Pending).
    //
    // Simple rack has no switches, so the real handler takes a shortened path:
    // FirmwareUpgrade(Start) skips (no firmware-object JSON configured)
    // -> NVOSUpdate(Start) skips (no switches)
    // -> ConfigureNmxCluster(Start) skips (no switches)
    // -> PowerSequence(PoweringOn) -> Completed -> Validating(Pending).
    controller.run_single_iteration().await; // FirmwareUpgrade(Start) -> NVOSUpdate(Start)
    controller.run_single_iteration().await; // NVOSUpdate(Start) -> ConfigureNmxCluster(Start)
    controller.run_single_iteration().await; // ConfigureNmxCluster(Start) -> PowerSequence(PoweringOn)
    controller.run_single_iteration().await; // PowerSequence(PoweringOn) -> Completed
    controller.run_single_iteration().await; // Completed -> Validating(Pending)

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(
            rack.controller_state.value,
            RackState::Validating {
                validating_state: RackValidationState::Pending,
            }
        ),
        "Expected rack to be in Validating(Pending), got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // --- Setup for iterations 8-11: Validation states ---
    //
    // Set rv.* labels on both compute trays so the real handler can drive the
    // validation sub-state machine. Both machines are assigned to the same
    // partition ("p0") and marked "pass", which is the minimal happy path:
    // one partition, all nodes validated.
    {
        let mut txn = pool.begin().await?;
        let machines = db_machine::find(
            &mut *txn,
            db::ObjectFilter::List(&[machine_id_1, machine_id_2]),
            MachineSearchConfig::default(),
        )
        .await?;
        for machine in machines {
            let mut metadata = machine.metadata.clone();
            metadata
                .labels
                .insert("rv.run-id".to_string(), "test-run".to_string());
            metadata
                .labels
                .insert("rv.part-id".to_string(), "p0".to_string());
            metadata
                .labels
                .insert("rv.st".to_string(), "pass".to_string());
            db_machine::update_metadata(&mut txn, &machine.id, machine.version, metadata).await?;
        }
        txn.commit().await?;
    }

    // Iteration 8: Validating(Pending) -> Validating(InProgress).
    // The handler finds rv.run-id on a machine and promotes to InProgress.
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(
            rack.controller_state.value,
            RackState::Validating {
                validating_state: RackValidationState::InProgress { .. }
            }
        ),
        "Expected Validating(InProgress), got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Iteration 9: Validating(InProgress) -> Validating(Partial).
    // Partition p0 has validated > 0 (both nodes pass), so InProgress -> Partial.
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(
            rack.controller_state.value,
            RackState::Validating {
                validating_state: RackValidationState::Partial { .. }
            }
        ),
        "Expected Validating(Partial), got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Iteration 10: Validating(Partial) -> Validating(Validated).
    // validated(1) == total_partitions(1) -> Validated.
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(
            rack.controller_state.value,
            RackState::Validating {
                validating_state: RackValidationState::Validated { .. }
            }
        ),
        "Expected Validating(Validated), got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Iteration 11: Validating(Validated) -> Ready.
    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(rack.controller_state.value, RackState::Ready),
        "Expected Ready, got: {:?}",
        rack.controller_state.value
    );

    //--------------------------------------------------------------------------

    // Verify the history RPC contains records for all transitions.
    let result = env
        .api
        .find_rack_state_histories(tonic::Request::new(rpc::forge::RackStateHistoriesRequest {
            rack_ids: vec![rack_id.clone()],
        }))
        .await?;
    let mut histories = result.into_inner().histories;
    let records = histories
        .remove(&rack_id.to_string())
        .unwrap_or_default()
        .records;

    // States are serialized via serde with #[serde(tag = "state", rename_all = "snake_case")].
    let expected = vec![
        "{\"state\": \"discovering\"}",
        "{\"state\": \"maintenance\", \"maintenance_state\": {\"FirmwareUpgrade\": {\"rack_firmware_upgrade\": \"Start\"}}}",
        "{\"state\": \"maintenance\", \"maintenance_state\": {\"NVOSUpdate\": {\"nvos_update\": \"Start\"}}}",
        "{\"state\": \"maintenance\", \"maintenance_state\": {\"ConfigureNmxCluster\": {\"configure_nmx_cluster\": \"Start\"}}}",
        "{\"state\": \"maintenance\", \"maintenance_state\": {\"PowerSequence\": {\"rack_power\": \"PoweringOn\"}}}",
        "{\"state\": \"maintenance\", \"maintenance_state\": \"Completed\"}",
        "{\"state\": \"validating\", \"validating_state\": \"Pending\"}",
        "{\"state\": \"validating\", \"validating_state\": {\"Validated\": {\"run_id\": \"test-run\"}}}",
        "{\"state\": \"ready\"}",
    ];
    assert!(validate_state_change_history(&records, &expected));

    Ok(())
}

/// Verifies (via the full controller) that a rack in Error state stays in
/// Error state.
///
/// Compare with `handler::test_error_state_does_nothing`, which tests the
/// same behaviour by calling the handler directly — without the controller
/// ceremony.
#[crate::sqlx_test]
async fn test_error_state_does_nothing_with_controller(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;

    set_rack_controller_state(
        pool.acquire().await?.as_mut(),
        &rack_id,
        RackState::Error {
            cause: "test error".to_string(),
        },
    )
    .await?;

    let rack_handler = Arc::new(RackStateHandler::default());
    let handler_services = Arc::new(env.rack_state_handler_services());
    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<RackStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), env.api.work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(handler_services)
        .state_handler(rack_handler)
        .build_for_manual_iterations(cancel_token.clone())
        .unwrap();

    controller.run_single_iteration().await;

    let rack = get_db_rack(env.db_reader().as_mut(), &rack_id).await;
    assert!(
        matches!(rack.controller_state.value, RackState::Error { .. }),
        "Error state should not transition"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_deletion_with_state_controller(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    // Create a rack
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;

    // Start the state controller
    let rack_handler = Arc::new(TestRackStateHandler::default());
    const ITERATION_TIME: Duration = Duration::from_millis(50);

    let handler_services = Arc::new(env.rack_state_handler_services());

    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<RackStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: ITERATION_TIME,
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), env.api.work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(handler_services.clone())
        .state_handler(rack_handler.clone())
        .build_for_manual_iterations(cancel_token.clone())
        .unwrap();

    controller.run_single_iteration().await;

    // Mark the rack as deleted
    db::rack::mark_as_deleted(&rack_id, pool.acquire().await?.as_mut()).await?;

    // Let the controller continue to run
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;

    // Verify that the DB object is gone
    let racks = env
        .api
        .find_racks_by_ids(tonic::Request::new(rpc::forge::RacksByIdsRequest {
            rack_ids: vec![rack_id],
        }))
        .await?
        .into_inner()
        .racks;
    assert!(racks.is_empty());

    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_controller_state_version_increment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create a rack
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.begin().await?;
    db_rack::create(
        &mut txn,
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    // Verify initial state
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    assert!(matches!(rack.controller_state.value, RackState::Created));
    let initial_version = rack.controller_state.version;

    // Update controller state with correct version
    let new_version = initial_version.increment();
    let updated = db_rack::try_update_controller_state(
        &mut txn,
        &rack_id,
        initial_version,
        new_version,
        &RackState::Discovering,
    )
    .await?;
    assert!(updated, "update with correct version should succeed");

    // Verify version was incremented
    let rack = get_db_rack(txn.as_mut(), &rack_id).await;
    assert_eq!(
        rack.controller_state.version.version_nr(),
        initial_version.version_nr() + 1,
        "version should be incremented after update"
    );

    // Trying to update with the old version should fail (optimistic lock)
    let stale_update = db_rack::try_update_controller_state(
        &mut txn,
        &rack_id,
        initial_version,
        initial_version.increment(),
        &RackState::Ready,
    )
    .await?;
    assert!(
        !stale_update,
        "update with stale version should be rejected"
    );

    // Updating with the current version should succeed
    let current_version = rack.controller_state.version;
    let updated_again = db_rack::try_update_controller_state(
        &mut txn,
        &rack_id,
        current_version,
        current_version.increment(),
        &RackState::Ready,
    )
    .await?;
    assert!(updated_again, "update with current version should succeed");

    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_maintenance_termination_latch_blocks_maintenance_transition_and_stale_config_write(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.begin().await?;
    let rack = db_rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let maintenance_version = rack.controller_state.version.increment();
    assert!(
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            rack.controller_state.version,
            maintenance_version,
            &RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::Start,
                },
            },
        )
        .await?
    );

    let termination_config = RackConfig {
        maintenance_requested: Some(MaintenanceScope::default()),
        maintenance_termination_requested: true,
        ..Default::default()
    };
    db_rack::update(txn.as_mut(), &rack_id, &termination_config).await?;

    assert!(
        !db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            maintenance_version,
            maintenance_version.increment(),
            &RackState::Ready,
        )
        .await?,
        "an accepted termination must block an in-flight state transition"
    );

    let stale_update = db_rack::update(txn.as_mut(), &rack_id, &RackConfig::default()).await?;
    assert!(
        stale_update.config.maintenance_termination_requested,
        "a stale RackConfig snapshot must not clear the termination latch"
    );
    assert!(
        stale_update.config.maintenance_requested.is_some(),
        "a stale RackConfig snapshot must not clear the termination scope"
    );

    let consumed = db_rack::consume_maintenance_termination_request(txn.as_mut(), &rack_id).await?;
    assert!(!consumed.config.maintenance_termination_requested);
    assert!(consumed.config.maintenance_requested.is_none());
    assert!(
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            maintenance_version,
            maintenance_version.increment(),
            &RackState::Ready,
        )
        .await?,
        "the rack controller can transition after consuming the termination latch"
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_stale_rack_maintenance_termination_latch_does_not_block_non_maintenance_transition(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.begin().await?;
    let rack = db_rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("Empty")),
        &RackConfig {
            maintenance_termination_requested: true,
            ..Default::default()
        },
        None,
    )
    .await?;

    assert!(
        db_rack::try_update_controller_state(
            txn.as_mut(),
            &rack_id,
            rack.controller_state.version,
            rack.controller_state.version.increment(),
            &RackState::Ready,
        )
        .await?,
        "a stale termination latch outside Maintenance must not freeze the rack state machine"
    );

    let updated = db_rack::update(
        txn.as_mut(),
        &rack_id,
        &RackConfig {
            maintenance_requested: Some(MaintenanceScope::default()),
            maintenance_termination_requested: true,
            ..Default::default()
        },
    )
    .await?;
    assert!(
        !updated.config.maintenance_termination_requested,
        "a config update outside Maintenance must clear a stale termination latch"
    );
    assert!(updated.config.maintenance_requested.is_some());

    txn.rollback().await?;
    Ok(())
}

async fn get_db_rack<DB>(conn: &mut DB, rack_id: &RackId) -> Rack
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    db_rack::find_by(conn, ObjectColumnFilter::One(db_rack::IdColumn, rack_id))
        .await
        .unwrap()
        .pop()
        .unwrap()
}
