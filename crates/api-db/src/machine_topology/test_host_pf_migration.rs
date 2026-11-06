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

use carbide_test_support::Case;
use carbide_test_support::Outcome::Yields;
use carbide_uuid::machine::{MachineIdSource, MachineType};
use model::hardware_info::{CpuInfo, DpuHostPf};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};

use super::*;

const HOST_PF_MIGRATION_VERSION: i64 = 20261104182622;
const HOST_PF_MIGRATION: &str =
    include_str!("../../migrations/20261104182622_preserve_registered_dpu_host_pfs.sql");
const LEGACY_REFRESH: &str = "UPDATE machine_topologies
    SET topology = jsonb_set(topology, '{discovery_data}', $2::jsonb),
        topology_update_needed = false, updated = NOW()
    WHERE machine_id = $1";

/// Preserves BF4 registration across old discovery replacements after migrating populated legacy inventory.
///
/// Reusing an old API connection proves that rollout protection comes from the database,
/// including when the replacement omits DpuData or supplies an empty collection.
#[crate::sqlx_test]
async fn migration_preserves_registered_host_pfs_across_old_api_refreshes(pool: sqlx::PgPool) {
    // Rebuild the predecessor schema because the harness has already applied the migration.
    crate::migrations::migrate_before(&pool, HOST_PF_MIGRATION_VERSION).await;
    let dpu_id = DpuMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x58; 32],
        MachineType::Dpu,
    ))
    .expect("DPU ID");
    let mut txn = pool.begin().await.expect("begin legacy inventory");
    crate::machine::create(
        &mut txn,
        None,
        &dpu_id.into(),
        ManagedHostState::Created,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .expect("create predecessor DPU");
    // The absent host_pfs field models legacy inventory; the other fields make a complete topology.
    let legacy_hardware = HardwareInfo {
        dpu_info: Some(DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // Establish BF4 before agent registration.
            firmware_version: "before-refresh".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    create_or_update(&mut txn, &dpu_id.into(), &legacy_hardware)
        .await
        .expect("persist legacy topology");
    txn.commit().await.expect("commit predecessor inventory");

    // Prepare the outgoing API's replacement SQL before installing the trigger and keep its connection alive.
    let mut old_writer = pool.acquire().await.expect("acquire old API connection");
    sqlx::query(LEGACY_REFRESH)
        .bind(dpu_id)
        .bind(sqlx::types::Json(&DiscoveryData {
            info: legacy_hardware.clone(),
        }))
        .execute(&mut *old_writer)
        .await
        .expect("refresh predecessor inventory with old SQL");
    let predecessor = find_latest_by_machine_ids(&mut old_writer, &[dpu_id])
        .await
        .expect("find populated predecessor")
        .remove(&dpu_id)
        .expect("predecessor topology");

    // Commit the shipped transactional migration before either API initializes registered inventory.
    let mut upgrade = pool.begin().await.expect("begin host-PF migration");
    sqlx::raw_sql(HOST_PF_MIGRATION)
        .execute(&mut *upgrade)
        .await
        .expect("apply shipped host-PF migration");
    upgrade.commit().await.expect("commit host-PF migration");

    // Reload to prove migration neither backfills an invented mapping nor changes inventory age.
    let migrated = find_latest_by_machine_ids(&mut old_writer, &[dpu_id])
        .await
        .expect("find migrated topology")
        .remove(&dpu_id)
        .expect("migrated topology");
    assert_eq!(migrated.topology, predecessor.topology);
    assert_eq!(migrated.updated, predecessor.updated);

    // Add one host PF to the complete legacy fixture for the first supporting BF4 agent report.
    let mut registered_hardware = legacy_hardware;
    let registered = registered_hardware.dpu_info.as_mut().expect("BF4 data");
    registered.host_pfs = vec![DpuHostPf {
        pci_controller_id: 1, // Identify the registered boot controller.
        pf_id: 0,
        mac_address: "96:ab:d0:3e:5f:a6".parse().expect("host-PF MAC"),
        physical_port_name: "c1pf0".to_string(),
        netdev_name: None,
        pci_address: None,
    }];
    // Register after migration to prove that protection still permits first initialization.
    let mut registration = pool.begin().await.expect("begin first agent registration");
    assert!(
        record_dpu_host_pfs(&mut registration, &dpu_id, registered)
            .await
            .expect("initialize host-PF inventory")
    );
    registration
        .commit()
        .await
        .expect("commit registered inventory");

    // Reload the committed registration before constructing old serializers' replacement forms.
    let initialized = find_latest_by_machine_ids(&mut old_writer, &[dpu_id])
        .await
        .expect("find initial registration")
        .remove(&dpu_id)
        .expect("registered topology");
    assert_eq!(
        initialized.topology.discovery_data.info,
        registered_hardware
    );

    // Construct an old API refresh with ordinary metadata, a conflicting part number, and no host_pfs.
    let old_hardware = HardwareInfo {
        cpu_info: vec![CpuInfo {
            model: "refreshed-cpu".to_string(), // Exercise ordinary discovery metadata beside DpuData.
            vendor: String::new(),
            sockets: 0,
            cores: 0,
            threads: 0,
        }],
        dpu_info: Some(DpuData {
            part_number: "900-9D3B6-00CV-AA0".to_string(), // Supply a conflicting known field alongside the omitted inventory.
            firmware_version: "refreshed-firmware".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let omitted = serde_json::to_value(DiscoveryData {
        info: old_hardware.clone(),
    })
    .expect("serialize old discovery");
    // Model explicit collection absence independently of an old serializer's omitted field.
    let mut empty = omitted.clone();
    empty["Info"]["dpu_info"]["host_pfs"] = serde_json::json!([]);
    // Model JSON null without changing the remaining legacy DpuData.
    let mut null = omitted.clone();
    null["Info"]["dpu_info"]["host_pfs"] = serde_json::Value::Null;
    // Remove the complete optional DpuData while leaving the ordinary CPU refresh present.
    let mut without_dpu = omitted.clone();
    without_dpu["Info"]["dpu_info"] = serde_json::Value::Null;
    // Retain only the protected fields in the expected result of a partial DpuData refresh.
    let mut refreshed = old_hardware.clone();
    let refreshed_dpu = refreshed.dpu_info.as_mut().expect("refreshed DPU data");
    let registered = registered_hardware
        .dpu_info
        .as_ref()
        .expect("registered DPU data");
    refreshed_dpu.part_number = registered.part_number.clone();
    refreshed_dpu.host_pfs = registered.host_pfs.clone();
    // Retain the complete registered DpuData only when the old report supplied none.
    let without_dpu_expected = HardwareInfo {
        dpu_info: Some(registered.clone()),
        ..old_hardware
    };

    for case in [
        // Old binaries omit unknown fields, so both registered inventory and BF4 classification must survive.
        Case {
            scenario: "old serializer omits host_pfs",
            input: omitted,
            expect: Yields(refreshed.clone()),
        },
        // An explicit empty collection cannot clear a registered mapping through a generic write.
        Case {
            scenario: "empty host_pfs preserves registration",
            input: empty,
            expect: Yields(refreshed.clone()),
        },
        // JSON null is another absence representation, not an inventory-clear operation.
        Case {
            scenario: "null host_pfs preserves registration",
            input: null,
            expect: Yields(refreshed),
        },
        // A generic report without DpuData must retain the registered DPU while accepting surrounding CPU facts.
        Case {
            scenario: "missing DpuData preserves registration",
            input: without_dpu,
            expect: Yields(without_dpu_expected),
        },
    ] {
        // Reset only the complete fixture so each old replacement starts from the same registered facts.
        sqlx::query(LEGACY_REFRESH)
            .bind(dpu_id)
            .bind(sqlx::types::Json(&DiscoveryData {
                info: registered_hardware.clone(),
            }))
            .execute(&mut *old_writer)
            .await
            .expect("restore registered fixture");

        let writer = &mut *old_writer;
        case.check_async(|discovery| async move {
            // Execute the old API's whole discovery replacement without the new Rust preservation code.
            sqlx::query(LEGACY_REFRESH)
                .bind(dpu_id)
                .bind(discovery)
                .execute(&mut *writer)
                .await
                .expect("apply old API refresh after migration");

            // Observe committed storage through the repository find API, including refreshed ordinary metadata.
            let persisted = find_latest_by_machine_ids(writer, &[dpu_id])
                .await
                .expect("find after old API refresh")
                .remove(&dpu_id)
                .expect("persisted topology");
            assert!(!persisted.topology_update_needed);
            Ok::<_, ()>(persisted.topology.discovery_data.info)
        })
        .await;
    }
}
