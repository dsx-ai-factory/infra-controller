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
use model::machine::{CURRENT_STATE_MODEL_VERSION, LoadSnapshotOptions, ManagedHostState};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::network_segment::NetworkSegmentControllerState;

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
