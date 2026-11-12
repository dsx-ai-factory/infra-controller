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

use std::collections::HashSet;
use std::time::Duration;

use carbide_machine_controller::metrics::MachineMetrics;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::HostMachineId;
use db::ConditionalWrite;
use libredfish::{PowerState, RedfishError, SystemPowerControl};
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use model::dpa_interface::{DpaInterfaceControllerState, DpaInterfaceType, NewDpaInterface};
use model::machine::{
    DecommissioningState, DeconfiguringHostState, MachineLastRebootRequestedMode, ManagedHostState,
};
use rpc::forge::DecommissionManagedHostRequest;
use rpc::forge::forge_server::Forge;
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerError};
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};

#[crate::sqlx_test]
async fn dpu_bmc_reset_recovers_after_losing_reachability(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let host_id: HostMachineId = managed_host.id.into();
    let host_completed = HashSet::from([host_id.into()]);
    let mut txn = env.db_txn().await;
    let host_mac = managed_host
        .host()
        .db_machine(&mut txn)
        .await
        .status
        .bmc_info
        .mac
        .unwrap();
    let dpu = managed_host.dpu().db_machine(&mut txn).await;
    let dpu_mac = dpu.status.bmc_info.mac.unwrap();
    let dpu_bmc_ip = dpu.status.bmc_info.ip.unwrap().to_string();
    db::bmc_suppression::upsert(
        &mut txn,
        &NewBmcSuppression {
            bmc_mac_address: host_mac,
            subsystem: BmcSuppressionSubsystem::Dhcp,
            source: BmcSuppressionSource::Decommissioning,
            reason: "host BMC reset already completed".to_string(),
        },
    )
    .await
    .unwrap();
    db::machine::update_state(
        &mut txn,
        &host_id,
        &ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::FactoryResettingBmcs {
                completed: host_completed.clone(),
            },
        },
    )
    .await
    .unwrap();
    let initial_version = managed_host.host().db_machine(&mut txn).await.state.version;
    txn.commit().await.unwrap();

    struct Step {
        name: &'static str,
        unreachable: bool,
        fail_reset: bool,
        suppressed: bool,
        completed: bool,
        client_calls: usize,
        action: Option<RedfishSimAction>,
    }
    for step in [
        Step {
            name: "DPU BMC still recovering from the host power cycle",
            unreachable: true,
            fail_reset: false,
            suppressed: false,
            completed: false,
            client_calls: 1,
            action: None,
        },
        Step {
            name: "BMC answers the readiness check but its reset fails",
            unreachable: false,
            fail_reset: true,
            suppressed: true,
            completed: false,
            client_calls: 1,
            action: Some(RedfishSimAction::BmcResetToDefaultsFailed),
        },
        Step {
            name: "restore DHCP while the BMC is unreachable",
            unreachable: true,
            fail_reset: false,
            suppressed: false,
            completed: false,
            client_calls: 0,
            action: None,
        },
        Step {
            name: "retry succeeds after BMC connectivity recovers",
            unreachable: false,
            fail_reset: false,
            suppressed: true,
            completed: true,
            client_calls: 1,
            action: Some(RedfishSimAction::BmcResetToDefaults),
        },
    ] {
        if step.unreachable {
            env.redfish_sim
                .set_create_client_error("DPU BMC has no DHCP address");
        } else {
            env.redfish_sim.clear_create_client_error();
        }
        let reset_request = step
            .fail_reset
            .then(|| env.redfish_sim.intercept_next_bmc_factory_reset());
        let timepoint = env.redfish_sim.timepoint();
        let client_calls = env.redfish_sim.create_client_calls().len();
        tokio::time::timeout(Duration::from_secs(30), async {
            let observe_reset = async {
                if let Some(reset_request) = reset_request {
                    let request = reset_request.await.unwrap();
                    assert_eq!(request.host, dpu_bmc_ip);
                    // A separate connection must see suppression before the
                    // reset responds: it cannot remain in an open transaction.
                    assert!(
                        db::bmc_suppression::find(
                            &env.pool,
                            dpu_mac,
                            BmcSuppressionSubsystem::Dhcp,
                            BmcSuppressionSource::Decommissioning,
                        )
                        .await
                        .unwrap()
                        .is_some()
                    );
                    request
                        .respond
                        .send(Err(RedfishError::GenericError {
                            error: "transient timeout sending the factory-reset request"
                                .to_string(),
                        }))
                        .unwrap();
                }
            };
            tokio::join!(
                env.run_machine_state_controller_iteration_without_requeue(),
                observe_reset
            );
        })
        .await
        .expect("controller tick and reset observation should finish");

        let mut txn = env.db_txn().await;
        let host = managed_host.host().db_machine(&mut txn).await;
        let mut expected_completed = host_completed.clone();
        if step.completed {
            expected_completed.insert(dpu.id.into());
            assert_ne!(host.state.version, initial_version, "{}", step.name);
        } else {
            // Recovery writes must not restart the lack-of-progress SLA clock.
            assert_eq!(host.state.version, initial_version, "{}", step.name);
        }
        assert_eq!(
            host.current_state(),
            &ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::FactoryResettingBmcs {
                    completed: expected_completed,
                },
            },
            "{}",
            step.name,
        );
        for (mac, suppressed) in [(host_mac, true), (dpu_mac, step.suppressed)] {
            assert_eq!(
                db::bmc_suppression::find(
                    &mut *txn,
                    mac,
                    BmcSuppressionSubsystem::Dhcp,
                    BmcSuppressionSource::Decommissioning,
                )
                .await
                .unwrap()
                .is_some(),
                suppressed,
                "{}: suppression for {mac}",
                step.name,
            );
        }
        txn.commit().await.unwrap();
        let actions = env.redfish_sim.actions_since(&timepoint);
        assert_eq!(
            actions.for_host(&dpu_bmc_ip),
            step.action.into_iter().collect::<Vec<_>>(),
            "{}",
            step.name
        );
        assert_eq!(
            env.redfish_sim.create_client_calls()[client_calls..]
                .iter()
                .filter(|call| call.host == dpu_bmc_ip)
                .count(),
            step.client_calls,
            "{}: Redfish client creation",
            step.name,
        );
    }
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

// A BMC without an AC power cycle action leaves the host off and continues;
// any other power cycle error keeps the host in PowerCyclingHost.
#[crate::sqlx_test]
async fn power_cycling_host_continues_when_ac_powercycle_is_not_supported(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let host_id: HostMachineId = managed_host.id.into();
    env.redfish_sim.set_ac_powercycle_supported(true);

    for (name, error, expected_state, expected_mode) in [
        (
            "not supported",
            RedfishError::NotSupported("no AC power cycle action".to_string()),
            DecommissioningState::PoweringOnHost,
            Some(MachineLastRebootRequestedMode::PowerOff),
        ),
        (
            "other error",
            RedfishError::GenericError {
                error: "BMC request failed".to_string(),
            },
            DecommissioningState::PowerCyclingHost,
            None,
        ),
    ] {
        let mut txn = env.db_txn().await;
        db::machine::update_state(
            &mut txn,
            &host_id,
            &ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::PowerCyclingHost,
            },
        )
        .await
        .unwrap();
        let bmc_ip = managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .status
            .bmc_info
            .ip
            .unwrap()
            .to_string();
        txn.commit().await.unwrap();

        env.redfish_sim.set_power_state(&bmc_ip, PowerState::On);
        env.redfish_sim
            .fail_next_power_action_with(SystemPowerControl::ACPowercycle, error);
        let redfish_timepoint = env.redfish_sim.timepoint();
        env.run_machine_state_controller_iteration().await;

        assert_eq!(
            env.redfish_sim
                .actions_since(&redfish_timepoint)
                .all_hosts(),
            vec![
                RedfishSimAction::Power(SystemPowerControl::ForceOff),
                RedfishSimAction::PowerFailed(SystemPowerControl::ACPowercycle),
            ],
            "{name}",
        );
        let mut txn = env.db_txn().await;
        let host = managed_host.host().db_machine(&mut txn).await;
        assert_eq!(
            host.current_state(),
            &ManagedHostState::Decommissioning {
                decommissioning_state: expected_state,
            },
            "{name}",
        );
        if let Some(expected_mode) = expected_mode {
            assert_eq!(
                host.status.last_reboot_requested.as_ref().unwrap().mode,
                expected_mode,
                "{name}",
            );
        }
        txn.commit().await.unwrap();
    }
}
