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

use std::sync::Arc;

use carbide_redfish::libredfish::RedfishClientPool as _;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_utils::redfish::BmcAccessInfo;
use model::machine::{
    DpuInitState, DpuInitStates, MachineMaintenanceOperation, ManagedHostState,
    PerformPowerOperation,
};
use model::power_manager::PowerState;
use model::test_support::ManagedHostConfig;
use rpc::forge::{
    MaintenanceOperation, MaintenanceRequest, PowerOptionRequest, PowerOptionUpdateRequest,
};
use tonic::Request;

use crate::env::Env;

struct TestContext {
    env: Env,
    mh: TestManagedHost,
}

impl TestContext {
    async fn init(pool: PgPool) -> Self {
        Self::from_env(power_manager_env_builder(pool).build().await).await
    }

    /// Like [`init`], but wires a component manager backed by an always-succeeding
    /// compute-tray backend (plus valid BMC credentials) so maintenance power
    /// operations can run to completion and return the host to Ready.
    async fn init_with_success_backend(pool: PgPool) -> Self {
        let backend = Arc::new(crate::maintenance::ReconciliationComputeTrayManager::new());
        let env = power_manager_env_builder(pool)
            .with_component_manager(crate::maintenance::component_manager(backend))
            .with_credential_manager(crate::maintenance::valid_credential_manager())
            .build()
            .await;
        Self::from_env(env).await
    }

    async fn from_env(env: Env) -> Self {
        Self::from_env_with_config(env, ManagedHostConfig::default()).await
    }

    /// Builds the fixture with an explicit hardware layout.
    ///
    /// Power-cycle coverage uses this to exercise one host with multiple DPUs
    /// without changing the default fixture used by the rest of this module.
    async fn from_env_with_config(env: Env, config: ManagedHostConfig) -> Self {
        let domain = env.test_harness.test_domain().await;
        let network_controller = env.test_harness.network_controller();
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        network_controller.create_admin_segment(&domain).await;
        let site_explorer = env.test_harness.default_test_site_explorer();
        let mh = env
            .test_harness
            .managed_host_builder(&site_explorer, underlay_segment)
            .with_config(config)
            .build()
            .await
            .0;
        mh.advance_to_converged_ready().await;
        Self { env, mh }
    }
}

fn power_manager_env_builder(pool: PgPool) -> crate::env::EnvBuilder {
    Env::builder(pool).configure_runtime(|config| {
        let zero = chrono::Duration::zero();
        config.power_manager_options.enabled = true;
        config.power_manager_options.next_try_duration_on_success = zero;
        config.power_manager_options.next_try_duration_on_failure = zero;
        config.power_manager_options.wait_duration_until_host_reboot = zero;
    })
}

trait TestManagedHostPowerExt {
    async fn bmc_access_info(&self) -> BmcAccessInfo;

    async fn set_next_power_poll_now(&self);
}

impl TestManagedHostPowerExt for TestManagedHost {
    async fn bmc_access_info(&self) -> BmcAccessInfo {
        let machine = self.host.machine().await;
        let bmc_address = machine.bmc_addr().expect("host should have a BMC address");
        let mut txn = self
            .api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start");
        let bmc_access_info = db::machine_interface::lookup_bmc_access_info(
            txn.as_mut(),
            bmc_address.ip(),
            Some(bmc_address.port()),
        )
        .await
        .expect("host BMC access information should exist");
        txn.commit()
            .await
            .expect("database transaction should commit");
        bmc_access_info
    }

    async fn set_next_power_poll_now(&self) {
        let mut txn = self
            .api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start");
        sqlx::query("UPDATE power_options SET last_fetched_next_try_at = now() WHERE host_id = $1")
            .bind(self.host.id)
            .execute(txn.as_mut())
            .await
            .expect("next power poll should be updated");
        txn.commit()
            .await
            .expect("database transaction should commit");
    }
}

#[sqlx_test]
async fn dpu_init_power_cycle_waits_for_observed_host_power_off(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { mut env, mh } = TestContext::from_env_with_config(
        Env::builder(pool).build().await,
        ManagedHostConfig::default().with_dpu_count(2),
    )
    .await;
    let dpu_init_state = |dpu_state: DpuInitState| ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: mh
                .dpus
                .iter()
                .map(|dpu| (dpu.id, dpu_state.clone()))
                .collect(),
        },
    };
    let powering_off = dpu_init_state(DpuInitState::WaitingForPlatformPowercycle {
        substate: PerformPowerOperation::Off,
    });
    let waiting_for_power_off = dpu_init_state(DpuInitState::WaitingForPlatformPowerOff);
    let powering_on = dpu_init_state(DpuInitState::WaitingForPlatformPowercycle {
        substate: PerformPowerOperation::On,
    });
    let configuring = dpu_init_state(DpuInitState::WaitingForPlatformConfiguration);
    mh.advance_state(powering_off).await;

    let bmc_access_info = mh.bmc_access_info().await;
    let redfish_client = env.redfish_sim.client_by_info(&bmc_access_info).await?;
    assert_eq!(
        redfish_client.get_power_state().await?,
        libredfish::PowerState::On,
    );

    // Keep the power-off observation in its own persisted phase. A delayed BMC
    // can then report stale `On` without letting the controller skip ahead.
    let redfish_timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_eq!(
        mh.host.machine().await.current_state(),
        &waiting_for_power_off,
    );
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access_info.host),
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceOff,
        )],
    );

    redfish_client
        .power(libredfish::SystemPowerControl::On)
        .await?;
    let redfish_timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_eq!(
        mh.host.machine().await.current_state(),
        &waiting_for_power_off,
    );
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access_info.host)
            .is_empty(),
    );

    // Once `Off` is observed, persist the power-on phase before issuing `On`.
    redfish_client
        .power(libredfish::SystemPowerControl::ForceOff)
        .await?;
    let redfish_timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_eq!(mh.host.machine().await.current_state(), &powering_on);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access_info.host)
            .is_empty(),
    );

    let redfish_timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_eq!(mh.host.machine().await.current_state(), &configuring);
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access_info.host),
        vec![RedfishSimAction::Power(libredfish::SystemPowerControl::On)],
    );
    assert_eq!(
        redfish_client.get_power_state().await?,
        libredfish::PowerState::On,
    );

    // If the process exits after `On` succeeds but before the transition is
    // committed, the persisted power-on phase must still resume cleanly.
    mh.advance_state(powering_on).await;
    let redfish_timepoint = env.redfish_sim.timepoint();
    env.run_single_iteration().await;

    assert_eq!(mh.host.machine().await.current_state(), &configuring);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access_info.host)
            .is_empty(),
    );

    Ok(())
}

#[sqlx_test]
async fn desired_on_polls_powered_off_machine(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { mut env, mh } = TestContext::init(pool).await;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::On);
    txn.rollback().await?;
    let bmc_access_info = mh.bmc_access_info().await;

    let sim = env.redfish_sim.client_by_info(&bmc_access_info).await?;
    sim.power(libredfish::SystemPowerControl::ForceOff).await?;
    assert_eq!(sim.get_power_state().await?, libredfish::PowerState::Off);
    mh.set_next_power_poll_now().await;

    env.run_single_iteration().await;
    // Since delay is set to 0 for test, db must be updated immediately.
    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
    txn.rollback().await?;

    // Wait for one cycle.
    env.run_single_iteration().await;
    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
    txn.rollback().await?;

    // State machine should power on the host.
    env.run_single_iteration().await;
    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn desired_on_limits_power_on_attempts(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { mut env, mh } = TestContext::init(pool).await;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::On);
    txn.rollback().await?;
    let bmc_access_info = mh.bmc_access_info().await;

    let sim = env.redfish_sim.client_by_info(&bmc_access_info).await?;
    sim.power(libredfish::SystemPowerControl::ForceOff).await?;
    assert_eq!(sim.get_power_state().await?, libredfish::PowerState::Off);
    mh.set_next_power_poll_now().await;

    // Run a iteration.
    // Since delay is set to 0 for test, db must be updated immediately.
    env.run_single_iteration().await;
    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
    txn.rollback().await?;

    for _ in 1..10 {
        // Keep power off
        sim.power(libredfish::SystemPowerControl::ForceOff).await?;
        env.run_single_iteration().await;

        let mut txn = env.test_harness.db_txn().await;
        let power_options = db::power_options::get_all(&mut txn).await?;
        assert_eq!(power_options[0].desired_power_state, PowerState::On);
        assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
        txn.rollback().await?;
    }

    let response = env
        .test_harness
        .api()
        .get_power_options(Request::new(PowerOptionRequest {
            machine_id: vec![mh.host.id],
        }))
        .await?
        .into_inner();
    assert_eq!(response.response[0].tried_triggering_on_counter, 3);

    Ok(())
}

#[sqlx_test]
async fn desired_off_persists_observed_off_state(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { mut env, mh } = TestContext::init(pool).await;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::On);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::On);
    txn.rollback().await?;
    let bmc_access_info = mh.bmc_access_info().await;

    // Set maintenance mode (required before setting desired=Off).
    env.test_harness
        .api()
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Enable as i32,
            host_id: Some(mh.host.id.into()),
            reference: Some("testing".to_string()),
        }))
        .await?;

    // Set desired power state to Off.
    env.test_harness
        .api()
        .update_power_option(Request::new(PowerOptionUpdateRequest {
            machine_id: Some(mh.host.id),
            power_state: rpc::forge::PowerState::Off as i32,
        }))
        .await?;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::Off);
    txn.rollback().await?;

    // Simulate the host being powered off via BMC.
    let sim = env.redfish_sim.client_by_info(&bmc_access_info).await?;
    sim.power(libredfish::SystemPowerControl::ForceOff).await?;
    assert_eq!(sim.get_power_state().await?, libredfish::PowerState::Off);

    // Record timestamps before the state controller iteration.
    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    let updated_at_before_poll = power_options[0].last_fetched_updated_at;
    txn.rollback().await?;

    // Advance next_try_at so the state controller will poll BMC.
    mh.set_next_power_poll_now().await;

    // Run state controller iteration — should poll BMC, see Off, and persist the update.
    env.run_single_iteration().await;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::Off);
    assert_eq!(power_options[0].last_fetched_power_state, PowerState::Off);
    assert!(power_options[0].last_fetched_updated_at > updated_at_before_poll);
    txn.rollback().await?;

    Ok(())
}

/// A queued machine-maintenance PowerOff (what the compute-tray state-controller
/// path posts when `compute_tray_use_state_controller = true`) must still reach
/// the Maintenance transition when the power manager is enabled and the host's
/// desired state is Off. Without the maintenance-takes-precedence guard,
/// `handle_power` returns `continue_state_machine = false` for desired Off and
/// `maintenance_transition_if_requested` is never reached, leaving the host
/// parked in Ready with the request unconsumed.
#[sqlx_test]
async fn queued_power_off_runs_when_power_manager_gates_desired_off(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let TestContext { mut env, mh } = TestContext::init_with_success_backend(pool).await;

    // Enable maintenance (the health override) so desired = Off is accepted, then
    // set desired = Off. This is precisely the gating condition for handle_power.
    env.test_harness
        .api()
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Enable as i32,
            host_id: Some(mh.host.id.into()),
            reference: Some("testing".to_string()),
        }))
        .await?;
    env.test_harness
        .api()
        .update_power_option(Request::new(PowerOptionUpdateRequest {
            machine_id: Some(mh.host.id),
            power_state: rpc::forge::PowerState::Off as i32,
        }))
        .await?;
    // Mirror the component manager, which removes the override immediately after
    // the power-option write: at reconcile time only desired = Off persists, so
    // the gate is driven purely by the power-manager desired state.
    env.test_harness
        .api()
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Disable as i32,
            host_id: Some(mh.host.id.into()),
            reference: Some("testing".to_string()),
        }))
        .await?;

    let mut txn = env.test_harness.db_txn().await;
    let power_options = db::power_options::get_all(&mut txn).await?;
    assert_eq!(power_options[0].desired_power_state, PowerState::Off);
    txn.rollback().await?;

    // Queue the PowerOff maintenance request, as the state-controller power path does.
    let mut txn = env.test_harness.db_txn().await;
    db::machine::set_machine_maintenance_requested(
        &mut txn,
        mh.host.id,
        "component-manager",
        MachineMaintenanceOperation::PowerOff,
    )
    .await?;
    txn.commit().await?;

    // First iteration must promote the request to Maintenance rather than parking
    // in Ready behind the desired-Off power-manager gate.
    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    assert!(
        matches!(
            machine.state.value,
            ManagedHostState::Maintenance {
                operation: MachineMaintenanceOperation::PowerOff
            }
        ),
        "expected Maintenance(PowerOff) after one iteration, got {:?}",
        machine.state.value,
    );

    // Second iteration reconciles the PowerOff against the (succeeding) backend,
    // clears the request, and returns the host to Ready -- confirming the queued
    // operation runs to completion, not just that it entered Maintenance.
    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    assert!(
        matches!(machine.state.value, ManagedHostState::Ready),
        "expected Ready after the PowerOff completes, got {:?}",
        machine.state.value,
    );
    assert!(
        machine.machine_maintenance_requested.is_none(),
        "maintenance request should be cleared once the operation completes",
    );

    Ok(())
}
