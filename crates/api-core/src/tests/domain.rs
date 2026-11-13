// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use db::ObjectColumnFilter;
use model::network_segment::NetworkSegmentSearchConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{NetworkPrefix, NetworkSegmentCreationRequest, NetworkSegmentType};
use rpc::protos::dns::{CreateDomainRequest, DomainDeletionRequest, UpdateDomainRequest};
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
            default_ttl: None,
            reserved_id: None,
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
            dhcpv6_link_address: None,
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
async fn test_domain_delete_tracks_network_segment_lifecycle(pool: PgPool) {
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
            cancel_reserved_id: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_domain_is_live(&env, domain_id).await;

    env.api
        .delete_network_segment(Request::new(rpc::forge::NetworkSegmentDeletionRequest {
            id: Some(segment_id),
        }))
        .await
        .unwrap();

    // The reference remains stored, so success must depend on its liveness.
    let deleted_reference: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM network_segments
             WHERE id = $1 AND subdomain_id = $2 AND deleted IS NOT NULL
         )",
    )
    .bind(segment_id)
    .bind(domain_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(deleted_reference);

    env.api
        .delete_domain(Request::new(DomainDeletionRequest {
            id: Some(domain_id),
            cancel_reserved_id: false,
        }))
        .await
        .expect("a soft-deleted Network Segment must not block Domain deletion");
    let domain = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert!(domain.deleted.is_some());
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
            cancel_reserved_id: false,
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
        cancel_reserved_id: false,
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
async fn test_domain_update_rejects_deleted_domain(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "deleted-update.example").await;
    env.api
        .delete_domain(Request::new(DomainDeletionRequest {
            id: Some(domain_id),
            cancel_reserved_id: false,
        }))
        .await
        .unwrap();
    let deleted = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert!(deleted.deleted.is_some());

    let error = env
        .api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                id: Some(domain_id),
                default_ttl: Some(600),
                ..Default::default()
            }),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::NotFound);

    let unchanged = db::dns::domain::find_by_uuid(&env.pool, domain_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.default_ttl, deleted.default_ttl);
    assert_eq!(unchanged.updated, deleted.updated);
    assert_eq!(unchanged.deleted, deleted.deleted);
}

#[crate::sqlx_test]
async fn test_network_segment_creation_rechecks_domain_after_concurrent_delete(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_name = "2.0.192.in-addr.arpa".to_string();
    // The Domain API rejects reverse zones, so the reverse-zone lock used to
    // hold deletion open needs a compatibility row as network lifecycle (or
    // a pre-existing deployment) persists it, not an API-created domain.
    let rejected = env
        .api
        .create_domain(Request::new(CreateDomainRequest {
            name: domain_name.clone(),
            default_ttl: None,
            reserved_id: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(rejected.code(), Code::InvalidArgument);
    let mut fixture_txn = env.pool.begin().await.unwrap();
    let domain_id = db::dns::domain::persist(
        model::dns::NewDomain::new(domain_name.clone()),
        fixture_txn.as_mut(),
    )
    .await
    .unwrap()
    .id;
    fixture_txn.commit().await.unwrap();
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
                cancel_reserved_id: false,
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

async fn create_addressed_interface(
    env: &TestEnv,
    domain_id: DomainId,
) -> (MachineInterfaceId, NetworkSegmentId) {
    let segment_id = NetworkSegmentId::new();
    let mut request = network_segment_request(
        segment_id,
        "domain-address-segment",
        "192.0.2.0/24",
        "192.0.2.1",
        Some(domain_id),
    );
    request.prefixes.push(NetworkPrefix {
        id: None,
        prefix: "2001:db8::/64".to_string(),
        gateway: None,
        reserve_first: 1,
        free_ip_count: 0,
        svi_ip: None,
        free_ip_count_v2: None,
        free_ip_count_saturated: false,
        dhcpv6_link_address: None,
    });
    env.api
        .create_network_segment(Request::new(request))
        .await
        .unwrap();
    let interface_id = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
         (segment_id, mac_address, hostname, domain_id, primary_interface)
         VALUES ($1, '02:00:00:00:56:60', 'addressed-host', $2, true)
         RETURNING id",
    )
    .bind(segment_id)
    .bind(domain_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    let mut txn = env.api.txn_begin().await.unwrap();
    for address in ["192.0.2.10", "2001:db8::10"] {
        db::machine_interface_address::assign_static(
            &mut txn,
            interface_id,
            address.parse().unwrap(),
        )
        .await
        .unwrap();
    }
    db::machine_interface::sync_hostname_after_address_assignment(
        &mut txn,
        interface_id,
        Some(domain_id),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    (interface_id, segment_id)
}

#[crate::sqlx_test]
async fn test_static_assignment_and_domain_update_do_not_deadlock(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "address-update.example").await;
    let (interface_id, _) = create_addressed_interface(&env, domain_id).await;

    // AssignStaticAddress holds the Domain row after its serial bump, before
    // synchronizing the interface's hostname and existing Domain reference.
    let mut assignment = env.api.txn_begin().await.unwrap();
    let assignment_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut assignment)
        .await
        .unwrap();
    db::machine_interface_address::assign_static(
        &mut assignment,
        interface_id,
        "192.0.2.11".parse().unwrap(),
    )
    .await
    .unwrap();
    let update_api = env.api.clone();
    let update = tokio::spawn(async move {
        update_api
            .update_domain(Request::new(UpdateDomainRequest {
                domain: Some(rpc::protos::dns::Domain {
                    id: Some(domain_id),
                    default_ttl: Some(600),
                    ..Default::default()
                }),
            }))
            .await
    });
    wait_for_blocked_query(&env.pool, assignment_pid, "UPDATE domains").await;

    let synchronized = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        db::machine_interface::sync_hostname_after_address_assignment(
            &mut assignment,
            interface_id,
            Some(domain_id),
        ),
    )
    .await;
    if synchronized.as_ref().is_ok_and(|result| result.is_ok()) {
        assignment.commit().await.unwrap();
    } else {
        assignment.rollback().await.unwrap();
    }
    if synchronized.is_err() {
        update.abort();
    }
    let updated = update.await;
    assert!(matches!(synchronized, Ok(Ok(()))), "{synchronized:?}");
    let error = updated.unwrap().unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains("expected version"), "{error}");

    let snapshot = db::machine_interface::find_one(&env.pool, interface_id)
        .await
        .unwrap();
    assert!(snapshot.addresses.contains(&"192.0.2.11".parse().unwrap()));
    assert!(!snapshot.addresses.contains(&"192.0.2.10".parse().unwrap()));
    assert_eq!(snapshot.domain_id, Some(domain_id));
    assert_domain_is_live(&env, domain_id).await;
    assert_eq!(
        db::dns::domain::find_by_uuid(&env.pool, domain_id)
            .await
            .unwrap()
            .unwrap()
            .default_ttl,
        None,
        "the stale TTL update must not overwrite the serial bump"
    );
}

#[crate::sqlx_test]
async fn test_static_address_cleanup_preserves_existing_deleted_domain_reference(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "legacy-reference.example").await;
    let (interface_id, segment_id) = create_addressed_interface(&env, domain_id).await;

    // Reproduce data the old Domain API could commit: its deletion helper
    // soft-deleted the Domain without rejecting existing interface references.
    let mut legacy_delete = env.api.txn_begin().await.unwrap();
    let domain = db::dns::domain::find_by_uuid(&mut legacy_delete, domain_id)
        .await
        .unwrap()
        .unwrap();
    let deleted = db::dns::domain::delete(domain, &mut legacy_delete)
        .await
        .unwrap();
    legacy_delete.commit().await.unwrap();

    for (address, remaining, expected_domain) in [
        ("192.0.2.10", vec!["2001:db8::10"], Some(domain_id)),
        ("2001:db8::10", vec![], None),
    ] {
        let result = env
            .api
            .remove_static_address(Request::new(rpc::forge::RemoveStaticAddressRequest {
                interface_id: Some(interface_id),
                ip_address: address.to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            result.status,
            rpc::forge::RemoveStaticAddressStatus::Removed as i32
        );
        let snapshot = db::machine_interface::find_one(&env.pool, interface_id)
            .await
            .unwrap();
        let expected_addresses: Vec<std::net::IpAddr> = remaining
            .into_iter()
            .map(|address| address.parse().unwrap())
            .collect();
        assert_eq!(snapshot.addresses, expected_addresses);
        assert_eq!(snapshot.domain_id, expected_domain);
        if expected_domain.is_some() {
            let mut retained = env.api.txn_begin().await.unwrap();
            db::machine_interface::update_segment_id(
                &mut retained,
                interface_id,
                segment_id,
                expected_domain,
            )
            .await
            .expect("retaining the existing segment/Domain is also permitted");
            retained.commit().await.unwrap();
        }
        let current = db::dns::domain::find_by_uuid(&env.pool, domain_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.deleted, deleted.deleted);
        assert_eq!(current.updated, deleted.updated);
    }
}

#[crate::sqlx_test]
async fn test_interface_creation_checks_domain_liveness(pool: PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let domain_id = create_domain(&env, "interface-creation.example").await;
    let segment_id = NetworkSegmentId::new();
    env.api
        .create_network_segment(Request::new(network_segment_request(
            segment_id,
            "interface-creation-segment",
            "192.0.2.0/24",
            "192.0.2.1",
            Some(domain_id),
        )))
        .await
        .unwrap();

    let mut creation = env.api.txn_begin().await.unwrap();
    let segments = db::network_segment::find_by(
        &mut creation,
        ObjectColumnFilter::One(db::network_segment::IdColumn, &segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    let live = db::machine_interface::create(
        &mut creation,
        &segments,
        &"02:00:00:00:56:61".parse().unwrap(),
        true,
        model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();
    assert_eq!(live.domain_id, Some(domain_id));
    assert!(!live.addresses.is_empty());
    creation.commit().await.unwrap();

    // Reproduce a retained segment whose Domain was deleted by the old API.
    let mut legacy_delete = env.api.txn_begin().await.unwrap();
    let domain = db::dns::domain::find_by_uuid(&mut legacy_delete, domain_id)
        .await
        .unwrap()
        .unwrap();
    db::dns::domain::delete(domain, &mut legacy_delete)
        .await
        .unwrap();
    legacy_delete.commit().await.unwrap();

    let rejected_mac = "02:00:00:00:56:62".parse().unwrap();
    let mut creation = env.api.txn_begin().await.unwrap();
    let error = db::machine_interface::create(
        &mut creation,
        &segments,
        &rejected_mac,
        true,
        model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .expect_err("a new interface must not reference a deleted Domain");
    assert!(
        matches!(error, db::DatabaseError::NotFoundError { kind: "domain", ref id } if id == &domain_id.to_string()),
        "{error:?}"
    );
    let inserted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM machine_interfaces WHERE mac_address = $1)",
    )
    .bind(rejected_mac)
    .fetch_one(&mut creation)
    .await
    .unwrap();
    assert!(!inserted);
    creation.rollback().await.unwrap();
}

#[crate::sqlx_test]
async fn test_interface_domain_reference_changes_reject_deleted_targets(pool: PgPool) {
    #[derive(Clone, Copy, Debug)]
    enum ReferenceUpdate {
        Hostname,
        Segment,
    }

    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let original_domain = create_domain(&env, "original-reference.example").await;
    let (interface_id, segment_id) = create_addressed_interface(&env, original_domain).await;
    let deleted_domain = create_domain(&env, "deleted-target.example").await;
    let mut deletion = env.api.txn_begin().await.unwrap();
    let domain = db::dns::domain::find_by_uuid(&mut deletion, deleted_domain)
        .await
        .unwrap()
        .unwrap();
    db::dns::domain::delete(domain, &mut deletion)
        .await
        .unwrap();
    deletion.commit().await.unwrap();

    for (operation, current_domain) in [
        (ReferenceUpdate::Hostname, None),
        (ReferenceUpdate::Segment, Some(original_domain)),
    ] {
        let mut update = env.api.txn_begin().await.unwrap();
        sqlx::query("UPDATE machine_interfaces SET domain_id = $1 WHERE id = $2")
            .bind(current_domain)
            .bind(interface_id)
            .execute(&mut update)
            .await
            .unwrap();
        let result = match operation {
            ReferenceUpdate::Hostname => {
                db::machine_interface::sync_hostname_after_address_assignment(
                    &mut update,
                    interface_id,
                    Some(deleted_domain),
                )
                .await
            }
            ReferenceUpdate::Segment => {
                db::machine_interface::update_segment_id(
                    &mut update,
                    interface_id,
                    segment_id,
                    Some(deleted_domain),
                )
                .await
            }
        };
        let error = result.expect_err("adding or rebinding a deleted Domain is rejected");
        assert!(
            matches!(error, db::DatabaseError::NotFoundError { kind: "domain", ref id } if id == &deleted_domain.to_string()),
            "{operation:?}: {error:?}"
        );
        let snapshot = db::machine_interface::find_one(&mut update, interface_id)
            .await
            .unwrap();
        assert_eq!(snapshot.domain_id, current_domain, "{operation:?}");
        update.rollback().await.unwrap();
    }
}
