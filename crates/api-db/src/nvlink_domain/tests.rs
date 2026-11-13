/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use model::nvlink_domain::NvLinkDomainMember;
use model::rack_type::RackCapabilityType;

use super::*;

async fn inventory(pool: &sqlx::PgPool) -> NvLinkDomain {
    sqlx::raw_sql(
        r#"INSERT INTO expected_rack_groups (rack_group_id, topology, racks)
        VALUES ('group', 'topology', '[{"rack_id":"rack","members":[
        {"type":"Compute","manufacturer":"NVIDIA","id":"compute","cartridge_index":0},
        {"type":"Switch","manufacturer":"NVIDIA","id":"switch","cartridge_index":0},
        {"type":"Compute","manufacturer":"NVIDIA","id":"compute-2","cartridge_index":1},
        {"type":"Switch","manufacturer":"NVIDIA","id":"switch-2","cartridge_index":1}]},
        {"rack_id":"rack-2","members":[
        {"type":"Compute","manufacturer":"NVIDIA","id":"compute-3","cartridge_index":0},
        {"type":"Switch","manufacturer":"NVIDIA","id":"switch-3","cartridge_index":0}]}]');"#,
    )
    .execute(pool)
    .await
    .unwrap();
    NvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: RackGroupId::new("group"),
        name: None,
        members: vec![
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 0,
                component_type: RackCapabilityType::Compute,
            },
            NvLinkDomainMember {
                rack_index: 0,
                slot_index: 0,
                component_type: RackCapabilityType::Switch,
            },
        ],
    }
}

#[crate::sqlx_test]
async fn lifecycle_and_group_dependencies(pool: sqlx::PgPool) {
    let domain = inventory(&pool).await;
    let mut txn = pool.begin().await.unwrap();
    create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_by_ids(&pool, &[Uuid::new_v4(), domain.id, domain.id])
            .await
            .unwrap(),
        vec![domain.clone()]
    );
    assert_eq!(
        find_ids(&pool, Some(&domain.rack_group_id)).await.unwrap(),
        vec![domain.id]
    );
    assert!(
        find_ids(&pool, Some(&RackGroupId::new("other")))
            .await
            .unwrap()
            .is_empty()
    );

    // Dependency errors must be classified independently of the constraint name.
    sqlx::query("ALTER TABLE nvlink_domains RENAME CONSTRAINT expected_nvlink_domains_rack_group_id_fkey TO test_domain_parent_reference")
        .execute(&pool)
        .await
        .unwrap();
    for clear in [false, true] {
        let mut txn = pool.begin().await.unwrap();
        let result = if clear {
            crate::expected_rack_group::clear(&mut txn).await
        } else {
            crate::expected_rack_group::delete(&mut txn, &domain.rack_group_id).await
        };
        assert!(matches!(
            result,
            Err(DatabaseError::FailedPrecondition(message))
                if message == "cannot delete or replace rack groups while dependent records exist"
        ));
        txn.rollback().await.unwrap();
    }
    let mut txn = pool.begin().await.unwrap();
    let mut group =
        crate::expected_rack_group::find_by_rack_group_id(&mut txn, &domain.rack_group_id)
            .await
            .unwrap()
            .unwrap();
    group.metadata.name = "renamed".into();
    group.topology = model::expected_rack_group::RackGroupTopology::new("updated-topology");
    group.protocol = Some(model::expected_rack_group::RackGroupProtocol::new(
        "updated-protocol",
    ));
    crate::expected_rack_group::update(&mut txn, &group)
        .await
        .unwrap();
    assert_eq!(
        crate::expected_rack_group::find_by_rack_group_id(&mut txn, &domain.rack_group_id)
            .await
            .unwrap(),
        Some(group.clone())
    );
    group.racks.clear();
    assert!(matches!(
        crate::expected_rack_group::update(&mut txn, &group).await,
        Err(DatabaseError::FailedPrecondition(_))
    ));
    txn.rollback().await.unwrap();

    let mut txn = pool.begin().await.unwrap();
    delete(&mut txn, domain.id).await.unwrap();
    assert!(matches!(
        delete(&mut txn, domain.id).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    let replacement = NvLinkDomain {
        id: Uuid::new_v4(),
        ..domain
    };
    create(&mut txn, &replacement).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(find_ids(&pool, None).await.unwrap(), vec![replacement.id]);
    let peer = NvLinkDomain {
        id: Uuid::new_v4(),
        members: vec![
            NvLinkDomainMember {
                rack_index: 1,
                slot_index: 0,
                component_type: RackCapabilityType::Compute,
            },
            NvLinkDomainMember {
                rack_index: 1,
                slot_index: 0,
                component_type: RackCapabilityType::Switch,
            },
        ],
        ..replacement
    };
    let mut txn = pool.begin().await.unwrap();
    create(&mut txn, &peer).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_ids(&pool, Some(&peer.rack_group_id))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[crate::sqlx_test]
async fn membership_storage_constraints(pool: sqlx::PgPool) {
    let domain = inventory(&pool).await;
    type Mutation = fn(&mut NvLinkDomain);
    let cases: &[(&str, Mutation)] = &[
        ("unknown group", |d| {
            d.rack_group_id = RackGroupId::new("unknown")
        }),
        ("power shelf", |d| {
            d.members[0].component_type = RackCapabilityType::PowerShelf
        }),
    ];
    for (name, mutate) in cases {
        let mut request = domain.clone();
        mutate(&mut request);
        let mut txn = pool.begin().await.unwrap();
        assert!(create(&mut txn, &request).await.is_err(), "{name}");
        txn.rollback().await.unwrap();
        assert!(find_ids(&pool, None).await.unwrap().is_empty());
    }

    // Storage accepts declared positions without resolving RackGroup inventory.
    let declaration = NvLinkDomain {
        members: vec![NvLinkDomainMember {
            rack_index: 99,
            slot_index: u32::MAX,
            component_type: RackCapabilityType::Compute,
        }],
        ..domain
    };
    let mut txn = pool.begin().await.unwrap();
    create(&mut txn, &declaration).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_by_ids(&pool, &[declaration.id]).await.unwrap(),
        vec![declaration]
    );
}

#[crate::sqlx_test]
async fn concurrent_claims_are_atomic(pool: sqlx::PgPool) {
    let domain = inventory(&pool).await;
    let mut first = pool.begin().await.unwrap();
    create(&mut first, &domain).await.unwrap();
    let mut second = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .unwrap();
    let competing = NvLinkDomain {
        id: Uuid::new_v4(),
        ..domain.clone()
    };
    let writer = tokio::spawn(async move {
        let result = create(&mut second, &competing).await;
        second.rollback().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted)",
            )
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            assert!(!writer.is_finished(), "competing claim bypassed uniqueness");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    first.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), writer)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(DatabaseError::AlreadyFoundError { .. })
    ));
    assert_eq!(find_ids(&pool, None).await.unwrap(), vec![domain.id]);
    assert_eq!(
        find_by_ids(&pool, &[domain.id]).await.unwrap(),
        vec![domain]
    );
}

#[crate::sqlx_test]
async fn migration_preserves_existing_inventory(pool: sqlx::PgPool) {
    use sqlx::Acquire;

    let domain = inventory(&pool).await;
    sqlx::raw_sql("DROP TABLE nvlink_domain_members; DROP TABLE nvlink_domains;")
        .execute(&pool)
        .await
        .unwrap();
    let mut txn = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261014011133_expected_nvlink_domains.sql"
    ))
    .execute(&mut *txn)
    .await
    .unwrap();
    sqlx::raw_sql("INSERT INTO expected_nvlink_domains (id, rack_group_id) VALUES ('12345678-1234-5678-90ab-cdef01234567', 'group');
        INSERT INTO expected_nvlink_domain_members (domain_id, bmc_mac_address) VALUES ('12345678-1234-5678-90ab-cdef01234567', '00:11:22:33:44:55');")
        .execute(&mut *txn).await.unwrap();
    let mut attempt = txn.begin().await.unwrap();
    let error = sqlx::raw_sql(include_str!(
        "../../migrations/20261111000712_nvlink_domain_positions.sql"
    ))
    .execute(&mut *attempt)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23502")
    );
    attempt.rollback().await.unwrap();
    let retained: String =
        sqlx::query_scalar("SELECT bmc_mac_address::text FROM expected_nvlink_domain_members")
            .fetch_one(&mut *txn)
            .await
            .unwrap();
    assert_eq!(retained, "00:11:22:33:44:55");
    sqlx::query("DELETE FROM expected_nvlink_domains")
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261111000712_nvlink_domain_positions.sql"
    ))
    .execute(&mut *txn)
    .await
    .unwrap();
    create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_by_ids(&pool, &[domain.id]).await.unwrap(),
        vec![domain]
    );
}
