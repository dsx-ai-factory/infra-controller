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

use super::*;

async fn seed_dpus(pool: &sqlx::PgPool) -> Result<(MachineId, MachineId), sqlx::Error> {
    let dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x81; 32],
        MachineType::Dpu,
    );
    let other_dpu_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x82; 32],
        MachineType::Dpu,
    );
    let mut txn = pool.begin().await?;
    for machine_id in [dpu_id, other_dpu_id] {
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn.as_mut())
            .await?;
    }
    txn.commit().await?;
    Ok((dpu_id, other_dpu_id))
}

#[crate::sqlx_test]
async fn prediction_preserves_ownership_and_the_callers_transaction(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (dpu_id, other_dpu_id) = seed_dpus(&pool).await?;
    let mac_address = MacAddress::new([2, 0, 0, 0, 0x81, 1]);
    let mut txn = pool.begin().await?;

    assert!(ensure_dpu_oob_interface_ownership(txn.as_mut(), &dpu_id, mac_address).await?);
    let initial =
        crate::predicted_machine_interface::find_by_mac_address(txn.as_mut(), mac_address)
            .await?
            .expect("a DPU without an OOB row needs a prediction");
    assert_eq!(initial.machine_id, dpu_id);
    assert_eq!(
        initial.expected_network_segment_type,
        NetworkSegmentType::Underlay
    );
    assert!(initial.primary_interface);
    assert!(initial.boot_interface_id.is_none());

    assert!(!ensure_dpu_oob_interface_ownership(txn.as_mut(), &dpu_id, mac_address).await?);
    let error = ensure_dpu_oob_interface_ownership(txn.as_mut(), &other_dpu_id, mac_address)
        .await
        .expect_err("another DPU must not overwrite an existing prediction");
    assert!(matches!(
        error,
        DatabaseError::AlreadyFoundError {
            kind: "PredictedMachineInterface",
            id,
        } if id == mac_address.to_string()
    ));

    let unchanged =
        crate::predicted_machine_interface::find_by_mac_address(txn.as_mut(), mac_address)
            .await?
            .expect("the conflict must leave the original prediction readable");
    assert_eq!(unchanged.id, initial.id);
    assert_eq!(unchanged.machine_id, dpu_id);
    txn.rollback().await?;

    let mut connection = pool.acquire().await?;
    assert!(
        crate::predicted_machine_interface::find_by_mac_address(&mut connection, mac_address)
            .await?
            .is_none(),
        "rolling back the caller's transaction must remove the prediction",
    );
    Ok(())
}

#[crate::sqlx_test]
async fn existing_interface_ownership_is_preserved(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (dpu_id, other_dpu_id) = seed_dpus(&pool).await?;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('dpu-oob-test', 'V1-T0', $1) RETURNING id",
    )
    .bind(NetworkSegmentType::Underlay)
    .fetch_one(&pool)
    .await?;

    for (suffix, scenario, owner, expected_owner, changed) in [
        (1, "unowned interface", None, dpu_id, true),
        (2, "same DPU", Some(dpu_id), dpu_id, false),
        (3, "another DPU", Some(other_dpu_id), other_dpu_id, false),
    ] {
        let mac_address = MacAddress::new([2, 0, 0, 0, 0x82, suffix]);
        let mut txn = pool.begin().await?;
        let interface_id = insert_machine_interface(
            txn.as_mut(),
            &segment_id,
            &mac_address,
            "dpu-oob-test".to_string(),
            None,
            true,
            InterfaceType::Data,
        )
        .await?;
        if let Some(owner) = owner {
            associate_interface_with_machine(
                &interface_id,
                MachineInterfaceAssociation::Machine(owner),
                txn.as_mut(),
            )
            .await?;
            associate_interface_with_dpu_machine(&interface_id, &owner, txn.as_mut()).await?;
        }

        assert_eq!(
            ensure_dpu_oob_interface_ownership(txn.as_mut(), &dpu_id, mac_address).await?,
            changed,
            "{scenario}",
        );
        let interfaces = find_by_mac_address(txn.as_mut(), mac_address).await?;
        let [interface] = interfaces.as_slice() else {
            panic!("{scenario}: expected one OOB interface, got {interfaces:?}");
        };
        assert_eq!(interface.id, interface_id, "{scenario}");
        assert_eq!(interface.machine_id, Some(expected_owner), "{scenario}");
        assert_eq!(
            interface.attached_dpu_machine_id.as_deref(),
            Some(&expected_owner),
            "{scenario}",
        );
        assert!(interface.primary_interface, "{scenario}");
        assert!(
            crate::predicted_machine_interface::find_by_mac_address(txn.as_mut(), mac_address)
                .await?
                .is_none(),
            "{scenario}: an existing interface must not need a prediction",
        );
        txn.rollback().await?;
        assert!(find_by_mac_address(&pool, mac_address).await?.is_empty());
    }
    Ok(())
}
