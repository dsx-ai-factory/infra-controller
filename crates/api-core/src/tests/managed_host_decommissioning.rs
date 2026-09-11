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

use carbide_machine_controller::metrics::MachineMetrics;
use carbide_uuid::machine::HostMachineId;
use db::ConditionalWrite;
use model::dpa_interface::{DpaInterfaceControllerState, DpaInterfaceType, NewDpaInterface};
use model::machine::{DecommissioningState, DeconfiguringHostState, ManagedHostState};
use rpc::forge::DecommissionManagedHostRequest;
use rpc::forge::forge_server::Forge;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerError};
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};

#[crate::sqlx_test]
async fn stale_supernic_unlock_rolls_back_before_a_fresh_iteration(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let host_id: HostMachineId = managed_host.id.into();
    let clearing = ManagedHostState::Decommissioning {
        decommissioning_state: DecommissioningState::DeconfiguringHost {
            deconfiguring_state: DeconfiguringHostState::ClearSuperNicLockdown,
        },
    };
    let mut txn = env.db_txn().await;
    db::machine::update_state(&mut txn, &host_id, &clearing)
        .await
        .unwrap();
    let first = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host_id,
            mac_address: "00:11:22:33:44:55".parse().unwrap(),
            device_type: "SuperNIC".to_string(),
            pci_name: "0000:cc:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .unwrap();
    let second = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host_id,
            mac_address: "00:11:22:33:44:66".parse().unwrap(),
            device_type: "SuperNIC".to_string(),
            pci_name: "0000:dd:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .unwrap();
    let first_version = first.controller_state.version;
    let second_version = second.controller_state.version.increment();
    let card_ids = [first.id, second.id];
    let mut snapshot = managed_host.snapshot(&mut txn).await;
    // Preserve this order so the first unlock applies before the second rejects.
    snapshot.dpa_interface_snapshots = vec![first, second];
    txn.commit().await.unwrap();

    // Another writer advances the second card after we loaded the snapshot.
    let mut txn = env.db_txn().await;
    let applied = db::dpa_interface::try_update_controller_state(
        &mut txn,
        card_ids[1],
        snapshot.dpa_interface_snapshots[1].controller_state.version,
        second_version,
        &DpaInterfaceControllerState::Ready,
    )
    .await
    .unwrap();
    assert_eq!(applied, ConditionalWrite::Applied(()));
    txn.commit().await.unwrap();

    let mut services = env.machine_state_handler_services();
    let mut metrics = MachineMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();
    let mut ctx = StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    let result = env
        .machine_state_handler
        .handle_object_state(&host_id, &mut snapshot, &clearing, &mut ctx)
        .await;
    assert!(matches!(
        result,
        Err(StateHandlerError::IterationInvalidated { .. })
    ));

    let mut txn = env.db_txn().await;
    for (id, expected_state, expected_version) in [
        (
            card_ids[0],
            DpaInterfaceControllerState::Provisioning,
            first_version,
        ),
        (
            card_ids[1],
            DpaInterfaceControllerState::Ready,
            second_version,
        ),
    ] {
        let card = db::dpa_interface::find_by_ids(&mut *txn, &[id], false)
            .await
            .unwrap()
            .pop()
            .expect("SuperNIC exists");
        assert_eq!(card.controller_state.value, expected_state);
        assert_eq!(card.controller_state.version, expected_version);
    }
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration().await;

    let mut txn = env.db_txn().await;
    let host = managed_host.host().db_machine(&mut txn).await;
    assert_eq!(
        host.current_state(),
        &ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::DeconfiguringHost {
                deconfiguring_state: DeconfiguringHostState::WaitForSuperNicLockdown,
            },
        }
    );
    for (id, previous_version) in card_ids.into_iter().zip([first_version, second_version]) {
        let card = db::dpa_interface::find_by_ids(&mut *txn, &[id], false)
            .await
            .unwrap()
            .pop()
            .expect("SuperNIC exists");
        assert_eq!(
            card.controller_state.value,
            DpaInterfaceControllerState::Unlocking
        );
        assert_eq!(
            card.controller_state.version.version_nr(),
            previous_version.version_nr() + 1
        );
    }
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn decommission_requires_redfish_bfb_install_support(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let dpu_id = managed_host.dpu().id;

    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,firmware_version}',
             to_jsonb('BF-24.04-1'::text)
         )
         WHERE machine_id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();

    let error = env
        .api
        .decommission_managed_host(Request::new(DecommissionManagedHostRequest {
            machine_id: Some(managed_host.id),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains(&dpu_id.to_string()));
    assert!(error.message().contains("BF-24.04-1"));
    assert!(
        error
            .message()
            .contains("dpus do not support bfb installation")
    );

    let mut txn = env.pool.begin().await.unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    assert!(!host.decommission_requested);
    txn.commit().await.unwrap();

    sqlx::query(
        "UPDATE machine_topologies
         SET topology = jsonb_set(
             topology,
             '{bmc_info,firmware_version}',
             to_jsonb('BF-24.10-0'::text)
         )
         WHERE machine_id = $1",
    )
    .bind(dpu_id)
    .execute(&env.pool)
    .await
    .unwrap();

    env.api
        .decommission_managed_host(Request::new(DecommissionManagedHostRequest {
            machine_id: Some(managed_host.id),
        }))
        .await
        .unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    assert!(host.decommission_requested);
    txn.commit().await.unwrap();
}
