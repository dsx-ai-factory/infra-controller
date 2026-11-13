/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine::{MachineIdSource, MachineType};
use carbide_uuid::rack::RackGroupId;
use carbide_uuid::switch::{SwitchIdSource, SwitchType};
use model::nvlink_domain::NvLinkDomain;

use super::*;

fn machine(seed: u8, kind: MachineType) -> MachineId {
    MachineId::new(MachineIdSource::Tpm, [seed; 32], kind)
}

fn switch(seed: u8) -> SwitchId {
    SwitchId::new(SwitchIdSource::Tpm, [seed; 32], SwitchType::NvLink)
}

async fn fixture(pool: &sqlx::PgPool) -> NvLinkDomain {
    sqlx::raw_sql(r#"INSERT INTO expected_rack_groups (rack_group_id, topology, racks)
        VALUES ('group', 'topology', '[{"rack_id":"rack-a","members":[]},{"rack_id":"rack-b","members":[]}]');
        INSERT INTO racks (id) VALUES ('rack-a'), ('rack-b');"#)
        .execute(pool).await.unwrap();
    let domain = NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: RackGroupId::new("group"),
        name: None,
        members: vec![
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 5,
                component_type: RackCapabilityType::Compute,
            },
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 5,
                component_type: RackCapabilityType::Switch,
            },
            NvLinkDomainMember {
                rack_index: 1,
                slot_index: 3,
                component_type: RackCapabilityType::Compute,
            },
        ],
    };
    let mut txn = pool.begin().await.unwrap();
    crate::nvlink_domain::create(&mut txn, &domain)
        .await
        .unwrap();
    for (seed, kind) in [
        (1, MachineType::Host),
        (2, MachineType::Dpu),
        (3, MachineType::PredictedHost),
    ] {
        let state = if kind.is_host() {
            serde_json::json!({"state":"ready"})
        } else {
            serde_json::json!({"state":"created"})
        };
        sqlx::query(
            "INSERT INTO machines (id, dpf, rack_id, slot_number, tray_index, controller_state)
            VALUES ($1, '{}', 'rack-a', 5, 4, $2)",
        )
        .bind(machine(seed, kind))
        .bind(Json(state))
        .execute(&mut *txn)
        .await
        .unwrap();
    }
    // An unrelated position must not be included by a cross-product of racks and slots.
    sqlx::query(
        "INSERT INTO machines (id, dpf, rack_id, slot_number, controller_state)
        VALUES ($1, '{}', 'rack-a', 3, '{\"state\":\"invalid\"}')",
    )
    .bind(machine(4, MachineType::Host))
    .execute(&mut *txn)
    .await
    .unwrap();
    for (seed, deleted) in [(1, false), (2, true)] {
        sqlx::query("INSERT INTO switches (id, name, config, rack_id, slot_number, tray_index, controller_state, deleted)
            VALUES ($1, 'switch', '{}', 'rack-a', 5, 4, '{\"state\":\"created\"}', CASE WHEN $2 THEN now() ELSE NULL END)")
            .bind(switch(seed)).bind(deleted).execute(&mut *txn).await.unwrap();
    }
    txn.commit().await.unwrap();
    domain
}

#[crate::sqlx_test]
async fn resolves_actual_inventory_preserving_missing_positions(pool: sqlx::PgPool) {
    let domain = fixture(&pool).await;
    let mut txn = pool.begin().await.unwrap();
    let resolved = resolve_domain_members(&mut txn, domain.id).await.unwrap();
    assert_eq!(
        resolved
            .iter()
            .map(|member| member.member.clone())
            .collect::<Vec<_>>(),
        domain.members
    );
    assert_eq!(resolved[0].rack_id.as_str(), "rack-a");
    match &resolved[0].tray {
        Some(NvLinkDomainTray::Compute {
            machine_id,
            controller_state,
        }) => {
            assert_eq!(machine_id.as_machine_id(), &machine(1, MachineType::Host));
            assert_eq!(controller_state.value, ManagedHostState::Ready);
        }
        other => panic!("expected actual compute, got {other:?}"),
    }
    match &resolved[1].tray {
        Some(NvLinkDomainTray::Switch {
            switch_id,
            controller_state,
        }) => {
            assert_eq!(*switch_id, switch(1));
            assert_eq!(controller_state.value, SwitchControllerState::Created);
        }
        other => panic!("expected actual switch, got {other:?}"),
    }
    assert_eq!(resolved[2].rack_id.as_str(), "rack-b");
    assert!(resolved[2].tray.is_none());

    let rack: RackId = "rack-a".parse().unwrap();
    for (name, kind, slot, present) in [
        ("compute SlotID", RackCapabilityType::Compute, 5, true),
        ("switch SlotID", RackCapabilityType::Switch, 5, true),
        (
            "tray index is not SlotID",
            RackCapabilityType::Compute,
            4,
            false,
        ),
        (
            "large undiscovered slot",
            RackCapabilityType::Compute,
            u32::MAX,
            false,
        ),
    ] {
        assert_eq!(
            find_tray(&mut txn, &rack, kind, slot)
                .await
                .unwrap()
                .is_some(),
            present,
            "{name}"
        );
    }
    assert!(matches!(
        find_tray(&mut txn, &rack, RackCapabilityType::PowerShelf, 5).await,
        Err(DatabaseError::InvalidArgument(_))
    ));
    txn.rollback().await.unwrap();
}

#[crate::sqlx_test]
async fn rejects_ambiguous_inventory_and_invalid_rack_indexes(pool: sqlx::PgPool) {
    let domain = fixture(&pool).await;
    let mut txn = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO machines (id, dpf, rack_id, slot_number, controller_state)
        VALUES ($1, '{}', 'rack-a', 5, '{\"state\":\"ready\"}')",
    )
    .bind(machine(5, MachineType::Host))
    .execute(&mut *txn)
    .await
    .unwrap();
    assert!(matches!(
        resolve_domain_members(&mut txn, domain.id).await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
    assert!(matches!(
        find_tray(
            &mut txn,
            &"rack-a".parse().unwrap(),
            RackCapabilityType::Compute,
            5
        )
        .await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
    txn.rollback().await.unwrap();

    let mut txn = pool.begin().await.unwrap();
    sqlx::query("UPDATE nvlink_domain_members SET rack_index=99 WHERE domain_id=$1")
        .bind(domain.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    assert!(matches!(
        resolve_domain_members(&mut txn, domain.id).await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
    assert!(matches!(
        resolve_domain_members(&mut txn, Uuid::new_v4()).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    txn.rollback().await.unwrap();
}
