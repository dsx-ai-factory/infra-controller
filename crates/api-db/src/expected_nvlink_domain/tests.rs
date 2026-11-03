/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use super::*;

async fn inventory(pool: &sqlx::PgPool) -> ExpectedNvLinkDomain {
    sqlx::raw_sql("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group', 'topology', '[{\"rack_id\":\"rack\",\"members\":[]}]');
        INSERT INTO expected_machines (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('compute', '00:11:22:33:44:55', 'test', 'test', 'rack');
        INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('switch', '00:11:22:33:44:66', 'test', 'test', 'rack');")
        .execute(pool).await.unwrap();
    ExpectedNvLinkDomain {
        id: Uuid::new_v4(),
        rack_group_id: RackGroupId::new("group"),
        name: None,
        member_bmc_mac_addresses: vec![
            "00:11:22:33:44:55".parse().unwrap(),
            "00:11:22:33:44:66".parse().unwrap(),
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
    sqlx::query("ALTER TABLE expected_nvlink_domains RENAME CONSTRAINT expected_nvlink_domains_rack_group_id_fkey TO test_domain_parent_reference")
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
    let replacement = ExpectedNvLinkDomain {
        id: Uuid::new_v4(),
        ..domain
    };
    create(&mut txn, &replacement).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(find_ids(&pool, None).await.unwrap(), vec![replacement.id]);
    sqlx::raw_sql("INSERT INTO expected_machines (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('compute-2', '00:11:22:33:44:77', 'test', 'test', 'rack');
        INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('switch-2', '00:11:22:33:44:88', 'test', 'test', 'rack');")
        .execute(&pool).await.unwrap();
    let peer = ExpectedNvLinkDomain {
        id: Uuid::new_v4(),
        member_bmc_mac_addresses: vec![
            "00:11:22:33:44:77".parse().unwrap(),
            "00:11:22:33:44:88".parse().unwrap(),
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
async fn membership_admission(pool: sqlx::PgPool) {
    let domain = inventory(&pool).await;
    type Mutation = fn(&mut ExpectedNvLinkDomain);
    let cases: &[(&str, Mutation)] = &[
        ("unknown group", |d| {
            d.rack_group_id = RackGroupId::new("unknown")
        }),
        ("unknown member", |d| {
            d.member_bmc_mac_addresses[0] = "00:11:22:33:44:77".parse().unwrap()
        }),
        ("missing switch", |d| {
            d.member_bmc_mac_addresses.pop();
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
    for rack in [Some("other-rack"), None] {
        sqlx::query("UPDATE expected_switches SET rack_id=$1")
            .bind(rack)
            .execute(&pool)
            .await
            .unwrap();
        let mut txn = pool.begin().await.unwrap();
        assert!(matches!(
            create(&mut txn, &domain).await,
            Err(DatabaseError::InvalidArgument(_))
        ));
        txn.rollback().await.unwrap();
    }
    sqlx::raw_sql("UPDATE expected_switches SET rack_id='rack'; INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('ambiguous', '00:11:22:33:44:55', 'test', 'test', 'rack');")
        .execute(&pool).await.unwrap();
    let mut txn = pool.begin().await.unwrap();
    assert!(matches!(
        create(&mut txn, &domain).await,
        Err(DatabaseError::InvalidArgument(_))
    ));
    txn.rollback().await.unwrap();
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
    let competing = ExpectedNvLinkDomain {
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
    let domain = inventory(&pool).await;
    sqlx::raw_sql("DROP TABLE expected_nvlink_domain_members; DROP TABLE expected_nvlink_domains;")
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
    create(&mut txn, &domain).await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        find_by_ids(&pool, &[domain.id]).await.unwrap(),
        vec![domain]
    );
}
