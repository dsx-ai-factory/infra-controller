/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use db::TransactionVending as _;
use db::nvlink_domain::{self, lifecycle};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::nvlink_domain::{
    NvLinkDomain, NvLinkDomainMember, NvLinkDomainMemberType, NvLinkDomainState,
};
use state_controller::controller::StateController;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    NvLinkDomainStateControllerIO, NvLinkDomainStateHandler, NvLinkDomainStateHandlerServices,
};

#[carbide_macros::sqlx_test]
async fn controller_releases_positions_during_discovery(pool: sqlx::PgPool) {
    sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group', 'topology', '[{\"rack_id\":\"rack-a\",\"members\":[]}]')")
        .execute(&pool).await.unwrap();
    let domain = NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: "group".parse().unwrap(),
        name: Some("domain-a".into()),
        members: vec![NvLinkDomainMember {
            rack_index: 0,
            slot_index: 5,
            component_type: NvLinkDomainMemberType::Compute,
        }],
    };
    let mut txn = pool.txn_begin().await.unwrap();
    lifecycle::create(&mut txn, &domain).await.unwrap();
    let legacy = NvLinkDomain {
        id: Uuid::new_v4(),
        members: vec![],
        ..domain.clone()
    };
    nvlink_domain::create(&mut txn, &legacy).await.unwrap();
    let empty = NvLinkDomain {
        id: Uuid::new_v4(),
        members: vec![],
        ..domain.clone()
    };
    lifecycle::create(&mut txn, &empty).await.unwrap();
    txn.commit().await.unwrap();

    let mut tasks = JoinSet::new();
    let mut controller = controller(&pool, &mut tasks).await;

    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    let initial = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        initial.controller_state.value,
        NvLinkDomainState::Discovering
    );
    assert!(matches!(
        initial.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { .. })
    ));
    assert!(
        lifecycle::find_snapshot(&pool, legacy.id)
            .await
            .unwrap()
            .is_none()
    );

    let empty_snapshot = lifecycle::find_snapshot(&pool, empty.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        empty_snapshot.controller_state.value,
        NvLinkDomainState::Created
    );
    assert!(matches!(empty_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason == "waiting for declared domain members"));

    let mut txn = pool.txn_begin().await.unwrap();
    nvlink_domain::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    controller.run_single_iteration().await;
    assert!(
        lifecycle::find_snapshot(&pool, domain.id)
            .await
            .unwrap()
            .is_none()
    );
    let replacement = NvLinkDomain {
        id: Uuid::new_v4(),
        ..domain
    };
    let mut txn = pool.txn_begin().await.unwrap();
    lifecycle::create(&mut txn, &replacement).await.unwrap();
    txn.commit().await.unwrap();
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn controller(
    pool: &sqlx::PgPool,
    tasks: &mut JoinSet<()>,
) -> StateController<NvLinkDomainStateControllerIO> {
    let lock_manager = db::work_lock_manager::start(
        tasks,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::noop(),
    )
    .await
    .unwrap();
    StateController::<NvLinkDomainStateControllerIO>::builder()
        .database(
            pool.clone(),
            lock_manager,
            &carbide_instrument::Instrumentation::noop(),
        )
        .processor_id("domain-controller-test".to_string())
        .services(Arc::new(NvLinkDomainStateHandlerServices {
            db_pool: pool.clone(),
        }))
        .state_handler(Arc::new(NvLinkDomainStateHandler))
        .build_for_manual_iterations(CancellationToken::new())
        .unwrap()
}

async fn discovery_domain(pool: &sqlx::PgPool) -> NvLinkDomain {
    sqlx::raw_sql(
        r#"INSERT INTO expected_rack_groups (rack_group_id, topology, racks)
        VALUES ('group', 'topology', '[{"rack_id":"rack-a","members":[]}]');
        INSERT INTO racks (id) VALUES ('rack-a');"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let domain = NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: "group".parse().unwrap(),
        name: None,
        members: vec![
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 1,
                component_type: NvLinkDomainMemberType::Compute,
            },
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 2,
                component_type: NvLinkDomainMemberType::Switch,
            },
        ],
    };
    let mut txn = pool.txn_begin().await.unwrap();
    lifecycle::create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();
    domain
}

#[carbide_macros::sqlx_test]
async fn discovery_resumes_and_gates_only_declared_members(pool: sqlx::PgPool) {
    let domain = discovery_domain(&pool).await;
    let mut tasks = JoinSet::new();
    let mut controller = controller(&pool, &mut tasks).await;
    controller.run_single_iteration().await;
    let discovering = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        discovering.controller_state.value,
        NvLinkDomainState::Discovering
    );

    // Rebuild the controller from persisted discovery state before inventory arrives.
    drop(controller);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let mut controller = self::controller(&pool, &mut tasks).await;
    for (name, sql, expected_wait) in [
        (
            "missing members",
            "SELECT 1",
            "waiting for domain members: missing=2/2",
        ),
        (
            "compute is still missing",
            r#"INSERT INTO switches
            (id, name, config, rack_id, slot_number, controller_state)
            VALUES ('sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60',
            'member', '{}', 'rack-a', 2, '{"state":"ready"}')"#,
            "waiting for domain members: missing=1/2",
        ),
        (
            "discovered switch is not ready",
            r#"INSERT INTO machines
            (id, dpf, rack_id, slot_number, controller_state)
            VALUES ('fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30',
            '{}', 'rack-a', 1, '{"state":"created"}');
            UPDATE switches SET controller_state='{"state":"created"}'"#,
            "waiting for domain switches ready: unready=1",
        ),
    ] {
        sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        controller.run_single_iteration().await;
        let snapshot = lifecycle::find_snapshot(&pool, domain.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.controller_state.value, discovering.controller_state.value,
            "{name}"
        );
        assert_eq!(
            snapshot.controller_state.version, discovering.controller_state.version,
            "{name}"
        );
        assert!(
            matches!(snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason == expected_wait),
            "{name}"
        );
    }
    // An unrelated, unready switch in this rack must not block this Domain.
    sqlx::raw_sql(r#"INSERT INTO switches (id, name, config, rack_id, slot_number, controller_state)
        VALUES ('sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b61', 'unrelated', '{}', 'rack-a', 99, '{"state":"created"}');
        UPDATE switches SET controller_state='{"state":"ready"}' WHERE slot_number=2;"#)
        .execute(&pool).await.unwrap();
    controller.run_single_iteration().await;
    let configured = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        configured.controller_state.value,
        NvLinkDomainState::Maintenance {
            maintenance_state: model::rack::RackMaintenanceState::FirmwareUpgrade {
                rack_firmware_upgrade: model::rack::FirmwareUpgradeState::Start,
            },
        }
    );
    assert_ne!(
        configured.controller_state.version,
        discovering.controller_state.version
    );
    assert_eq!(configured.domain.members, domain.members);
    assert_eq!(configured.config.value, discovering.config.value);
    assert_eq!(configured.config.version, discovering.config.version);
    controller.run_single_iteration().await;
    let repeated = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated.controller_state.value,
        configured.controller_state.value
    );
    assert_eq!(
        repeated.controller_state.version,
        configured.controller_state.version
    );
    assert!(matches!(repeated.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason == "waiting for domain maintenance"));

    let mut txn = pool.txn_begin().await.unwrap();
    nvlink_domain::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    controller.run_single_iteration().await;
    let deleted = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.controller_state.value, NvLinkDomainState::Deleting);
    assert_eq!(deleted.domain.members, domain.members);
    controller.run_single_iteration().await;
    let retained = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.controller_state.value, NvLinkDomainState::Deleting);
    assert_eq!(
        retained.controller_state.version,
        deleted.controller_state.version
    );
    assert_eq!(retained.domain.members, domain.members);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[carbide_macros::sqlx_test]
async fn discovery_records_resolution_errors_and_retries_after_repair(pool: sqlx::PgPool) {
    let domain = discovery_domain(&pool).await;
    let mut tasks = JoinSet::new();
    let mut controller = controller(&pool, &mut tasks).await;
    controller.run_single_iteration().await;
    let discovering = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE nvlink_domain_members SET rack_index=99 WHERE domain_id=$1")
        .bind(domain.id)
        .execute(&pool)
        .await
        .unwrap();
    controller.run_single_iteration().await;
    let failed = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        failed.controller_state.value,
        discovering.controller_state.value
    );
    assert_eq!(
        failed.controller_state.version,
        discovering.controller_state.version
    );
    assert!(matches!(failed.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. }) if err.contains("rack index 99 is outside rack group")));
    sqlx::query("UPDATE nvlink_domain_members SET rack_index=0 WHERE domain_id=$1")
        .bind(domain.id)
        .execute(&pool)
        .await
        .unwrap();
    controller.run_single_iteration().await;
    let repaired = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repaired.controller_state.value,
        discovering.controller_state.value
    );
    assert_eq!(
        repaired.controller_state.version,
        discovering.controller_state.version
    );
    assert!(matches!(repaired.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason == "waiting for domain members: missing=2/2"));
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[carbide_macros::sqlx_test]
async fn managed_domain_retries_all_positions_after_conflict(pool: sqlx::PgPool) {
    let racks = r#"[{"rack_id":"rack-a","members":[{"type":"Compute","id":"c","manufacturer":"v","cartridge_index":13},{"type":"PowerShelf","id":"p","manufacturer":"v","cartridge_index":10}]},{"rack_id":"rack-b","members":[{"type":"Compute","id":"c2","manufacturer":"v","cartridge_index":40},{"type":"Switch","id":"s","manufacturer":"v","cartridge_index":42}]}]"#;
    sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group','topology',$1::jsonb)")
        .bind(racks).execute(&pool).await.unwrap();
    let group_id = "group".parse().unwrap();
    let group = db::expected_rack_group::find_by_rack_group_id(&pool, &group_id)
        .await
        .unwrap()
        .unwrap();
    let switch = NvLinkDomainMember {
        rack_index: 1,
        slot_index: 3,
        component_type: NvLinkDomainMemberType::Switch,
    };
    let other = NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: group_id.clone(),
        name: None,
        members: vec![switch.clone()],
    };
    let mut txn = pool.txn_begin().await.unwrap();
    lifecycle::create_managed(&mut txn, &group).await.unwrap();
    let id = lifecycle::find_ids(txn.as_pgconn()).await.unwrap()[0];
    nvlink_domain::create(&mut txn, &other).await.unwrap();
    txn.commit().await.unwrap();
    let mut tasks = JoinSet::new();
    let mut controller = controller(&pool, &mut tasks).await;
    controller.run_single_iteration().await;
    let blocked = lifecycle::find_snapshot(&pool, id).await.unwrap().unwrap();
    assert_eq!(blocked.controller_state.value, NvLinkDomainState::Created);
    assert!(blocked.domain.members.is_empty());
    assert!(
        matches!(blocked.controller_state_outcome, Some(PersistentStateHandlerOutcome::Error { err, .. }) if err.contains("rack 1 Switch slot 3"))
    );
    let mut txn = pool.txn_begin().await.unwrap();
    nvlink_domain::delete(&mut txn, other.id).await.unwrap();
    txn.commit().await.unwrap();
    controller.run_single_iteration().await;
    let initialized = lifecycle::find_snapshot(&pool, id).await.unwrap().unwrap();
    assert_eq!(
        initialized.controller_state.value,
        NvLinkDomainState::Discovering
    );
    assert_eq!(
        initialized.domain.members,
        vec![
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 1,
                component_type: NvLinkDomainMemberType::Compute
            },
            NvLinkDomainMember {
                rack_index: 1,
                slot_index: 1,
                component_type: NvLinkDomainMemberType::Compute
            },
            switch
        ]
    );
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}
