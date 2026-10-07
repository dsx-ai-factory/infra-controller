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

use super::*;
use crate::inventory::{
    Guid, InfinibandPortState, InventoryLease, InventoryMachine, InventoryPort, InventorySnapshot,
};

fn fabric() -> Fabric {
    Fabric::new(FabricConfig::default())
}

fn snapshot(
    inventory_id: &str,
    generation: u64,
    machines: Vec<InventoryMachine>,
) -> InventorySnapshot {
    InventorySnapshot {
        inventory_id: inventory_id.into(),
        epoch_id: "epoch-1".into(),
        generation: generation.into(),
        machines,
    }
}

fn machine(mat_id: &str, guid: &str, state: InfinibandPortState) -> InventoryMachine {
    InventoryMachine {
        mat_id: mat_id.into(),
        machine_id: None,
        infiniband_ports: Some(vec![InventoryPort {
            guid: guid.parse().unwrap(),
            state,
        }]),
    }
}

fn lease(value: u64) -> InventoryLease {
    (1..value).fold(InventoryLease::FIRST_REMOTE, |lease, _| {
        lease.checked_next().unwrap()
    })
}

#[test]
fn live_updates_preserve_partition_membership_and_complete_snapshots_remove_ports() {
    let fabric = fabric();
    fabric
        .reconcile(
            snapshot(
                "inventory-a",
                1,
                vec![machine(
                    "mat-a",
                    "0000000000000001",
                    InfinibandPortState::Active,
                )],
            ),
            lease(1),
        )
        .unwrap();
    fabric
        .bind(BindRequest {
            pkey: "1".parse().unwrap(),
            ip_over_ib: false,
            membership: PortMembership::Full,
            index0: true,
            guids: vec!["0000000000000001".parse().unwrap()],
        })
        .unwrap();

    fabric
        .reconcile(
            snapshot(
                "inventory-a",
                2,
                vec![machine(
                    "mat-a",
                    "0000000000000001",
                    InfinibandPortState::Down,
                )],
            ),
            lease(1),
        )
        .unwrap();

    {
        let fabric = fabric.read();
        assert_eq!(fabric.ports()[0].logical_state, "Down");
        let partition = fabric
            .partition("1".parse().unwrap(), true, true)
            .unwrap()
            .unwrap();
        let members = partition.guids.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].guid, &Guid::from(1));
        assert_eq!(members[0].membership, &PortMembership::Full);
        assert!(*members[0].index0);
    }

    fabric
        .reconcile(snapshot("inventory-a", 3, Vec::new()), lease(1))
        .unwrap();

    let fabric = fabric.read();
    assert!(fabric.ports().is_empty());
    assert!(
        fabric
            .partition("1".parse().unwrap(), true, true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn lower_inventory_id_wins_guid_conflict_regardless_of_arrival_order() {
    let fabric = fabric();
    fabric
        .reconcile(
            snapshot(
                "inventory-z",
                1,
                vec![machine(
                    "mat-z",
                    "0000000000000001",
                    InfinibandPortState::Down,
                )],
            ),
            lease(1),
        )
        .unwrap();
    fabric
        .reconcile(
            snapshot(
                "inventory-a",
                1,
                vec![machine(
                    "mat-a",
                    "0000000000000001",
                    InfinibandPortState::Active,
                )],
            ),
            lease(2),
        )
        .unwrap();

    let fabric = fabric.read();
    let ports = fabric.ports();
    let port = &ports[0];
    assert_eq!(port.dname, "mat-a");
    assert_eq!(port.logical_state, "Active");
}

#[test]
fn stale_generation_does_not_replace_newer_state() {
    let fabric = fabric();
    fabric
        .reconcile(
            snapshot(
                "inventory-a",
                2,
                vec![machine(
                    "mat-a",
                    "0000000000000001",
                    InfinibandPortState::Active,
                )],
            ),
            lease(1),
        )
        .unwrap();

    let outcome = fabric
        .reconcile(snapshot("inventory-a", 1, Vec::new()), lease(1))
        .unwrap();

    assert_eq!(outcome, ReconcileOutcome::Stale);
    assert_eq!(fabric.read().ports().len(), 1);
}

#[test]
fn reconciles_four_ports_for_4500_machines() {
    let fabric = fabric();
    let machines = (0_u64..4_500)
        .map(|machine_index| InventoryMachine {
            mat_id: format!("mat-{machine_index}").into(),
            machine_id: None,
            infiniband_ports: Some(
                (0_u64..4)
                    .map(|port_index| InventoryPort {
                        guid: Guid::from(machine_index * 4 + port_index + 1),
                        state: InfinibandPortState::Active,
                    })
                    .collect(),
            ),
        })
        .collect();

    fabric
        .reconcile(snapshot("inventory-scale", 1, machines), lease(1))
        .unwrap();

    let fabric = fabric.read();
    assert_eq!(fabric.ports().len(), 18_000);
    assert_eq!(
        fabric
            .partition("0x7fff".parse().unwrap(), true, false)
            .unwrap()
            .unwrap()
            .guids
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn guid_display_is_normalized() {
    assert_eq!(
        "0x1".parse::<Guid>().unwrap().to_string(),
        "0000000000000001"
    );
    assert_eq!(
        "AABB".parse::<Guid>().unwrap().to_string(),
        "000000000000aabb"
    );
}

#[test]
fn unbind_is_idempotent_for_an_absent_guid() {
    let fabric = fabric();

    fabric.unbind(UnbindRequest {
        pkey: "1".parse().unwrap(),
        guids: vec![Guid::from(1)],
    });
}
