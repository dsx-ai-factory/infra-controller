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

//! Switch Ready-state best-effort power-state polling.

use std::sync::Arc;

use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials};
use carbide_switch_controller::context::{
    SwitchStateHandlerContextObjects, SwitchStateHandlerServices,
};
use carbide_switch_controller::handler::SwitchStateHandler;
use carbide_switch_controller::metrics::SwitchMetrics;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_uuid::switch::SwitchId;
use component_manager::mock::MockNvSwitchManager;
use component_manager::nv_switch_manager::NvSwitchManager;
use db::switch as db_switch;
use model::switch::{Switch, SwitchControllerState, SwitchStatus};
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerOutcome};

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, set_switch_controller_state,
    set_switch_rack_id,
};
use crate::state_controller::mock_component_manager;

/// Builds services whose component manager wraps `nv_switch`.
fn services_with_switch_manager(
    env: &ControllerEnv,
    nv_switch: Arc<dyn NvSwitchManager>,
) -> SwitchStateHandlerServices {
    SwitchStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager: Some(mock_component_manager(nv_switch)),
        credential_manager: env.test_credential_manager.clone(),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: env.per_object_metrics_registry.clone(),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    }
}

async fn load_switch(env: &ControllerEnv, id: &SwitchId) -> Switch {
    let mut conn = env.pool.acquire().await.unwrap();
    db_switch::find_by_id(conn.as_mut(), id)
        .await
        .unwrap()
        .expect("switch should exist")
}

async fn run_handler(
    services: &mut SwitchStateHandlerServices,
    state: &mut Switch,
) -> StateHandlerOutcome<SwitchControllerState> {
    let handler = SwitchStateHandler::default();
    let mut metrics = SwitchMetrics::default();
    let mut writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<SwitchStateHandlerContextObjects> {
        services,
        metrics: &mut metrics,
        pending_db_writes: &mut writes,
    };
    let controller_state = state.controller_state.value.clone();
    let switch_id = state.id;
    handler
        .handle_object_state(&switch_id, state, &controller_state, &mut ctx)
        .await
        .expect("state handler should not return an error result")
}

/// Commits any transaction the outcome carries, asserting the handler kept the
/// switch in `Ready` (`DoNothing`) rather than transitioning.
async fn commit_ready_outcome(mut outcome: StateHandlerOutcome<SwitchControllerState>) {
    assert!(
        matches!(outcome, StateHandlerOutcome::DoNothing { .. }),
        "Ready power poll must not transition the switch"
    );
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await.unwrap();
    }
}

/// Grants the switch its NVOS admin credential so `resolve_switch_endpoint`
/// succeeds, and associates it with a rack and the `Ready` controller state.
async fn make_ready_with_endpoint(env: &ControllerEnv, switch_id: &SwitchId) {
    let bmc_mac_address = load_switch(env, switch_id)
        .await
        .bmc_mac_address
        .expect("fixture switch has a BMC MAC");
    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &Credentials::UsernamePassword {
                username: "admin".into(),
                password: "password".into(),
            },
        )
        .await
        .unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    set_switch_rack_id(&mut txn, switch_id, &"rack-id-1".into())
        .await
        .unwrap();
    set_switch_controller_state(&mut txn, switch_id, SwitchControllerState::Ready)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

/// A Ready switch persists an observed power state into `switches.status`, and a
/// backend that reports no power state leaves `status` untouched.
#[sqlx_test]
async fn ready_persists_observed_power_state(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The observed power state is persisted verbatim, so each case is driven by
    // what the backend reports: `Some` is written to `status.power_state`, while
    // a backend with no power state leaves `status` unset.
    for backend_power_state in [Some("on"), None] {
        let env = ControllerEnv::new(pool.clone()).await;
        let switch_id = new_switch(&env, Some("Switch1".into()), None).await?;
        make_ready_with_endpoint(&env, &switch_id).await;

        let mut manager = MockNvSwitchManager::default();
        if let Some(power_state) = backend_power_state {
            manager = manager.with_power_state(power_state);
        }
        let mut services = services_with_switch_manager(&env, Arc::new(manager));

        let mut switch = load_switch(&env, &switch_id).await;
        assert!(switch.status.is_none(), "fixture switch has no status yet");

        let outcome = run_handler(&mut services, &mut switch).await;
        commit_ready_outcome(outcome).await;

        let reloaded = load_switch(&env, &switch_id).await;
        match backend_power_state {
            Some(power_state) => {
                let status = reloaded
                    .status
                    .expect("observed power state should be persisted");
                assert_eq!(status.power_state, power_state);
                // A freshly populated status seeds the switch name from config
                // and leaves health unknown until a health source fills it in.
                assert_eq!(status.switch_name, "Switch1");
                assert_eq!(status.health_status, "");
            }
            None => assert!(
                reloaded.status.is_none(),
                "no observed power state must leave status unset"
            ),
        }
    }

    Ok(())
}

/// Updating the observed power state preserves the other `SwitchStatus` fields
/// an earlier observation populated.
#[sqlx_test]
async fn ready_power_state_update_preserves_other_status_fields(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, Some("Switch1".into()), None).await?;
    make_ready_with_endpoint(&env, &switch_id).await;

    // Seed a prior status carrying a name and health the poll must not clobber.
    let mut switch = load_switch(&env, &switch_id).await;
    switch.status = Some(SwitchStatus {
        switch_name: "gb-nvl-switch01".into(),
        power_state: "off".into(),
        health_status: "ok".into(),
    });
    {
        let mut txn = pool.begin().await?;
        db_switch::update(&switch, txn.as_mut()).await?;
        txn.commit().await?;
    }

    let mut services = services_with_switch_manager(
        &env,
        Arc::new(MockNvSwitchManager::default().with_power_state("on")),
    );
    let mut switch = load_switch(&env, &switch_id).await;
    let outcome = run_handler(&mut services, &mut switch).await;
    commit_ready_outcome(outcome).await;

    let status = load_switch(&env, &switch_id)
        .await
        .status
        .expect("status should remain present");
    assert_eq!(status.power_state, "on");
    assert_eq!(status.switch_name, "gb-nvl-switch01");
    assert_eq!(status.health_status, "ok");

    Ok(())
}
