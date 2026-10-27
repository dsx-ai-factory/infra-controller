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
