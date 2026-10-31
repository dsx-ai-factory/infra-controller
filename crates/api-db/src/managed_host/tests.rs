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

use carbide_test_support::{Case, Outcome};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{AsMachineId, MachineId, MachineIdSource, MachineType};
use model::address_selection_strategy::AddressSelectionStrategy;
use model::hardware_info::{HardwareInfo, TpmDescription};
use model::machine::{
    CURRENT_STATE_MODEL_VERSION, HardwareInfoScope, LoadSnapshotOptions, ManagedHostState,
};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentControllerState;
use model::test_support::machine_snapshot::{dpu_hardware_info, host_hardware_info};

use crate::test_support::network_segment::admin_segment;

#[crate::sqlx_test]
async fn load_by_machine_ids_preserves_shared_host_snapshot(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x77; 32],
        MachineType::Host,
    );
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x78; 32],
        MachineType::Dpu,
    );
    let mut txn = pool.begin().await?;
    for machine_id in [host_id, dpu_id] {
        crate::machine::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
    }
    let segment = crate::network_segment::persist(
        admin_segment("snapshot-test", "192.0.2.0/24", "192.0.2.1", 3),
        txn.as_mut(),
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let interface = crate::machine_interface::create(
        txn.as_mut(),
        std::slice::from_ref(&segment),
        &"02:00:00:00:00:78".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    crate::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::Machine(host_id),
        txn.as_mut(),
    )
    .await?;
    crate::machine_interface::associate_interface_with_dpu_machine(
        &interface.id,
        &dpu_id,
        txn.as_mut(),
    )
    .await?;

    let snapshots = super::load_by_machine_ids(
        txn.as_mut(),
        &[host_id, dpu_id],
        LoadSnapshotOptions {
            include_instance_data: false,
            ..Default::default()
        },
    )
    .await?;

    assert_eq!(snapshots.len(), 2);
    for machine_id in [host_id, dpu_id] {
        let snapshot = snapshots
            .get(&machine_id)
            .expect("both requested machines should resolve to their shared host snapshot");
        assert_eq!(snapshot.host_snapshot.id.to_machine_id(), host_id);
        assert_eq!(
            snapshot
                .dpu_snapshots
                .iter()
                .map(|dpu| dpu.id.to_machine_id())
                .collect::<Vec<_>>(),
            vec![dpu_id],
        );
    }
    Ok(())
}

// Unrequested hosts must stay out of both the results and snapshot enrichment.
#[crate::sqlx_test]
async fn load_by_instance_ids_preserves_selection_and_snapshot_options(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_ids = [0x79, 0x7a, 0x7b].map(|index| {
        MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [index; 32],
            MachineType::Host,
        )
    });
    let instance_ids = [1, 2, 3].map(|index| InstanceId::from(uuid::Uuid::from_u128(index)));
    let mut txn = pool.begin().await?;
    for (host_id, instance_id) in host_ids.into_iter().zip(instance_ids) {
        crate::machine::create(
            txn.as_mut(),
            None,
            &host_id,
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO instances
                (id, machine_id, tenant_org, os_ipxe_script, network_config, nvlink_config)
            VALUES ($1, $2, 'snapshot-test', '#!ipxe', '{"interfaces": []}', '{"gpu_configs": []}')"#,
        )
        .bind(instance_id)
        .bind(host_id)
        .execute(txn.as_mut())
        .await?;
    }

    let missing_instance_id = InstanceId::from(uuid::Uuid::from_u128(4));
    sqlx::query("SET LOCAL plan_cache_mode = force_generic_plan")
        .execute(txn.as_mut())
        .await?;
    let mut expected_pairs = vec![
        (host_ids[0], instance_ids[0]),
        (host_ids[1], instance_ids[1]),
    ];
    expected_pairs.sort_unstable();
    for case in [
        Case {
            scenario: "duplicate and missing instance IDs",
            input: vec![
                instance_ids[1],
                instance_ids[0],
                instance_ids[1],
                missing_instance_id,
            ],
            expect: Outcome::Yields(expected_pairs),
        },
        Case {
            scenario: "empty instance IDs",
            input: vec![],
            expect: Outcome::Yields(vec![]),
        },
    ] {
        let txn = &mut *txn;
        case.check_async(|requested_ids| async move {
            let snapshots =
                super::load_by_instance_ids(txn, &requested_ids, LoadSnapshotOptions::default())
                    .await
                    .map_err(|error| error.to_string())?;
            let mut actual_pairs = snapshots
                .into_iter()
                .map(|snapshot| {
                    assert!(snapshot.host_snapshot.history.is_empty());
                    (
                        snapshot.host_snapshot.id.to_machine_id(),
                        snapshot
                            .instance
                            .expect("instance data is included by default")
                            .id,
                    )
                })
                .collect::<Vec<_>>();
            actual_pairs.sort_unstable();
            Ok::<_, String>(actual_pairs)
        })
        .await;
    }

    // Explain the statement the loader actually executed, so this check also
    // catches a return to the outer join. This fixture has no DPUs, so a
    // singleton request needs exactly one host-snapshot enrichment.
    let statements: Vec<String> = sqlx::query_scalar(
        "SELECT statement FROM pg_prepared_statements
         WHERE parameter_types = ARRAY['uuid[]'::regtype]
           AND statement LIKE '% AS host_snapshot%'
           AND statement LIKE '% AS dpu_snapshots%'
           AND statement LIKE '% AS instance%'",
    )
    .fetch_all(txn.as_mut())
    .await?;
    assert_eq!(statements.len(), 1);
    let explain = format!("EXPLAIN (ANALYZE, FORMAT JSON) {}", statements[0]);
    let sqlx::types::Json(plan): sqlx::types::Json<serde_json::Value> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
            .bind(&instance_ids[2..])
            .fetch_one(txn.as_mut())
            .await?;

    fn host_snapshot_loops(node: &serde_json::Value, loops: &mut Vec<u64>) {
        if node["Alias"] == "host_snapshot" {
            loops.push(node["Actual Loops"].as_u64().expect("actual loop count"));
        }
        for child in node["Plans"].as_array().into_iter().flatten() {
            host_snapshot_loops(child, loops);
        }
    }
    let mut loops = Vec::new();
    host_snapshot_loops(&plan[0]["Plan"], &mut loops);
    assert_eq!(loops, vec![1], "{plan:#}");

    // Without the instance payload join, selection must still resolve the
    // instance ID independently and include the requested host's history.
    let snapshots = super::load_by_instance_ids(
        txn.as_mut(),
        &instance_ids[..1],
        LoadSnapshotOptions {
            include_history: true,
            include_instance_data: false,
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.host_snapshot.id.to_machine_id(), host_ids[0]);
    assert!(snapshot.instance.is_none());
    let history = &snapshot.host_snapshot.history;
    assert_eq!(history.len(), 1);
    assert_eq!(
        serde_json::from_str::<ManagedHostState>(&history[0].state)?,
        ManagedHostState::Ready,
    );
    Ok(())
}

// A `WithoutComponents` load must hold the same hardware as the full load, with
// only the component lists emptied, for the host and its DPU under every option
// combination. Anything else missing would change what network configuration
// reads.
#[crate::sqlx_test]
async fn without_components_omits_only_component_lists(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x81; 32],
        MachineType::Host,
    );
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x82; 32],
        MachineType::Dpu,
    );
    // The shared host fixture has no TPM description; give it one so every
    // component list has something to remove.
    let host_hardware = HardwareInfo {
        tpm_description: Some(TpmDescription {
            vendor: "fixture-vendor".to_string(),
            firmware_version: "1.0".to_string(),
            tpm_spec: "2.0".to_string(),
        }),
        ..host_hardware_info()
    };
    let mut txn = pool.begin().await?;
    for (machine_id, hardware) in [(host_id, host_hardware), (dpu_id, dpu_hardware_info(0))] {
        crate::machine::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        crate::machine_topology::create_or_update(txn.as_mut(), &machine_id, &hardware).await?;
    }
    let segment = crate::network_segment::persist(
        admin_segment("hardware-info-scope", "192.0.2.0/24", "192.0.2.1", 3),
        txn.as_mut(),
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let interface = crate::machine_interface::create(
        txn.as_mut(),
        std::slice::from_ref(&segment),
        &"02:00:00:00:00:82".parse()?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    crate::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::Machine(host_id),
        txn.as_mut(),
    )
    .await?;
    crate::machine_interface::associate_interface_with_dpu_machine(
        &interface.id,
        &dpu_id,
        txn.as_mut(),
    )
    .await?;
    // An instance on the host shows whether each query joined instance data.
    sqlx::query(
        r#"INSERT INTO instances
            (id, machine_id, tenant_org, os_ipxe_script, network_config, nvlink_config)
        VALUES ($1, $2, 'snapshot-test', '#!ipxe', '{"interfaces": []}', '{"gpu_configs": []}')"#,
    )
    .bind(InstanceId::from(uuid::Uuid::from_u128(0x81)))
    .bind(host_id)
    .execute(txn.as_mut())
    .await?;

    let without_components = |hardware: &HardwareInfo| HardwareInfo {
        cpu_info: vec![],
        block_devices: vec![],
        nvme_devices: vec![],
        gpus: vec![],
        memory_devices: vec![],
        infiniband_interfaces: vec![],
        tpm_description: None,
        ..hardware.clone()
    };

    // Each combination selects a different query string.
    for (include_instance_data, include_history) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let options = || LoadSnapshotOptions {
            include_history,
            include_instance_data,
            ..Default::default()
        };
        let full = super::load_snapshot(txn.as_mut(), &dpu_id, options())
            .await?
            .expect("full snapshot loads");
        let scoped = super::load_snapshot_with_hardware_info_scope(
            txn.as_mut(),
            &dpu_id,
            options(),
            HardwareInfoScope::WithoutComponents,
        )
        .await?
        .expect("snapshot without components loads");
        let case = format!("instance data {include_instance_data}, history {include_history}");

        assert_eq!(full.hardware_info_scope, HardwareInfoScope::Full, "{case}");
        assert_eq!(
            scoped.hardware_info_scope,
            HardwareInfoScope::WithoutComponents,
            "{case}"
        );
        assert_eq!(scoped.instance.is_some(), include_instance_data, "{case}");
        assert_eq!(
            !scoped.host_snapshot.history.is_empty(),
            include_history,
            "{case}"
        );

        let full_host = full
            .host_snapshot
            .status
            .hardware_info
            .as_ref()
            .expect("host hardware");
        assert!(
            !full_host.cpu_info.is_empty()
                && !full_host.block_devices.is_empty()
                && !full_host.nvme_devices.is_empty()
                && !full_host.gpus.is_empty()
                && !full_host.memory_devices.is_empty()
                && !full_host.infiniband_interfaces.is_empty()
                && full_host.tpm_description.is_some(),
            "{case}: the host fixture must hold every component list"
        );
        assert_eq!(
            scoped.host_snapshot.status.hardware_info,
            Some(without_components(full_host)),
            "{case}: host",
        );

        let full_dpu = full.dpu_snapshots[0]
            .status
            .hardware_info
            .as_ref()
            .expect("DPU hardware");
        assert!(
            !full_dpu.block_devices.is_empty(),
            "{case}: the DPU fixture must hold a component list"
        );
        assert_eq!(scoped.dpu_snapshots.len(), 1, "{case}");
        assert_eq!(
            scoped.dpu_snapshots[0].status.hardware_info,
            Some(without_components(full_dpu)),
            "{case}: DPU",
        );
    }
    Ok(())
}
