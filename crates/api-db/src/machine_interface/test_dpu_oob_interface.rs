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
use crate::TransactionVending as _;

async fn seed_dpus(
    pool: &sqlx::PgPool,
) -> Result<(MachineId, MachineId), Box<dyn std::error::Error>> {
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
    let mut txn = pool.txn_begin().await?;
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
    let mut txn = pool.txn_begin().await?;

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
        crate::predicted_machine_interface::find_by_mac_address(&mut *connection, mac_address)
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
        let mut txn = pool.txn_begin().await?;
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

#[crate::sqlx_test]
async fn dpu_loopback_owner_during_first_dhcp(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::db_read::PgPoolReader;
    use crate::test_support::postgres::wait_for_blocked_query;

    let (machine_id, _) = seed_dpus(&pool).await?;
    let dpu_id = DpuMachineId::try_from(machine_id)?;
    let mac = MacAddress::new([2, 0, 0, 0, 0x83, 1]);
    let relay_ip = "192.0.2.1".parse()?;
    let mut underlay = crate::test_support::network_segment::admin_segment(
        "dpu-oob",
        "192.0.2.0/24",
        "192.0.2.1",
        1,
    );
    underlay.segment_type = NetworkSegmentType::Underlay;
    let mut setup = pool.txn_begin().await?;
    let segment = db_network_segment::persist(
        underlay,
        setup.as_mut(),
        model::network_segment::NetworkSegmentControllerState::Ready,
    )
    .await?;
    ensure_dpu_oob_interface_ownership(setup.as_mut(), &dpu_id, mac).await?;
    // Until the real row has an owner, the prediction is still authoritative.
    insert_machine_interface(
        setup.as_mut(),
        &segment.id,
        &mac,
        "oob".to_string(),
        None,
        false,
        InterfaceType::Data,
    )
    .await?;
    assert!(dpu_owns_oob_mac(setup.as_mut(), &dpu_id, mac).await?);
    let prediction = crate::predicted_machine_interface::find_by_mac_address(setup.as_mut(), mac)
        .await?
        .expect("Site Explorer recorded the OS/admin MAC before DHCP");
    setup.commit().await?;

    let mut dhcp = pool.txn_begin().await?;
    lock_network_segments_exclusive(dhcp.as_mut(), &[segment.id]).await?;
    sqlx::query("LOCK TABLE predicted_machine_interfaces IN ACCESS EXCLUSIVE MODE")
        .execute(dhcp.as_mut())
        .await?;
    let dhcp_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(dhcp.as_mut())
        .await?;

    // With two queries, the first could see no real owner and the second
    // would wait here until DHCP had already deleted the prediction.
    let ownership = dpu_owns_oob_mac(&pool, &dpu_id, mac);
    tokio::pin!(ownership);
    let mut observer = PgPoolReader::from(pool.clone());
    tokio::select! {
        result = &mut ownership => panic!("ownership read did not wait for promotion: {result:?}"),
        _ = wait_for_blocked_query(&mut observer, dhcp_pid, "predicted_machine_interfaces") => {}
    }
    move_predicted_machine_interface_to_machine(
        dhcp.as_mut(),
        &prediction,
        relay_ip,
        None,
        &[segment.id],
    )
    .await?;
    dhcp.commit().await?;
    assert!(
        ownership.await?,
        "the same DPU owns the MAC across promotion"
    );
    Ok(())
}

#[crate::sqlx_test]
async fn dpu_loopback_owner_requires_an_uncontested_data_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use MachineInterfaceAssociation::{Machine, PowerShelf};

    let (machine_id, other_machine_id) = seed_dpus(&pool).await?;
    let dpu_id = DpuMachineId::try_from(machine_id)?;
    let mut setup = pool.txn_begin().await?;
    let segments: Vec<NetworkSegmentId> = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('first', 'V1-T0', 'underlay'), ('second', 'V1-T0', 'underlay') RETURNING id",
    )
    .fetch_all(setup.as_mut())
    .await?;
    let shelf = crate::test_support::power_shelf::create_seeded(setup.as_mut(), 1, "shelf").await?;
    setup.commit().await?;

    let mac = MacAddress::new([2, 0, 0, 0, 0x84, 1]);
    for (case, interface_type, owner, add_dpu_data_row, expected) in [
        (
            "DPU BMC only",
            InterfaceType::Bmc,
            Machine(machine_id),
            false,
            false,
        ),
        (
            "same DPU data and BMC",
            InterfaceType::Bmc,
            Machine(machine_id),
            true,
            true,
        ),
        (
            "two machine owners",
            InterfaceType::Data,
            Machine(other_machine_id),
            true,
            false,
        ),
        (
            "shelf overrides prediction",
            InterfaceType::Data,
            PowerShelf(shelf.id),
            false,
            false,
        ),
    ] {
        let mut txn = pool.txn_begin().await?;
        ensure_dpu_oob_interface_ownership(txn.as_mut(), &dpu_id, mac).await?;
        let mut rows = vec![(interface_type, owner)];
        if add_dpu_data_row {
            rows.push((InterfaceType::Data, Machine(machine_id)));
        }
        for ((interface_type, owner), segment_id) in rows.into_iter().zip(&segments) {
            let interface_id = insert_machine_interface(
                txn.as_mut(),
                segment_id,
                &mac,
                "test".to_string(),
                None,
                false,
                interface_type,
            )
            .await?;
            associate_interface_with_machine(&interface_id, owner, txn.as_mut()).await?;
        }
        assert_eq!(
            dpu_owns_oob_mac(txn.as_mut(), &dpu_id, mac).await?,
            expected,
            "{case}"
        );
        txn.rollback().await?;
    }
    Ok(())
}
