// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkSegmentId;
use db::ObjectColumnFilter;
use model::network_segment::NetworkSegmentSearchConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{NetworkPrefix, NetworkSegmentCreationRequest, NetworkSegmentType};
use rpc::protos::dns::{CreateDomainRequest, DomainDeletionRequest};
use sqlx::PgPool;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides,
};
use crate::tests::common::postgres::wait_for_blocked_query;

async fn create_domain(env: &TestEnv, name: &str) -> DomainId {
    env.api
        .create_domain(Request::new(CreateDomainRequest {
            name: name.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .id
        .map(DomainId::try_from)
        .unwrap()
        .unwrap()
}

fn network_segment_request(
    id: NetworkSegmentId,
    name: &str,
    prefix: &str,
    gateway: &str,
    domain_id: Option<DomainId>,
) -> NetworkSegmentCreationRequest {
    NetworkSegmentCreationRequest {
        id: Some(id),
        mtu: Some(1500),
        name: name.to_string(),
        prefixes: vec![NetworkPrefix {
            id: None,
            prefix: prefix.to_string(),
            gateway: Some(gateway.to_string()),
            reserve_first: 1,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }],
        subdomain_id: domain_id,
        vpc_id: None,
        segment_type: NetworkSegmentType::Admin as i32,
        infer_slaac_eui64_addresses: false,
    }
}

async fn assert_domain_is_live(env: &TestEnv, domain_id: DomainId) {
    let domain = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert!(domain.deleted.is_none());
}

#[crate::sqlx_test]
async fn test_domain_delete_rejects_live_network_segment_reference(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "segment-reference.example").await;
    let segment_id = NetworkSegmentId::new();
    env.api
        .create_network_segment(Request::new(network_segment_request(
            segment_id,
            "domain-reference-segment",
            "192.0.2.0/24",
            "192.0.2.1",
            Some(domain_id),
        )))
        .await
        .unwrap();

    let error = env
        .api
        .delete_domain(Request::new(DomainDeletionRequest {
            id: Some(domain_id),
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_domain_is_live(&env, domain_id).await;
}

#[crate::sqlx_test]
async fn test_domain_delete_rejects_live_machine_interface_reference(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "interface-reference.example").await;
    let segment_id = NetworkSegmentId::new();
    env.api
        .create_network_segment(Request::new(network_segment_request(
            segment_id,
            "interface-reference-segment",
            "198.51.100.0/24",
            "198.51.100.1",
            None,
        )))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO machine_interfaces
         (segment_id, mac_address, hostname, domain_id, primary_interface)
         VALUES ($1, '02:00:00:00:00:01', 'referencing-host', $2, false)",
    )
    .bind(segment_id)
    .bind(domain_id)
    .execute(&env.pool)
    .await
    .unwrap();

    let error = env
        .api
        .delete_domain(Request::new(DomainDeletionRequest {
            id: Some(domain_id),
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_domain_is_live(&env, domain_id).await;
}

#[crate::sqlx_test]
async fn test_unreferenced_domain_delete_is_idempotent(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "unreferenced.example").await;
    let request = DomainDeletionRequest {
        id: Some(domain_id),
    };

    env.api
        .delete_domain(Request::new(request.clone()))
        .await
        .unwrap();
    let first_delete = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert!(first_delete.deleted.is_some());

    env.api.delete_domain(Request::new(request)).await.unwrap();
    let second_delete = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_delete.updated, first_delete.updated);
    assert_eq!(second_delete.deleted, first_delete.deleted);
}

#[crate::sqlx_test]
async fn test_network_segment_creation_rechecks_domain_after_concurrent_delete(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_name = "2.0.192.in-addr.arpa".to_string();
    let domain_id = create_domain(&env, &domain_name).await;
    let segment_id = NetworkSegmentId::new();

    let mut reverse_zone_blocker = env.api.txn_begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut reverse_zone_blocker)
        .await
        .unwrap();
    db::dns::lock_reverse_zone_names(
        &mut reverse_zone_blocker,
        std::slice::from_ref(&domain_name),
    )
    .await
    .unwrap();

    let delete_api = env.api.clone();
    let deletion_task = tokio::spawn(async move {
        delete_api
            .delete_domain(Request::new(DomainDeletionRequest {
                id: Some(domain_id),
            }))
            .await
    });
    let deletion_pid = wait_for_blocked_query(&env.pool, blocker_pid, "dns:reverse-zone:").await;

    let create_api = env.api.clone();
    let creation_task = tokio::spawn(async move {
        create_api
            .create_network_segment(Request::new(network_segment_request(
                segment_id,
                "concurrent-domain-segment",
                "203.0.113.0/24",
                "203.0.113.1",
                Some(domain_id),
            )))
            .await
    });
    wait_for_blocked_query(&env.pool, deletion_pid, "pg_advisory_xact_lock_shared").await;
    reverse_zone_blocker.commit().await.unwrap();

    deletion_task.await.unwrap().unwrap();
    let error = creation_task.await.unwrap().unwrap_err();
    assert_eq!(error.code(), Code::NotFound);
    assert_eq!(error.message(), format!("domain not found: {domain_id}"));

    let domain = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert!(domain.deleted.is_some());

    let mut txn = env.api.txn_begin().await.unwrap();
    assert!(
        db::network_segment::find_by(
            &mut txn,
            ObjectColumnFilter::One(db::network_segment::IdColumn, &segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .is_empty()
    );
    txn.rollback().await.unwrap();
}
