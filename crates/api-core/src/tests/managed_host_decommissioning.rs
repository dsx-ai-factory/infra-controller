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

use std::future::Future;
use std::time::Duration;

use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{self as rpc, DecommissionManagedHostRequest};
use carbide_machine_controller::metrics::MachineMetrics;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use db::ConditionalWrite;
use model::dpa_interface::{DpaInterfaceControllerState, DpaInterfaceType, NewDpaInterface};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    CleanupState, DecommissioningState, DeconfiguringHostState, ManagedHostState,
    SpdmMeasuringState,
};
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerError};
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, TestManagedHost, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
};
use crate::tests::common::postgres::wait_for_blocked_query;

fn allocation_request(
    managed_host: &TestManagedHost,
    segment_id: NetworkSegmentId,
    allow_unhealthy_machine: bool,
) -> rpc::InstanceAllocationRequest {
    rpc::InstanceAllocationRequest {
        machine_id: Some(managed_host.id),
        config: Some(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            ..Default::default()
        }),
        metadata: Some(rpc::Metadata {
            name: "decommission-race-instance".to_string(),
            ..Default::default()
        }),
        allow_unhealthy_machine,
        ..Default::default()
    }
}

/// Queue both requests on the held Machine lock before allowing either to run.
async fn requests_in_machine_lock_order<F, S>(
    pool: &sqlx::PgPool,
    mut blocker: sqlx::Transaction<'_, sqlx::Postgres>,
    first: F,
    second: S,
) -> (F::Output, S::Output)
where
    F: Future,
    S: Future,
{
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(90), async {
        let run_second = async {
            // Match the query prefix: pg_stat_activity can truncate the expanded
            // Machine query before its locking clause.
            let first_pid = wait_for_blocked_query(pool, blocker_pid, "SELECT row_to_json").await;
            let release = async {
                wait_for_blocked_query(pool, first_pid, "SELECT row_to_json").await;
                blocker.commit().await.unwrap();
            };
            let (result, ()) = tokio::join!(second, release);
            result
        };
        tokio::join!(first, run_second)
    })
    .await
    .expect("competing requests should finish after the Machine lock is released")
}

#[crate::sqlx_test]
async fn decommission_and_allocation_serialize_in_both_commit_orders(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    for (scenario, decommission_first) in [
        ("decommission commits first", true),
        ("allocation commits first", false),
    ] {
        let managed_host = create_managed_host(&env).await;
        let mut blocker = env.db_txn().await;
        let host = db::machine::find_one(
            &mut *blocker,
            &managed_host.id,
            MachineSearchConfig {
                for_update: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(host.current_state(), &ManagedHostState::Ready, "{scenario}");
        assert!(!host.decommission_requested, "{scenario}");
        assert!(
            db::instance::find_id_by_machine_id(&mut *blocker, managed_host.id.as_machine_id())
                .await
                .unwrap()
                .is_none(),
            "{scenario}",
        );

        // The losing allocation uses the override to prove decommission cannot
        // be bypassed by allowing unhealthy hosts.
        let allocation = env.api.allocate_instance(Request::new(allocation_request(
            &managed_host,
            segment_id,
            decommission_first,
        )));
        let decommission =
            env.api
                .decommission_managed_host(Request::new(DecommissionManagedHostRequest {
                    machine_id: Some(managed_host.id),
                }));
        let (allocated, decommissioned) = if decommission_first {
            let (decommissioned, allocated) =
                requests_in_machine_lock_order(&env.pool, blocker, decommission, allocation).await;
            (allocated, decommissioned)
        } else {
            requests_in_machine_lock_order(&env.pool, blocker, allocation, decommission).await
        };
        assert_ne!(allocated.is_ok(), decommissioned.is_ok(), "{scenario}");
        let instance_id = if decommission_first {
            decommissioned.expect(scenario);
            let error = allocated.expect_err(scenario);
            assert_eq!(error.code(), Code::FailedPrecondition, "{scenario}");
            assert!(
                error.message().contains("decommission"),
                "{scenario}: {error}"
            );
            None
        } else {
            let instance_id = allocated.expect(scenario).into_inner().id.unwrap();
            let error = decommissioned.expect_err(scenario);
            assert_eq!(error.code(), Code::FailedPrecondition, "{scenario}");
            assert!(error.message().contains("instance"), "{scenario}: {error}");
            Some(instance_id)
        };

        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert_eq!(
            snapshot.managed_state,
            ManagedHostState::Ready,
            "{scenario}"
        );
        assert_eq!(
            snapshot.instance.as_ref().map(|i| i.id),
            instance_id,
            "{scenario}"
        );
        assert_eq!(
            snapshot.host_snapshot.decommission_requested, decommission_first,
            "{scenario}",
        );
        txn.commit().await.unwrap();

        // Check the Ready handler's persisted decision without transition-driven
        // requeues advancing the same host again while other tasks finish.
        env.run_machine_state_controller_iteration_without_requeue()
            .await;

        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        let expected_state = if decommission_first {
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::SuppressingSiteExplorer,
            }
        } else {
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
            }
        };
        assert_eq!(snapshot.managed_state, expected_state, "{scenario}");
        assert_eq!(
            snapshot.instance.as_ref().map(|i| i.id),
            instance_id,
            "{scenario}"
        );
        assert!(!snapshot.host_snapshot.decommission_requested, "{scenario}");
        txn.commit().await.unwrap();
    }
}

#[crate::sqlx_test]
async fn ready_instance_defers_decommission_until_release_cleanup_finishes(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.bom_validation.enabled = false;
    config.machine_validation_config.enabled = false;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance_id = env
        .api
        .allocate_instance(Request::new(allocation_request(
            &managed_host,
            segment_id,
            false,
        )))
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    let mut txn = env.db_txn().await;
    db::machine::set_decommission_requested(&mut txn, managed_host.id)
        .await
        .unwrap();
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, ManagedHostState::Ready);
    assert_eq!(snapshot.instance.as_ref().map(|i| i.id), Some(instance_id));
    assert!(snapshot.host_snapshot.decommission_requested);
    txn.commit().await.unwrap();

    // A second handler pass can skip measuring when SPDM is disabled.
    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::PreAssignedMeasuring {
            spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
        },
    );
    assert_eq!(snapshot.instance.as_ref().map(|i| i.id), Some(instance_id));
    assert!(snapshot.host_snapshot.decommission_requested);
    txn.commit().await.unwrap();

    env.api
        .release_instance(Request::new(rpc::InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();

    let mut cleanup_completed = false;
    let mut returned_to_ready = false;
    // Report the same reboot, network and cleanup observations as the release fixture,
    // but stop at Ready before a requeue can consume the pending decommission.
    for _ in 0..64 {
        managed_host.network_configured(&env).await;
        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        db::machine::update_reboot_time(&snapshot.host_snapshot, &mut txn)
            .await
            .unwrap();
        if matches!(
            snapshot.managed_state,
            ManagedHostState::WaitingForCleanup {
                cleanup_state: CleanupState::HostCleanup { .. },
                ..
            }
        ) {
            db::machine::update_cleanup_time(&snapshot.host_snapshot, &mut txn)
                .await
                .unwrap();
            cleanup_completed = true;
        }
        txn.commit().await.unwrap();

        env.run_machine_state_controller_iteration_without_requeue()
            .await;

        let mut txn = env.db_txn().await;
        let snapshot = managed_host.snapshot(&mut txn).await;
        assert!(
            snapshot.host_snapshot.decommission_requested,
            "release consumed the pending decommission in {:?}",
            snapshot.managed_state,
        );
        returned_to_ready = snapshot.managed_state == ManagedHostState::Ready;
        if returned_to_ready {
            assert!(snapshot.instance.is_none());
        }
        txn.commit().await.unwrap();
        if returned_to_ready {
            break;
        }
    }
    assert!(cleanup_completed, "release must complete host cleanup");
    assert!(returned_to_ready, "release must return the host to Ready");
    assert!(
        db::instance::find_by_id(&env.pool, instance_id)
            .await
            .unwrap()
            .is_none(),
    );

    env.run_machine_state_controller_iteration_without_requeue()
        .await;

    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::SuppressingSiteExplorer,
        },
    );
    assert!(snapshot.instance.is_none());
    assert!(!snapshot.host_snapshot.decommission_requested);
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn uefi_password_job_advances_to_completion_wait(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let host_id: HostMachineId = managed_host.id.into();
    let job_id = "JID_893866234996";

    for (name, job_state, iterations, expected_actions) in [
        (
            "scheduled job",
            libredfish::JobState::Scheduled,
            2,
            vec![RedfishSimAction::Power(
                libredfish::SystemPowerControl::ForceRestart,
            )],
        ),
        (
            "already-completed job",
            libredfish::JobState::Completed,
            1,
            vec![],
        ),
    ] {
        let mut txn = env.db_txn().await;
        db::machine::update_state(
            &mut txn,
            &host_id,
            &ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::DeconfiguringHost {
                    deconfiguring_state: DeconfiguringHostState::WaitForUefiPasswordJobScheduled {
                        job_id: job_id.to_string(),
                    },
                },
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        env.redfish_sim.set_job_state_sequence(vec![job_state]);
        let redfish_timepoint = env.redfish_sim.timepoint();
        for _ in 0..iterations {
            env.run_machine_state_controller_iteration().await;
        }

        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts(),
            expected_actions,
            "{name}",
        );
        let mut txn = env.db_txn().await;
        assert_eq!(
            managed_host
                .host()
                .db_machine(&mut txn)
                .await
                .current_state(),
            &ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::DeconfiguringHost {
                    deconfiguring_state: DeconfiguringHostState::WaitForUefiPasswordJobCompletion {
                        job_id: job_id.to_string(),
                    },
                },
            },
            "{name}",
        );
        txn.commit().await.unwrap();
    }
}

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
    // Multi-card writers lock by ID. Make the stale card the later write so
    // this still proves rollback of an earlier successful state update.
    let (first, second) = if first.id < second.id {
        (first, second)
    } else {
        (second, first)
    };
    let first_version = first.controller_state.version;
    let second_version = second.controller_state.version.increment();
    let card_ids = [first.id, second.id];
    let mut snapshot = managed_host.snapshot(&mut txn).await;
    // Match the controller's ID order: the first unlock precedes the stale one.
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
