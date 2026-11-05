/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::rack::RackId;
use model::nvlink_domain::NvLinkDomainMember;

use super::*;

const COMPUTE_ID: &str = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30";
const SWITCH_ID: &str = "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60";
const OTHER_SWITCH_ID: &str = "sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

#[crate::sqlx_test]
async fn additive_migration_preserves_legacy_declarations(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    sqlx::raw_sql("DROP TABLE nvlink_domain_members; DROP TABLE nvlink_domains")
        .execute(&pool)
        .await
        .unwrap();
    let declaration = model::expected_nvlink_domain::ExpectedNvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: config.rack_group_id,
        name: Some("existing-domain".into()),
        member_bmc_mac_addresses: vec![
            "00:11:22:33:44:55".parse().unwrap(),
            "00:11:22:33:44:66".parse().unwrap(),
        ],
    };
    let mut txn = pool.begin().await.unwrap();
    crate::expected_nvlink_domain::create(&mut txn, &declaration)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let mut txn = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261105175433_nvlink_domains.sql"
    ))
    .execute(&mut *txn)
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        crate::expected_nvlink_domain::find_by_ids(&pool, &[declaration.id])
            .await
            .unwrap(),
        vec![declaration],
    );
    assert!(find_ids(&pool, None).await.unwrap().is_empty());
}

#[crate::sqlx_test]
async fn resolution_preserves_missing_and_rejects_ambiguous_members(pool: sqlx::PgPool) {
    use model::nvlink_domain::NvLinkDomainTray;

    let config = inventory(&pool).await;
    let id = insert(&pool, &config).await;
    let missing = resolve_members(&pool, id).await.unwrap();
    assert_eq!(missing.len(), 2);
    assert!(missing.iter().all(|member| member.tray.is_none()));
    sqlx::query(
        "INSERT INTO switches (id, name, config, bmc_mac_address, rack_id, tray_index)
        VALUES ($1, 'switch', '{}', '00:11:22:33:44:66', 'rack', 0)",
    )
    .bind(SWITCH_ID)
    .execute(&pool)
    .await
    .unwrap();
    let partial = resolve_members(&pool, id).await.unwrap();
    assert_eq!(partial.len(), 2);
    assert_eq!(partial[0].tray, None);
    assert_eq!(
        partial[1].tray,
        Some(NvLinkDomainTray::Switch(SWITCH_ID.parse().unwrap()))
    );

    sqlx::query(
        "INSERT INTO machines (id, dpf, rack_id, tray_index)
        VALUES ($1, '{\"enabled\":true,\"used_for_ingestion\":false}', 'rack', 0)",
    )
    .bind(COMPUTE_ID)
    .execute(&pool)
    .await
    .unwrap();
    let complete = resolve_members(&pool, id).await.unwrap();
    assert_eq!(
        complete[0].tray,
        Some(NvLinkDomainTray::Compute(COMPUTE_ID.parse().unwrap()))
    );
    assert_eq!(complete[1].tray, partial[1].tray);

    sqlx::query(
        "INSERT INTO switches (id, name, config, bmc_mac_address, rack_id, tray_index)
        VALUES ($1, 'replacement', '{}', '00:11:22:33:44:77', 'rack', 0)",
    )
    .bind(OTHER_SWITCH_ID)
    .execute(&pool)
    .await
    .unwrap();
    let ambiguous = resolve_members(&pool, id).await;
    assert!(
        matches!(ambiguous, Err(DatabaseError::FailedPrecondition(_))),
        "{ambiguous:?}"
    );
    sqlx::query("UPDATE switches SET deleted = now() WHERE id = $1")
        .bind(OTHER_SWITCH_ID)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(resolve_members(&pool, id).await.unwrap(), complete);
    sqlx::query("UPDATE switches SET deleted = CASE WHEN id = $1 THEN now() ELSE NULL END WHERE id = ANY($2)")
        .bind(SWITCH_ID)
        .bind(vec![SWITCH_ID, OTHER_SWITCH_ID])
        .execute(&pool)
        .await
        .unwrap();
    let replacement = resolve_members(&pool, id).await.unwrap();
    assert_eq!(replacement[0], complete[0]);
    assert_eq!(replacement[1].member, complete[1].member);
    assert_eq!(
        replacement[1].tray,
        Some(NvLinkDomainTray::Switch(OTHER_SWITCH_ID.parse().unwrap()))
    );
}

async fn inventory(pool: &sqlx::PgPool) -> NvLinkDomainConfig {
    sqlx::raw_sql(
        "INSERT INTO expected_rack_groups (rack_group_id, topology, racks)
        VALUES ('group', 'topology', '[{\"rack_id\":\"rack\",\"members\":[]}]');
        INSERT INTO expected_machines (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id)
        VALUES ('compute', '00:11:22:33:44:55', 'test', 'test', 'rack');
        INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id)
        VALUES ('switch', '00:11:22:33:44:66', 'test', 'test', 'rack');",
    )
    .execute(pool)
    .await
    .unwrap();
    NvLinkDomainConfig {
        rack_group_id: RackGroupId::new("group"),
        domain_profile_id: "test-fabric".into(),
        members: vec![
            NvLinkDomainMember {
                component_type: NvLinkDomainComponentType::Compute,
                rack_id: RackId::new("rack"),
                tray_index: 0,
            },
            NvLinkDomainMember {
                component_type: NvLinkDomainComponentType::Switch,
                rack_id: RackId::new("rack"),
                tray_index: 0,
            },
        ],
    }
}

async fn insert(pool: &sqlx::PgPool, config: &NvLinkDomainConfig) -> Uuid {
    let id = Uuid::new_v4();
    let mut txn = pool.begin().await.unwrap();
    create(&mut txn, id, config, &Metadata::default(), false)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    id
}

#[crate::sqlx_test]
async fn deletion_retains_members_and_fences_old_controller(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    let id = insert(&pool, &config).await;
    let original = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    assert_eq!(original.config, config);
    assert_eq!(*original.controller_state, NvLinkDomainState::Discovering);
    assert_eq!(original.nmxc_domain_id, None);

    let mut txn = pool.begin().await.unwrap();
    request_deletion(&mut txn, id).await.unwrap();
    txn.commit().await.unwrap();
    let deleting = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    assert!(deleting.deleted.is_some());
    assert_ne!(deleting.version, original.version);
    assert_eq!(deleting.config.members, config.members);

    let mut txn = pool.begin().await.unwrap();
    request_deletion(&mut txn, id).await.unwrap();
    assert_eq!(
        try_update_controller_state(
            &mut txn,
            &original,
            &NvLinkDomainState::Ready,
            &PersistentStateHandlerOutcome::Transition { source_ref: None },
        )
        .await
        .unwrap(),
        ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    );
    txn.commit().await.unwrap();
    let retried = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    assert_eq!(retried.deleted, deleting.deleted);
    assert_eq!(retried.version, deleting.version);
    assert_eq!(*retried.controller_state, NvLinkDomainState::Discovering);

    let mut txn = pool.begin().await.unwrap();
    assert!(matches!(
        create(
            &mut txn,
            Uuid::new_v4(),
            &config,
            &Metadata::default(),
            false
        )
        .await,
        Err(DatabaseError::AlreadyFoundError { .. }),
    ));
    txn.rollback().await.unwrap();
    assert_eq!(find_ids(&pool, None).await.unwrap(), vec![id]);
}

#[crate::sqlx_test]
async fn controller_state_compare_and_swap(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    let id = insert(&pool, &config).await;
    let original = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    let outcome = PersistentStateHandlerOutcome::Transition { source_ref: None };
    let mut txn = pool.begin().await.unwrap();
    let updated =
        try_update_controller_state(&mut txn, &original, &NvLinkDomainState::Ready, &outcome)
            .await
            .unwrap();
    assert!(matches!(updated, ConditionalWrite::Applied(_)));
    assert_eq!(
        try_update_controller_state(
            &mut txn,
            &original,
            &NvLinkDomainState::Discovering,
            &outcome
        )
        .await
        .unwrap(),
        ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    );
    txn.commit().await.unwrap();
    let persisted = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    assert_eq!(*persisted.controller_state, NvLinkDomainState::Ready);
    assert_ne!(
        persisted.controller_state.version,
        original.controller_state.version
    );
    assert_eq!(persisted.version, original.version);
    assert_eq!(persisted.controller_state_outcome, Some(outcome));
}

#[crate::sqlx_test]
async fn invalid_membership_does_not_leave_partial_domains(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    type Mutation = fn(&mut NvLinkDomainConfig);
    let cases: &[(&str, Mutation)] = &[
        ("duplicate", |config| {
            config.members.push(config.members[0].clone())
        }),
        ("missing switch", |config| {
            config.members.pop();
        }),
        ("negative tray index", |config| {
            config.members[0].tray_index = -1
        }),
        ("rack outside group", |config| {
            config.members[0].rack_id = RackId::new("other")
        }),
        ("empty profile", |config| config.domain_profile_id.clear()),
        ("unknown group", |config| {
            config.rack_group_id = RackGroupId::new("unknown")
        }),
    ];
    for (label, mutate) in cases {
        let mut config = config.clone();
        mutate(&mut config);
        let mut txn = pool.begin().await.unwrap();
        assert!(
            create(
                &mut txn,
                Uuid::new_v4(),
                &config,
                &Metadata::default(),
                false
            )
            .await
            .is_err(),
            "{label}"
        );
        txn.rollback().await.unwrap();
        assert!(find_ids(&pool, None).await.unwrap().is_empty(), "{label}");
    }
}

#[crate::sqlx_test]
async fn positions_can_be_declared_before_hardware_is_known(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    sqlx::raw_sql("DELETE FROM expected_machines; DELETE FROM expected_switches")
        .execute(&pool)
        .await
        .unwrap();
    let id = insert(&pool, &config).await;
    let domain = find_by_ids(&pool, &[id]).await.unwrap().remove(0);
    assert_eq!(domain.config.members, config.members);
    assert_eq!(*domain.controller_state, NvLinkDomainState::Discovering);
    assert!(
        resolve_members(&pool, id)
            .await
            .unwrap()
            .iter()
            .all(|member| member.tray.is_none())
    );
    sqlx::query("UPDATE expected_rack_groups SET racks = '[]' WHERE rack_group_id = $1")
        .bind(&config.rack_group_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        resolve_members(&pool, id).await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
}

#[crate::sqlx_test]
async fn concurrent_member_claims_have_one_owner(pool: sqlx::PgPool) {
    let config = inventory(&pool).await;
    let first_id = Uuid::new_v4();
    let mut first = pool.begin().await.unwrap();
    create(&mut first, first_id, &config, &Metadata::default(), false)
        .await
        .unwrap();
    let mut txn = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *txn)
        .await
        .unwrap();
    let second = tokio::spawn(async move {
        let result = create(
            &mut txn,
            Uuid::new_v4(),
            &config,
            &Metadata::default(),
            false,
        )
        .await;
        txn.rollback().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
            )
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            assert!(
                !second.is_finished(),
                "member claim did not wait for its competing writer"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    first.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(DatabaseError::AlreadyFoundError { .. })
    ));
    assert_eq!(find_ids(&pool, None).await.unwrap(), vec![first_id]);
}
