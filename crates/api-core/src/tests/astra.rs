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

use std::time::Duration;

use carbide_dpa::rpc::{DpaMetadata, Pfvni, SetVni};
use carbide_uuid::spx::NULL_SPX_PARTITION_ID;
use db::ConditionalWrite;
use model::dpa_interface::{DpaInterface, DpaInterfaceType, NewDpaInterface};
use model::instance::config::spx::SpxAttachmentType;
use model::machine::spx::{MachineSpxAttachmentStatusObservation, MachineSpxStatusObservation};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    AstraAttachmentStatus, AstraConfigStatus, AstraPhase, AstraStatus, DpaInterfaceDeletionRequest,
    DpuNetworkStatus, SpxPartitionCreationRequest,
};
use tonic::Request;

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
};
use crate::tests::common::postgres::wait_for_blocked_query;

// Mix both observation kinds to check ownership within one persisted machine
// record. This fixture does not model a supported hardware topology.
struct AstraStatusFixture {
    env: TestEnv,
    host: TestManagedHost,
    astra: DpaInterface,
    svpc_observation: MachineSpxAttachmentStatusObservation,
}

/// `create_astra_status_fixture` enables Astra reporting and seeds an active
/// SVPC interface whose exact observation must survive Astra replacement.
async fn create_astra_status_fixture(pool: sqlx::PgPool) -> AstraStatusFixture {
    let mut config = get_config();
    config.ewethers_config.as_mut().unwrap().astra_enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let host = create_managed_host(&env).await;
    let mut txn = env.db_txn().await;
    let astra = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host.id.into(),
            mac_address: "02:11:22:33:44:55".parse().unwrap(),
            device_type: "ConnectX9".to_string(),
            pci_name: "0000:cc:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Astra,
        },
        &mut txn,
    )
    .await
    .expect("create Astra interface");
    let svpc = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host.id.into(),
            mac_address: "02:11:22:33:44:66".parse().unwrap(),
            device_type: "ConnectX7".to_string(),
            pci_name: "0000:dd:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .expect("create SVPC interface");
    let svpc_observation = MachineSpxAttachmentStatusObservation {
        mac_address: svpc.mac_address,
        partition_id: Some(NULL_SPX_PARTITION_ID),
        config_version: Some(svpc.network_config.version),
        attachment_type: Some(SpxAttachmentType::Physical),
        virtual_function_id: None,
        observed_at: chrono::Utc::now(),
    };
    assert_eq!(
        db::machine::update_spx_status_observation(
            &mut txn,
            &host.id.into(),
            &MachineSpxStatusObservation {
                spx_attachments: vec![svpc_observation.clone()],
                observed_at: chrono::Utc::now(),
            },
        )
        .await
        .expect("seed SVPC observation"),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();

    AstraStatusFixture {
        env,
        host,
        astra,
        svpc_observation,
    }
}

/// `astra_status` reports one interface at its current admin network revision
/// with the requested phase. Individual cases can override the zero VNI.
fn astra_status(astra: &DpaInterface, phase: AstraPhase) -> AstraConfigStatus {
    AstraConfigStatus {
        astra_attachments_status: vec![AstraAttachmentStatus {
            mac_address: astra.mac_address.to_string(),
            vni: 0,
            revision: astra.network_config.version.to_string(),
            status: Some(AstraStatus {
                phase: phase.into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `record_astra_status` uses the public DPU-report path, including its required
/// health payload, so `None` and an explicit empty Astra report stay distinct.
async fn record_astra_status(fixture: &AstraStatusFixture, status: Option<AstraConfigStatus>) {
    fixture
        .env
        .api
        .record_dpu_network_status(Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(fixture.host.dpu().id),
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                ..Default::default()
            }),
            astra_config_status: status,
            ..Default::default()
        }))
        .await
        .expect("record Astra status through DPU report");
}

/// `add_second_astra` adds a separate MAC so tests can remove one interface or
/// report one pending interface while preserving another interface's readiness.
async fn add_second_astra(fixture: &AstraStatusFixture) -> DpaInterface {
    let mut txn = fixture.env.db_txn().await;
    let astra = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: fixture.host.id.into(),
            mac_address: "02:11:22:33:44:77".parse().unwrap(),
            device_type: "ConnectX9".to_string(),
            pci_name: "0000:ee:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Astra,
        },
        &mut txn,
    )
    .await
    .expect("create second Astra interface");
    txn.commit().await.unwrap();
    astra
}

// Delivered failures retract earlier readiness, while omission makes no new
// claim. Check committed state through the controller's sync predicate.
#[crate::sqlx_test]
async fn unsuccessful_astra_reports_clear_readiness_and_preserve_svpc(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    for (scenario, status, expected_synced) in [
        (
            "pending",
            Some(astra_status(&fixture.astra, AstraPhase::PhasePending)),
            false,
        ),
        (
            "deleting",
            Some(astra_status(&fixture.astra, AstraPhase::PhaseDeleting)),
            false,
        ),
        (
            "error",
            Some(astra_status(&fixture.astra, AstraPhase::PhaseError)),
            false,
        ),
        // No attachment succeeded in a delivered replacement report.
        ("empty", Some(AstraConfigStatus::default()), false),
        // No Astra report leaves the previous observation unchanged.
        ("absent", None, true),
    ] {
        record_astra_status(
            &fixture,
            Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
        )
        .await;
        let mut txn = fixture.env.db_txn().await;
        let machine = fixture.host.host().db_machine(&mut txn).await;
        txn.commit().await.unwrap();
        assert!(fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        ));

        record_astra_status(&fixture, status).await;
        let mut txn = fixture.env.db_txn().await;
        let machine = fixture.host.host().db_machine(&mut txn).await;
        txn.commit().await.unwrap();
        assert_eq!(
            fixture.astra.managed_host_network_config_version_synced(
                &None,
                &machine.status.spx_status_observation,
            ),
            expected_synced,
            "{scenario}"
        );
        let observation = machine.status.spx_status_observation.unwrap();
        assert_eq!(
            observation.spx_attachments.len(),
            if expected_synced { 2 } else { 1 },
            "{scenario}"
        );
        assert_eq!(
            observation
                .spx_attachments
                .iter()
                .find(|obs| obs.mac_address == fixture.svpc_observation.mac_address),
            Some(&fixture.svpc_observation),
            "{scenario}"
        );
    }

    // Even Ready cannot acknowledge a VNI that has no corresponding partition.
    record_astra_status(
        &fixture,
        Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
    )
    .await;
    let unknown_vni = 1;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    assert!(
        db::spx_partition::find_by(
            txn.as_mut(),
            db::ObjectColumnFilter::List(db::spx_partition::VniColumn, &[unknown_vni]),
        )
        .await
        .expect("look up unassigned Astra VNI")
        .is_empty()
    );
    txn.commit().await.unwrap();
    assert!(
        fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );

    let mut status = astra_status(&fixture.astra, AstraPhase::PhaseReady);
    status.astra_attachments_status[0].vni = unknown_vni;
    record_astra_status(&fixture, Some(status)).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        !fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    assert_eq!(
        machine
            .status
            .spx_status_observation
            .unwrap()
            .spx_attachments,
        vec![fixture.svpc_observation]
    );
}

// A replacement can acknowledge one Astra NIC and retract another. Inventory
// ownership must also prevent a Ready Astra entry from overwriting an SVPC MAC.
#[crate::sqlx_test]
async fn mixed_astra_report_retains_only_ready_interfaces_and_svpc(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    let second_astra = add_second_astra(&fixture).await;
    let partition = fixture
        .env
        .api
        .create_spx_partition(Request::new(SpxPartitionCreationRequest {
            metadata: Some(rpc::forge::Metadata {
                name: "astra-report-partition".to_string(),
                ..Default::default()
            }),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..Default::default()
        }))
        .await
        .expect("create SPX partition for Astra report")
        .into_inner();
    assert!(partition.vni > 0);
    let mut second_status = astra_status(&second_astra, AstraPhase::PhaseReady);
    second_status.astra_attachments_status[0].vni = i32::try_from(partition.vni).unwrap();
    let mut status = astra_status(&fixture.astra, AstraPhase::PhaseReady);
    status
        .astra_attachments_status
        .extend(second_status.astra_attachments_status.clone());
    record_astra_status(&fixture, Some(status)).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    for astra in [&fixture.astra, &second_astra] {
        assert!(astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        ));
    }

    // Give the live SVPC MAC the other Astra's nonzero VNI and a conflicting
    // revision. Exact SVPC equality below detects replacement or duplication.
    let mut svpc_status = second_status.astra_attachments_status[0].clone();
    svpc_status.mac_address = fixture.svpc_observation.mac_address.to_string();
    svpc_status.revision = fixture
        .svpc_observation
        .config_version
        .unwrap()
        .increment()
        .to_string();
    let mut status = astra_status(&fixture.astra, AstraPhase::PhasePending);
    status
        .astra_attachments_status
        .extend(second_status.astra_attachments_status);
    status.astra_attachments_status.push(svpc_status);
    record_astra_status(&fixture, Some(status)).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        !fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    assert!(
        second_astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    let observation = machine.status.spx_status_observation.unwrap();
    assert_eq!(observation.spx_attachments.len(), 2);
    assert_eq!(
        observation
            .spx_attachments
            .iter()
            .find(|obs| obs.mac_address == second_astra.mac_address)
            .unwrap()
            .partition_id,
        partition.id
    );
    assert_eq!(
        observation
            .spx_attachments
            .iter()
            .find(|obs| obs.mac_address == fixture.svpc_observation.mac_address),
        Some(&fixture.svpc_observation)
    );
}

// Delivered reports prune observations whose inventory disappeared, including
// a late empty report after the final Astra interface has been removed.
#[crate::sqlx_test]
async fn astra_reports_remove_deleted_interfaces_and_preserve_svpc(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    let second_astra = add_second_astra(&fixture).await;
    let mut status = astra_status(&fixture.astra, AstraPhase::PhaseReady);
    status
        .astra_attachments_status
        .extend(astra_status(&second_astra, AstraPhase::PhaseReady).astra_attachments_status);
    record_astra_status(&fixture, Some(status.clone())).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    for astra in [&fixture.astra, &second_astra] {
        assert!(astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        ));
    }

    // Stored observations can outlive inventory. This MAC has no active
    // interface, so the next present report must drop it, regardless of origin.
    let orphaned_svpc_mac = "02:11:22:33:44:88".parse().unwrap();
    let mut observation = machine.status.spx_status_observation.unwrap();
    observation
        .spx_attachments
        .push(MachineSpxAttachmentStatusObservation {
            mac_address: orphaned_svpc_mac,
            ..fixture.svpc_observation.clone()
        });
    observation.observed_at = chrono::Utc::now();
    let mut txn = fixture.env.db_txn().await;
    assert_eq!(
        db::machine::update_spx_status_observation(
            &mut txn,
            &fixture.host.id.into(),
            &observation,
        )
        .await
        .expect("seed observation for SVPC interface absent from inventory"),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();

    fixture
        .env
        .api
        .delete_dpa_interface(Request::new(DpaInterfaceDeletionRequest {
            id: Some(fixture.astra.id),
        }))
        .await
        .expect("delete first Astra interface");
    // A delayed Ready entry for a deleted interface must not restore it.
    record_astra_status(&fixture, Some(status)).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        !fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    assert!(
        second_astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    let observation = machine.status.spx_status_observation.unwrap();
    assert_eq!(observation.spx_attachments.len(), 2);
    assert!(
        observation
            .spx_attachments
            .iter()
            .all(|obs| obs.mac_address != orphaned_svpc_mac)
    );
    assert_eq!(
        observation
            .spx_attachments
            .iter()
            .find(|obs| obs.mac_address == fixture.svpc_observation.mac_address),
        Some(&fixture.svpc_observation)
    );

    fixture
        .env
        .api
        .delete_dpa_interface(Request::new(DpaInterfaceDeletionRequest {
            id: Some(second_astra.id),
        }))
        .await
        .expect("delete last Astra interface");
    // An in-flight empty report can arrive after removal; fresh configs omit Astra.
    record_astra_status(&fixture, Some(AstraConfigStatus::default())).await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert_eq!(
        machine
            .status
            .spx_status_observation
            .unwrap()
            .spx_attachments,
        vec![fixture.svpc_observation]
    );
}

// An Astra replacement must merge the observation committed by the previous
// transaction holding the host lock, even if its initial snapshot lacks it.
#[crate::sqlx_test]
async fn astra_report_preserves_svpc_committed_while_waiting_for_host(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    let mut blocker = fixture.env.db_txn().await;
    let blocker_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid() FROM machines WHERE id = $1 FOR UPDATE")
            .bind(fixture.host.id)
            .fetch_one(blocker.as_mut())
            .await
            .unwrap();
    // Change both fields so a stale merge cannot pass by retaining the seed.
    let updated_svpc = MachineSpxAttachmentStatusObservation {
        config_version: Some(fixture.svpc_observation.config_version.unwrap().increment()),
        observed_at: chrono::Utc::now(),
        ..fixture.svpc_observation.clone()
    };

    let concurrent_report = async {
        // Both the host lock and an unprotected update wait on this row.
        // Commit SVPC only after Astra has read its initial host snapshot.
        wait_for_blocked_query(&fixture.env.pool, blocker_pid, "machines").await;
        assert_eq!(
            db::machine::update_spx_status_observation(
                &mut blocker,
                &fixture.host.id.into(),
                &MachineSpxStatusObservation {
                    spx_attachments: vec![updated_svpc.clone()],
                    observed_at: chrono::Utc::now(),
                },
            )
            .await
            .expect("record concurrent SVPC observation"),
            ConditionalWrite::Applied(())
        );
        blocker.commit().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            record_astra_status(
                &fixture,
                Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
            ),
            concurrent_report,
        )
    })
    .await
    .expect("Astra report finishes after the concurrent SVPC report commits");

    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    let observation = machine.status.spx_status_observation.unwrap();
    assert_eq!(observation.spx_attachments.len(), 2);
    assert_eq!(
        observation
            .spx_attachments
            .iter()
            .find(|obs| obs.mac_address == updated_svpc.mac_address),
        Some(&updated_svpc)
    );
}

// An acknowledgment that began earlier must keep a newer committed value for
// its own MAC, even when it acquires the host lock after that value was stored.
#[crate::sqlx_test]
async fn older_svpc_report_preserves_newer_observation_for_same_mac(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let mut txn = env.db_txn().await;
    // This host has only SVPC inventory; the ordering guard is independent of Astra.
    let svpc = db::dpa_interface::persist(
        NewDpaInterface {
            machine_id: host.id.into(),
            mac_address: "02:11:22:33:44:66".parse().unwrap(),
            device_type: "SuperNIC".to_string(),
            pci_name: "0000:dd:00.0".to_string(),
            device_description: None,
            interface_type: DpaInterfaceType::Svpc,
        },
        &mut txn,
    )
    .await
    .expect("create SVPC interface");
    txn.commit().await.unwrap();

    let message = SetVni {
        metadata: Some(DpaMetadata {
            dpa_id: svpc.mac_address.to_string(),
            host_id: host.id.to_string(),
            revision: svpc.network_config.version.to_string(),
            ..Default::default()
        }),
        pf_info: Some(Pfvni {
            mac: svpc.mac_address.to_string(),
            vni: 0,
            ..Default::default()
        }),
    };
    let mut blocker = env.db_txn().await;
    let blocker_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid() FROM machines WHERE id = $1 FOR UPDATE")
            .bind(host.id)
            .fetch_one(blocker.as_mut())
            .await
            .unwrap();
    let concurrent_report = async {
        // The handler has timestamped its older acknowledgment before this wait.
        wait_for_blocked_query(&env.pool, blocker_pid, "machines").await;
        // Advance the real desired revision before storing its acknowledgment.
        // This distinguishes stale configuration from a change of timestamp alone.
        assert_eq!(
            db::dpa_interface::try_update_network_config(
                &mut blocker,
                &svpc.id,
                svpc.network_config.version,
                &svpc.network_config.value,
            )
            .await
            .expect("advance desired SVPC revision"),
            ConditionalWrite::Applied(svpc.id)
        );
        let current_svpc = db::dpa_interface::find_by_ids(blocker.as_mut(), &[svpc.id], false)
            .await
            .expect("read current SVPC revision")
            .remove(0);
        let newer_observation = MachineSpxStatusObservation {
            spx_attachments: vec![MachineSpxAttachmentStatusObservation {
                mac_address: svpc.mac_address,
                partition_id: Some(NULL_SPX_PARTITION_ID),
                config_version: Some(current_svpc.network_config.version),
                attachment_type: Some(SpxAttachmentType::Physical),
                virtual_function_id: None,
                observed_at: chrono::Utc::now(),
            }],
            observed_at: chrono::Utc::now(),
        };
        assert_eq!(
            db::machine::update_spx_status_observation(
                &mut blocker,
                &host.id.into(),
                &newer_observation,
            )
            .await
            .expect("commit newer SVPC acknowledgment before releasing the host lock"),
            ConditionalWrite::Applied(())
        );
        blocker.commit().await.unwrap();
        (current_svpc, newer_observation)
    };
    let (_, (current_svpc, newer_observation)) =
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                crate::dpa::handler::handle_test_dpa_message(
                    env.api.clone(),
                    message,
                    format!("dpa/ack/{}/SetVni", svpc.mac_address),
                ),
                concurrent_report,
            )
        })
        .await
        .expect("older SVPC report finishes after the newer acknowledgment commits");

    let mut txn = env.db_txn().await;
    let machine = host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        current_svpc.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    let observation = machine.status.spx_status_observation.unwrap();
    // The handler returns no result, so a later aggregate timestamp proves it
    // committed. Exact attachment equality rejects a duplicate or old value.
    assert!(observation.observed_at > newer_observation.observed_at);
    assert_eq!(
        observation.spx_attachments,
        newer_observation.spx_attachments
    );
}

// Exercise the SVPC writer's merge against a concurrent removal of another
// MAC's evidence. The mixed fixture tests storage ownership, not topology.
#[crate::sqlx_test]
async fn svpc_report_does_not_restore_astra_cleared_while_waiting_for_host(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    record_astra_status(
        &fixture,
        Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
    )
    .await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );

    // The revision stays current; a newer persisted timestamp proves that the
    // actual handler committed its acknowledgment rather than returning early.
    let svpc_mac = fixture.svpc_observation.mac_address;
    let message = SetVni {
        metadata: Some(DpaMetadata {
            dpa_id: svpc_mac.to_string(),
            host_id: fixture.host.id.to_string(),
            revision: fixture.svpc_observation.config_version.unwrap().to_string(),
            ..Default::default()
        }),
        pf_info: Some(Pfvni {
            mac: svpc_mac.to_string(),
            vni: 0,
            ..Default::default()
        }),
    };
    let mut blocker = fixture.env.db_txn().await;
    let blocker_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid() FROM machines WHERE id = $1 FOR UPDATE")
            .bind(fixture.host.id)
            .fetch_one(blocker.as_mut())
            .await
            .unwrap();
    let concurrent_retraction = async {
        wait_for_blocked_query(&fixture.env.pool, blocker_pid, "machines").await;
        // Model the replacement an explicit empty Astra report would commit.
        assert_eq!(
            db::machine::update_spx_status_observation(
                &mut blocker,
                &fixture.host.id.into(),
                &MachineSpxStatusObservation {
                    spx_attachments: vec![fixture.svpc_observation.clone()],
                    observed_at: chrono::Utc::now(),
                },
            )
            .await
            .expect("clear Astra observation before the SVPC report acquires the host lock"),
            ConditionalWrite::Applied(())
        );
        blocker.commit().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            crate::dpa::handler::handle_test_dpa_message(
                fixture.env.api.clone(),
                message,
                format!("dpa/ack/{svpc_mac}/SetVni"),
            ),
            concurrent_retraction,
        )
    })
    .await
    .expect("SVPC report finishes after Astra readiness is cleared");

    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    let observation = machine.status.spx_status_observation.as_ref().unwrap();
    let svpc = observation
        .spx_attachments
        .iter()
        .find(|obs| obs.mac_address == svpc_mac)
        .expect("SVPC observation remains present");
    assert!(svpc.observed_at > fixture.svpc_observation.observed_at);
    assert_eq!(
        svpc,
        &MachineSpxAttachmentStatusObservation {
            observed_at: svpc.observed_at,
            ..fixture.svpc_observation.clone()
        }
    );
    assert!(
        !fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        ),
        "SVPC must preserve Astra retraction committed while it waited"
    );
    assert_eq!(observation.spx_attachments.len(), 1);
}

// Inventory read before waiting for the host lock can admit a removed NIC.
// The seeded Ready entry and delayed report must both disappear after removal.
#[crate::sqlx_test]
async fn astra_report_rejects_interface_deleted_while_waiting_for_host(pool: sqlx::PgPool) {
    let fixture = create_astra_status_fixture(pool).await;
    record_astra_status(
        &fixture,
        Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
    )
    .await;
    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert!(
        fixture.astra.managed_host_network_config_version_synced(
            &None,
            &machine.status.spx_status_observation,
        )
    );
    let mut blocker = fixture.env.db_txn().await;
    let blocker_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid() FROM machines WHERE id = $1 FOR UPDATE")
            .bind(fixture.host.id)
            .fetch_one(blocker.as_mut())
            .await
            .unwrap();

    let concurrent_deletion = async {
        wait_for_blocked_query(&fixture.env.pool, blocker_pid, "machines").await;
        fixture
            .env
            .api
            .delete_dpa_interface(Request::new(DpaInterfaceDeletionRequest {
                id: Some(fixture.astra.id),
            }))
            .await
            .expect("delete Astra interface before the report acquires the host lock");
        blocker.commit().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            record_astra_status(
                &fixture,
                Some(astra_status(&fixture.astra, AstraPhase::PhaseReady)),
            ),
            concurrent_deletion,
        )
    })
    .await
    .expect("Astra report finishes after the interface is deleted");

    let mut txn = fixture.env.db_txn().await;
    let machine = fixture.host.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    assert_eq!(
        machine
            .status
            .spx_status_observation
            .unwrap()
            .spx_attachments,
        vec![fixture.svpc_observation]
    );
}
