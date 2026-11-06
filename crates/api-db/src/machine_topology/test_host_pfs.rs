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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::hardware_info::{DmiData, DpuHostPf, DpuHostPfError};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};

use super::*;
use crate::test_support::postgres::wait_for_blocked_query;

/// Commits BF4 topology without registered host-PFs so competing transactions can observe the same uninitialized inventory.
async fn create_unregistered_bf4_topology(
    pool: &sqlx::PgPool,
    identity: u8,
) -> (DpuMachineId, DpuData) {
    // Use a distinct stable DPU ID for each scenario while retaining the same BF4 report.
    let dpu_id = DpuMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [identity; 32],
        MachineType::Dpu,
    ))
    .expect("DPU ID");
    let mut txn = pool.begin().await.expect("begin topology test");
    crate::machine::create(
        &mut txn,
        None,
        &dpu_id.into(),
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .expect("create DPU");
    let registered = DpuData {
        part_number: "900-9D4B4-CWAA-TSA".to_string(), // Require BF4 inventory before boot selection.
        factory_mac_address: "02:00:00:00:00:f0".to_string(),
        host_pfs: vec![DpuHostPf {
            pci_controller_id: 1, // Select the site's default host-PF controller.
            pf_id: 0,             // Use the required boot PF rather than a diagnostic PF.
            mac_address: "96:ab:d0:3e:5f:a6".parse().expect("valid host-PF MAC"),
            physical_port_name: "c1pf0".to_string(),
            netdev_name: None,
            pci_address: None,
        }],
        ..Default::default()
    };
    // Generic discovery is deliberately supplied the field to prove it cannot initialize agent-owned inventory.
    let hardware = HardwareInfo {
        dpu_info: Some(registered.clone()),
        ..Default::default()
    };
    create_or_update(&mut txn, &dpu_id.into(), &hardware)
        .await
        .expect("create generic topology");
    // Verify persisted generic discovery leaves agent inventory uninitialized and does not request a refresh.
    let initial = find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("find generic topology")
        .remove(&dpu_id)
        .expect("generic topology exists");
    assert!(!initial.topology_update_needed);
    assert!(
        initial
            .topology
            .discovery_data
            .info
            .dpu_info
            .expect("DPU data")
            .host_pfs
            .is_empty()
    );

    txn.commit().await.expect("commit generic topology");
    (dpu_id, registered)
}

/// Merges first agent inventory despite topology throttling and preserves it across idempotent reports and generic refreshes.
#[crate::sqlx_test]
async fn registered_host_pfs_survive_topology_throttling_and_refresh(pool: sqlx::PgPool) {
    // Start with Redfish-created DPU inventory whose generic refresh flag is false.
    let (dpu_id, registered) = create_unregistered_bf4_topology(&pool, 0x57).await;
    let mut txn = pool.begin().await.expect("begin topology registration");

    // First phone-home initializes the mapping independently of generic inventory age.
    assert!(
        record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
            .await
            .expect("initialize mapping")
    );
    assert!(
        !record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
            .await
            .expect("repeat registration")
    );
    // A generic writer cannot replace registered boot MACs with its own nonempty inventory.
    let mut conflicting = registered.clone();
    conflicting.host_pfs[0].mac_address = "96:ab:d0:3e:5f:a8".parse().expect("conflicting MAC");
    set_topology_update_needed(&mut txn, &dpu_id.into(), true)
        .await
        .expect("request conflicting generic refresh");
    create_or_update(
        &mut txn,
        &dpu_id.into(),
        &HardwareInfo {
            dpu_info: Some(conflicting),
            ..Default::default()
        },
    )
    .await
    .expect("refresh generic DPU data");
    let refreshed = find_latest_by_machine_ids(&mut txn, &[dpu_id])
        .await
        .expect("find generic refresh");
    assert_eq!(
        refreshed[&dpu_id]
            .topology
            .discovery_data
            .info
            .dpu_info
            .as_ref()
            .expect("registered DPU data")
            .host_pfs,
        registered.host_pfs
    );

    // A generic refresh with no DpuData cannot discard registered host-PF identity.
    set_topology_update_needed(&mut txn, &dpu_id.into(), true)
        .await
        .expect("request generic refresh");
    create_or_update(&mut txn, &dpu_id.into(), &HardwareInfo::default())
        .await
        .expect("refresh other discovery facts");
    txn.commit().await.expect("commit persisted inventory");

    // Reload through the repository find API rather than trusting mutation return values.
    let mut reader = pool.acquire().await.expect("read persisted topology");
    let persisted = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find topology");
    assert_eq!(
        persisted[&dpu_id]
            .topology
            .discovery_data
            .info
            .dpu_info
            .as_ref()
            .expect("registered DPU data")
            .host_pfs,
        registered.host_pfs
    );

    // Changed boot MACs must fail closed while the stored key set remains unchanged.
    let mut drifted = registered.clone();
    drifted.host_pfs[0].mac_address = "96:ab:d0:3e:5f:a8".parse().expect("replacement MAC");
    let mut drift_txn = pool.begin().await.expect("begin drift report");
    assert!(matches!(
        record_dpu_host_pfs(&mut drift_txn, &dpu_id, &drifted).await,
        Err(DatabaseError::DpuHostPf {
            source: DpuHostPfError::BootMacDrift { .. },
            ..
        })
    ));
    drift_txn
        .rollback()
        .await
        .expect("roll back rejected report");

    // Rejection leaves the durable identity and MAC intact.
    let preserved = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find after rejection");
    assert_eq!(
        preserved[&dpu_id]
            .topology
            .discovery_data
            .info
            .dpu_info
            .as_ref()
            .expect("DPU data")
            .host_pfs,
        registered.host_pfs
    );
}

/// Rejects competing first registrations that change the committed boot MAC and treats an identical waiter as idempotent, preventing two initial reports from replacing each other.
#[crate::sqlx_test]
async fn competing_first_registrations_preserve_committed_host_pfs(pool: sqlx::PgPool) {
    for (identity, scenario, changes_boot_mac) in [
        // Identical reports must recognize the committed mapping instead of both claiming initialization.
        (0x58, "identical report", false),
        // A different boot MAC must be rejected after the first registration commits its identity.
        (0x59, "conflicting boot MAC", true),
    ] {
        // Commit empty BF4 inventory before either registration opens its transaction.
        let (dpu_id, registered) = create_unregistered_bf4_topology(&pool, identity).await;
        let mut competing = registered.clone();
        // Change only the competing MAC so both reports still claim the same controller/PF identity.
        if changes_boot_mac {
            competing.host_pfs[0].mac_address =
                "96:ab:d0:3e:5f:a8".parse().expect("competing boot MAC");
        }
        let mut first = pool.begin().await.expect("begin first registration");
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *first)
            .await
            .expect("first registration backend");
        assert!(
            record_dpu_host_pfs(&mut first, &dpu_id, &registered)
                .await
                .expect("initialize first mapping")
        );

        // Keep the first mapping uncommitted until the other transaction waits on its topology row.
        let mut second = pool.begin().await.expect("begin competing registration");
        let result = {
            let registration = record_dpu_host_pfs(&mut second, &dpu_id, &competing);
            tokio::pin!(registration);
            tokio::select! {
                result = &mut registration => panic!("{scenario} finished before the first commit: {result:?}"),
                _ = wait_for_blocked_query(&pool, first_pid, "SELECT machine_id FROM machine_topologies") => {}
            }
            // Publish the first mapping only after the competing report reaches its locking read.
            first.commit().await.expect("commit first registration");
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut registration)
                .await
                .expect("competing registration completes after commit")
        };

        // The waiter must compare with committed inventory before deciding whether it initialized anything.
        if changes_boot_mac {
            assert!(matches!(
                result,
                Err(DatabaseError::DpuHostPf {
                    source: DpuHostPfError::BootMacDrift { .. },
                    ..
                })
            ));
            second.rollback().await.expect("reject competing identity");
        } else {
            assert!(!result.expect("accept identical registration"));
            second
                .commit()
                .await
                .expect("commit identical registration");
        }

        // Reload the durable mapping so a successful return cannot hide an overwritten first registration.
        let mut reader = pool.acquire().await.expect("read committed inventory");
        let persisted = find_latest_by_machine_ids(&mut reader, &[dpu_id])
            .await
            .expect("find after competing registrations");
        assert_eq!(
            persisted[&dpu_id]
                .topology
                .discovery_data
                .info
                .dpu_info
                .as_ref()
                .expect("registered DPU data")
                .host_pfs,
            registered.host_pfs,
            "{scenario}"
        );
    }
}

/// Reloads registered host-PFs after a generic refresh waits for registration, preserving the boot MAC while still publishing unrelated discovery updates.
#[crate::sqlx_test]
async fn generic_refresh_preserves_a_registration_committed_while_waiting(pool: sqlx::PgPool) {
    // Publish the refresh request before registration so a generic writer must refresh the visible topology.
    let (dpu_id, registered) = create_unregistered_bf4_topology(&pool, 0x5a).await;
    let mut setup = pool.begin().await.expect("begin pending refresh");
    set_topology_update_needed(&mut setup, &dpu_id.into(), true)
        .await
        .expect("request refresh before registration");
    setup.commit().await.expect("commit pending refresh");
    let hardware = HardwareInfo {
        dmi_data: Some(DmiData {
            bios_version: "refreshed-bios".to_string(), // This diagnostic must survive registration too.
            ..Default::default()
        }),
        ..Default::default() // Omitting DpuData must not remove registered BF4 facts.
    };
    let mut registration = pool.begin().await.expect("begin first registration");
    let registration_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *registration)
        .await
        .expect("registration backend");
    assert!(
        record_dpu_host_pfs(&mut registration, &dpu_id, &registered)
            .await
            .expect("initialize uncommitted mapping")
    );

    // The refresh reads empty inventory first, then waits before composing its replacement discovery data.
    let mut refreshing = pool.begin().await.expect("begin generic refresh");
    {
        let machine_id = dpu_id.into();
        let refresh = create_or_update(&mut refreshing, &machine_id, &hardware);
        tokio::pin!(refresh);
        tokio::select! {
            result = &mut refresh => panic!("generic refresh finished before registration committed: {result:?}"),
            _ = wait_for_blocked_query(&pool, registration_pid, "SELECT machine_id FROM machine_topologies") => {}
        }
        // Publish inventory after refresh waits so its earlier empty snapshot cannot be trusted.
        registration
            .commit()
            .await
            .expect("commit registered inventory");
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut refresh)
            .await
            .expect("refresh completes after registration commits")
            .expect("publish generic refresh");
    }
    refreshing
        .commit()
        .await
        .expect("commit generic discovery data");

    // Reload both facts to prove refresh did real work without restoring its stale empty inventory.
    let mut reader = pool.acquire().await.expect("read refreshed inventory");
    let persisted = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find after concurrent refresh");
    let info = &persisted[&dpu_id].topology.discovery_data.info;
    assert_eq!(
        info.dpu_info
            .as_ref()
            .expect("registered DPU data")
            .host_pfs,
        registered.host_pfs
    );
    assert_eq!(
        info.dmi_data
            .as_ref()
            .expect("refreshed DMI data")
            .bios_version,
        "refreshed-bios"
    );
    assert!(!persisted[&dpu_id].topology_update_needed);
}

/// Keeps Redfish's BF4 requirement through generic refresh before first registration, so omitted or unrecognized part numbers cannot authorize empty inventory.
#[crate::sqlx_test]
async fn generic_refresh_preserves_unregistered_bf4_classification(pool: sqlx::PgPool) {
    for (identity, scenario, incoming) in [
        // Missing DpuData must retain classification even before the agent has registered host-PFs.
        (0x5b, "missing DPU data", None),
        // A blank part number must not turn a classified BF4 into a legacy DPU.
        (0x5c, "blank part number", Some(DpuData::default())),
        // An unrecognized nonempty part number must not bypass the same requirement.
        (
            0x5d,
            "unrecognized part number",
            Some(DpuData {
                part_number: "unrecognized".to_string(),
                ..Default::default()
            }),
        ),
    ] {
        // Construct the prior BF4 classification independently of incoming generic data.
        let (dpu_id, registered) = create_unregistered_bf4_topology(&pool, identity).await;
        let mut txn = pool.begin().await.expect("begin classification refresh");
        set_topology_update_needed(&mut txn, &dpu_id.into(), true)
            .await
            .expect("request classification refresh");

        // Refresh the generic report before registration attempts to enforce its inventory requirement.
        create_or_update(
            &mut txn,
            &dpu_id.into(),
            &HardwareInfo {
                dpu_info: incoming, // Vary the report while preserving the established BF4 requirement.
                ..Default::default()
            },
        )
        .await
        .expect("refresh incomplete discovery data");
        txn.commit().await.expect("commit preserved classification");

        // Reload the BF4 classification and reject an empty, unclassified agent report against that stored policy.
        let mut reader = pool.acquire().await.expect("read prior BF4 classification");
        let persisted = find_latest_by_machine_ids(&mut reader, &[dpu_id])
            .await
            .expect("find classification after refresh");
        let dpu = persisted[&dpu_id]
            .topology
            .discovery_data
            .info
            .dpu_info
            .as_ref()
            .expect("BF4 DPU data survives refresh");
        assert_eq!(dpu.part_number, registered.part_number, "{scenario}");
        assert!(dpu.host_pfs.is_empty(), "{scenario}");
        let mut registration = pool.begin().await.expect("begin incomplete registration");
        assert!(matches!(
            record_dpu_host_pfs(&mut registration, &dpu_id, &DpuData::default()).await,
            Err(DatabaseError::DpuHostPf {
                source: DpuHostPfError::MissingInventory,
                ..
            })
        ));
        registration
            .rollback()
            .await
            .expect("reject missing inventory");
    }
}

/// Publishes an agent's first BF4 classification with its inventory while generic refresh is throttled, preventing newly identified BF4 hardware from using legacy selection.
#[crate::sqlx_test]
async fn first_registration_persists_bf4_classification_despite_topology_throttling(
    pool: sqlx::PgPool,
) {
    // Model prior discovery that stored DpuData without identifying the BF4 part number.
    let (dpu_id, registered) = create_unregistered_bf4_topology(&pool, 0x5e).await;
    sqlx::query("UPDATE machine_topologies SET topology = topology #- '{discovery_data,Info,dpu_info,part_number}' WHERE machine_id = $1")
        .bind(dpu_id)
        .execute(&pool)
        .await
        .expect("remove prior BF4 classification");
    let mut txn = pool.begin().await.expect("begin classified registration");

    // Registration must publish classification without requiring a generic discovery refresh.
    assert!(
        record_dpu_host_pfs(&mut txn, &dpu_id, &registered)
            .await
            .expect("initialize classified inventory")
    );
    txn.commit().await.expect("commit classified inventory");

    // Reload classification and inventory together while confirming the refresh throttle remains unchanged.
    let mut reader = pool.acquire().await.expect("read classified inventory");
    let persisted = find_latest_by_machine_ids(&mut reader, &[dpu_id])
        .await
        .expect("find registered classification");
    let dpu = persisted[&dpu_id]
        .topology
        .discovery_data
        .info
        .dpu_info
        .as_ref()
        .expect("registered BF4 data");
    assert_eq!(dpu.part_number, registered.part_number);
    assert_eq!(dpu.host_pfs, registered.host_pfs);
    assert!(!persisted[&dpu_id].topology_update_needed);
}

/// Returns an unchanged host topology while another transaction locks it, avoiding an added topology wait after secure discovery locks the host interface.
#[crate::sqlx_test]
async fn unchanged_host_discovery_does_not_wait_for_topology_writer(pool: sqlx::PgPool) {
    // Commit a stable host whose generic discovery data does not need refreshing.
    let host_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x5f; 32],
        MachineType::Host,
    );
    let mut setup = pool.begin().await.expect("begin host topology fixture");
    crate::machine::create(
        &mut setup,
        None,
        &host_id,
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .expect("create stable host");
    create_or_update(&mut setup, &host_id, &HardwareInfo::default())
        .await
        .expect("create unchanged host topology");
    setup.commit().await.expect("commit unchanged host");
    let mut writer = pool.begin().await.expect("begin topology writer");
    lock_by_machine_id(&mut writer, &host_id)
        .await
        .expect("hold host topology row");

    // An unchanged discovery must finish before the topology writer releases its row lock.
    let mut discovery = pool.begin().await.expect("begin unchanged host discovery");
    let unchanged = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        create_or_update(&mut discovery, &host_id, &HardwareInfo::default()),
    )
    .await
    .expect("unchanged host discovery must not wait for topology")
    .expect("read unchanged topology");
    assert_eq!(unchanged.machine_id, host_id);
    assert!(!unchanged.topology_update_needed);
    discovery
        .commit()
        .await
        .expect("commit unchanged discovery");
    writer.rollback().await.expect("release topology writer");
}
