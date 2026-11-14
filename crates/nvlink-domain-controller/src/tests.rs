/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use db::nvlink_domain::{self, lifecycle};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::nvlink_domain::{NvLinkDomain, NvLinkDomainMember, NvLinkDomainState};
use model::rack_type::RackCapabilityType;
use state_controller::controller::StateController;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{NvLinkDomainStateControllerIO, NvLinkDomainStateHandler};

#[carbide_macros::sqlx_test]
async fn controller_persists_wait_and_deletion_without_releasing_positions(pool: sqlx::PgPool) {
    sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group', 'topology', '[]')")
        .execute(&pool).await.unwrap();
    let domain = NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: "group".parse().unwrap(),
        name: Some("domain-a".into()),
        members: vec![NvLinkDomainMember {
            rack_index: 0,
            slot_index: 5,
            component_type: RackCapabilityType::Compute,
        }],
    };
    let mut txn = pool.begin().await.unwrap();
    lifecycle::create(&mut txn, &domain).await.unwrap();
    let legacy = NvLinkDomain {
        id: Uuid::new_v4(),
        members: vec![],
        ..domain.clone()
    };
    nvlink_domain::create(&mut txn, &legacy).await.unwrap();
    txn.commit().await.unwrap();

    let mut tasks = JoinSet::new();
    let lock_manager = db::work_lock_manager::start(&mut tasks, pool.clone(), Default::default())
        .await
        .unwrap();
    let mut controller = StateController::<NvLinkDomainStateControllerIO>::builder()
        .database(pool.clone(), lock_manager)
        .processor_id("domain-controller-test".to_string())
        .services(Arc::new(pool.clone()))
        .state_handler(Arc::new(NvLinkDomainStateHandler))
        .build_for_manual_iterations(CancellationToken::new())
        .unwrap();

    controller.run_single_iteration().await;
    let initial = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.controller_state.value, NvLinkDomainState::Created);
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

    let mut txn = pool.begin().await.unwrap();
    nvlink_domain::delete(&mut txn, domain.id).await.unwrap();
    txn.commit().await.unwrap();
    controller.run_single_iteration().await;
    let deleted = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.controller_state.value, NvLinkDomainState::Deleting);
    assert!(deleted.deleted.is_some());
    assert_eq!(deleted.domain.members, domain.members);
    let mut txn = pool.begin().await.unwrap();
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::NvLinkDomain,
        &domain.id,
    )
    .await
    .unwrap();
    txn.rollback().await.unwrap();
    assert_eq!(history.len(), 2);
    assert_ne!(
        deleted.controller_state.version,
        initial.controller_state.version
    );
    let history_states: Vec<_> = history.iter().map(|record| record.state.as_str()).collect();
    assert!(history_states[0].contains("created"));
    assert!(history_states[1].contains("deleting"));

    controller.run_single_iteration().await;
    let repeated = lifecycle::find_snapshot(&pool, domain.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated.controller_state.version,
        deleted.controller_state.version
    );
    assert_eq!(repeated.domain.members, domain.members);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}
