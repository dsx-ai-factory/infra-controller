/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use model::rack_type::RackCapabilityType;

use super::*;

async fn declaration(pool: &sqlx::PgPool) -> NvLinkDomain {
    sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group', 'topology', '[]')")
        .execute(pool).await.unwrap();
    NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: RackGroupId::new("group"),
        name: Some("domain-a".into()),
        members: vec![NvLinkDomainMember {
            rack_index: 0,
            slot_index: 5,
            component_type: RackCapabilityType::Compute,
        }],
    }
}

#[crate::sqlx_test]
async fn migration_preserves_unadopted_declarations_and_predecessor_writer(pool: sqlx::PgPool) {
    let domain = declaration(&pool).await;
    sqlx::raw_sql(
        "ALTER TABLE nvlink_domains DROP COLUMN controller_state,
        DROP COLUMN controller_state_version, DROP COLUMN controller_state_outcome,
        DROP COLUMN config, DROP COLUMN version, DROP COLUMN deleted;
        DROP TABLE nvlink_domain_controller_iteration_ids;
        DROP TABLE nvlink_domain_controller_queued_objects;
        DROP TABLE nvlink_domain_state_history;
        DROP FUNCTION nvlink_domain_state_history_keep_limit();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut txn = pool.begin().await.unwrap();
    super::super::create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();

    sqlx::raw_sql(include_str!(
        "../../../migrations/20261113203000_nvlink_domain_controller.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        super::super::find_by_ids(&pool, &[domain.id])
            .await
            .unwrap(),
        vec![domain.clone()]
    );
    assert!(find_ids(&pool).await.unwrap().is_empty());
    assert!(find_snapshot(&pool, domain.id).await.unwrap().is_none());

    let fresh = NvLinkDomain {
        id: Uuid::new_v4(),
        members: vec![],
        ..domain.clone()
    };
    let mut txn = pool.begin().await.unwrap();
    super::super::create(&mut txn, &fresh).await.unwrap();
    super::super::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        super::super::find_ids(&pool, None).await.unwrap(),
        vec![fresh.id]
    );
    assert!(find_ids(&pool).await.unwrap().is_empty());
}

#[crate::sqlx_test]
async fn versioned_state_and_deletion_preserve_member_ownership(pool: sqlx::PgPool) {
    let domain = declaration(&pool).await;
    let mut txn = pool.begin().await.unwrap();
    create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();
    let initial = find_snapshot(&pool, domain.id).await.unwrap().unwrap();
    assert_eq!(initial.domain, domain);
    assert_eq!(initial.controller_state.value, NvLinkDomainState::Created);
    assert_eq!(initial.config.value, NvLinkDomainConfig::default());
    assert_eq!(find_ids(&pool).await.unwrap(), vec![domain.id]);

    let mut txn = pool.begin().await.unwrap();
    let history = crate::state_history::for_object(
        &mut txn,
        crate::state_history::StateHistoryTableId::NvLinkDomain,
        &domain.id,
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        serde_json::from_str::<NvLinkDomainState>(&history[0].state).unwrap(),
        NvLinkDomainState::Created
    );
    let next_version = initial.controller_state.version.increment();
    assert_eq!(
        try_update_controller_state(
            &mut txn,
            domain.id,
            initial.controller_state.version,
            next_version,
            &NvLinkDomainState::Discovering
        )
        .await
        .unwrap(),
        ConditionalWrite::Applied(())
    );
    assert_eq!(
        try_update_controller_state(
            &mut txn,
            domain.id,
            initial.controller_state.version,
            next_version,
            &NvLinkDomainState::Ready
        )
        .await
        .unwrap(),
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    );
    txn.commit().await.unwrap();
    let discovering = find_snapshot(&pool, domain.id).await.unwrap().unwrap();

    let mut txn = pool.begin().await.unwrap();
    super::super::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    let deleting = find_snapshot(&pool, domain.id).await.unwrap().unwrap();
    assert!(deleting.deleted.is_some());
    assert_ne!(deleting.config.version, discovering.config.version);
    assert_eq!(
        deleting.controller_state.version,
        discovering.controller_state.version
    );
    let mut txn = pool.begin().await.unwrap();
    assert_eq!(
        lock_snapshot(&mut txn, &discovering).await.unwrap(),
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    );
    assert_eq!(
        try_update_controller_state(
            &mut txn,
            domain.id,
            discovering.controller_state.version,
            discovering.controller_state.version.increment(),
            &NvLinkDomainState::Ready
        )
        .await
        .unwrap(),
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    );
    super::super::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    let repeated = find_snapshot(&pool, domain.id).await.unwrap().unwrap();
    assert_eq!(repeated.deleted, deleting.deleted);
    assert_eq!(repeated.config.version, deleting.config.version);

    let conflict = NvLinkDomain {
        id: Uuid::new_v4(),
        ..domain.clone()
    };
    let mut txn = pool.begin().await.unwrap();
    assert!(matches!(
        create(&mut txn, &conflict).await,
        Err(DatabaseError::AlreadyFoundError { .. })
    ));
    txn.rollback().await.unwrap();
    assert_eq!(
        super::super::find_by_ids(&pool, &[domain.id])
            .await
            .unwrap(),
        vec![domain]
    );
}

#[crate::sqlx_test]
async fn rejected_creation_rolls_back_state_and_history(pool: sqlx::PgPool) {
    let mut domain = declaration(&pool).await;
    domain.members.push(domain.members[0].clone());
    let mut txn = pool.begin().await.unwrap();
    assert!(create(&mut txn, &domain).await.is_err());
    txn.rollback().await.unwrap();
    assert!(
        super::super::find_ids(&pool, None)
            .await
            .unwrap()
            .is_empty()
    );
    let history_count: i64 = sqlx::query_scalar("SELECT count(*) FROM nvlink_domain_state_history")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(history_count, 0);
}
