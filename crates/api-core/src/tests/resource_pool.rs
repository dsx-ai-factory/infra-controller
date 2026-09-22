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

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use common::api_fixtures::create_test_env;
use model::machine::ManagedHostState;
use model::resource_pool::common::LOOPBACK_IP_V6;
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolError, ResourcePoolStats as St, ValueType,
};
use rpc::forge::forge_server::Forge;

use crate::tests::common;

// Define an IPv4 pool from a range via the admin grpc
#[crate::sqlx_test]
async fn test_define_range(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let toml = r#"
[test_define_range]
type = "ipv4"
ranges = [{ start = "172.0.1.0", end = "172.0.1.255" }]
"#;
    let rp_req = rpc::forge::GrowResourcePoolRequest {
        text: toml.to_string(),
    };
    env.api
        .admin_grow_resource_pool(tonic::Request::new(rp_req))
        .await
        .unwrap();

    let pool: ResourcePool<Ipv4Addr> =
        ResourcePool::new("test_define_range".to_string(), ValueType::Ipv4);

    let mut txn = db_pool.begin().await?;
    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 0,
            free: 255,
            auto_assign_free: 255,
            auto_assign_used: 0,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );

    Ok(())
}

// Define an IPv4 pool from a prefix via the admin grpc
#[crate::sqlx_test]
async fn test_define_prefix(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let toml = r#"
[test_define_range]
type = "ipv4"
prefix = "172.0.1.0/24"
"#;
    let rp_req = rpc::forge::GrowResourcePoolRequest {
        text: toml.to_string(),
    };
    env.api
        .admin_grow_resource_pool(tonic::Request::new(rp_req))
        .await
        .unwrap();

    let pool: ResourcePool<Ipv4Addr> =
        ResourcePool::new("test_define_range".to_string(), ValueType::Ipv4);

    let mut txn = db_pool.begin().await?;
    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 0,
            free: 255,
            auto_assign_free: 255,
            auto_assign_used: 0,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_grow_ipv6_loopback_pool_backfills_existing_dpus(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let dpu_machine_ids = [
        MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?,
        MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?,
    ];

    // These rows predate the optional pool. Growing `lo-ip-v6` at runtime must
    // reconcile them without waiting for another API restart or discovery.
    let mut txn = db_pool.begin().await?;
    for machine_id in &dpu_machine_ids {
        db::machine::create(
            txn.as_mut(),
            None,
            machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
    }
    txn.commit().await?;

    let toml = r#"
[lo-ip-v6]
type = "ipv6"
prefix = "2001:db8:2389::/126"
"#;
    let grow = || rpc::forge::GrowResourcePoolRequest {
        text: toml.to_string(),
    };
    env.api
        .admin_grow_resource_pool(tonic::Request::new(grow()))
        .await?;

    let mut assigned = HashSet::new();
    let mut versions = Vec::new();
    let mut txn = db_pool.begin().await?;
    for machine_id in &dpu_machine_ids {
        let config = db::machine::get_network_config(txn.as_mut(), machine_id).await?;
        assigned.insert(
            config
                .value
                .loopback_ip_v6
                .expect("runtime pool growth should backfill every existing DPU"),
        );
        versions.push(config.version);
    }
    txn.commit().await?;
    assert_eq!(assigned.len(), dpu_machine_ids.len());

    let stats_after_backfill = db::resource_pool::stats(&db_pool, LOOPBACK_IP_V6).await?;
    assert_eq!(stats_after_backfill.used, dpu_machine_ids.len());

    // Replaying the additive grow request also replays reconciliation. Both the
    // persisted addresses and their group versions must remain unchanged.
    env.api
        .admin_grow_resource_pool(tonic::Request::new(grow()))
        .await?;
    let mut txn = db_pool.begin().await?;
    for (machine_id, version) in dpu_machine_ids.iter().zip(versions) {
        let config = db::machine::get_network_config(txn.as_mut(), machine_id).await?;
        assert!(assigned.contains(&config.value.loopback_ip_v6.unwrap()));
        assert_eq!(config.version, version);
    }
    txn.commit().await?;
    assert_eq!(
        db::resource_pool::stats(&db_pool, LOOPBACK_IP_V6).await?,
        stats_after_backfill
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_simple(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let mut txn = db_pool.begin().await?;
    let pool = ResourcePool::new("test_simple".to_string(), ValueType::Integer);

    // one auto-assignable value in the pool
    db::resource_pool::populate(&pool, &mut txn, vec!["1".to_string()], true).await?;

    // one non-auto-assignable value in the pool
    db::resource_pool::populate(&pool, &mut txn, vec!["2".to_string()], false).await?;

    // Get an auto-allocated value
    let auto_allocated =
        db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "123", None).await?;
    assert_eq!(auto_allocated, "1");
    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 1,
            free: 1,
            auto_assign_free: 0,
            auto_assign_used: 1,
            non_auto_assign_free: 1,
            non_auto_assign_used: 0
        }
    );

    // no more auto values
    match db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "id456", None).await {
        Err(db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
            ResourcePoolError::Empty,
        )) => {} // expected
        Err(err) => panic!("Unexpected err: {err}"),
        Ok(_) => panic!("Pool should be empty"),
    }

    // Get an non-auto-allocated value
    let non_auto_allocated = db::resource_pool::allocate(
        &pool,
        &mut txn,
        OwnerType::Machine,
        "123",
        Some("2".to_string()),
    )
    .await?;
    assert_eq!(non_auto_allocated, "2");
    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 2,
            free: 0,
            auto_assign_free: 0,
            auto_assign_used: 1,
            non_auto_assign_free: 0,
            non_auto_assign_used: 1
        }
    );

    // return the values
    db::resource_pool::release(&pool, &mut txn, auto_allocated).await?;
    db::resource_pool::release(&pool, &mut txn, non_auto_allocated).await?;

    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 0,
            free: 2,
            auto_assign_free: 1,
            auto_assign_used: 0,
            non_auto_assign_free: 1,
            non_auto_assign_used: 0
        }
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_rollback(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let pool = ResourcePool::new("test_rollback".to_string(), ValueType::Integer);

    // Pool has a single value
    let mut txn = db_pool.begin().await?;
    db::resource_pool::populate(&pool, &mut txn, vec![1], true).await?;
    txn.commit().await?;

    // Which we allocate then rollback
    let mut txn = db_pool.begin().await?;
    db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "my_id", None).await?;
    assert_eq!(
        db::resource_pool::stats(&mut *txn, pool.name()).await?,
        St {
            used: 1,
            free: 0,
            auto_assign_free: 0,
            auto_assign_used: 1,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );
    txn.rollback().await?;

    // The single value should be available
    assert_eq!(
        db::resource_pool::stats(&db_pool, pool.name()).await?,
        St {
            used: 0,
            free: 1,
            auto_assign_free: 1,
            auto_assign_used: 0,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );
    let mut txn = db_pool.begin().await?;
    db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "my_id", None).await?;
    txn.commit().await?;

    // And now it's really allocated
    assert_eq!(
        db::resource_pool::stats(&db_pool, pool.name()).await?,
        St {
            used: 1,
            free: 0,
            auto_assign_free: 0,
            auto_assign_used: 1,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_list(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let mut txn = db_pool.begin().await?;
    let names = &["a", "b", "c"];
    let max = &[10, 100, 500];

    // Setup
    let pool1 = ResourcePool::new(names[0].to_string(), ValueType::Integer);
    let pool2 = ResourcePool::new(names[1].to_string(), ValueType::Integer);
    let pool3 = ResourcePool::new(names[2].to_string(), ValueType::Integer);
    db::resource_pool::populate(&pool1, &mut txn, (1..=max[0]).collect::<Vec<_>>(), true).await?;
    db::resource_pool::populate(&pool2, &mut txn, (1..=max[1]).collect::<Vec<_>>(), true).await?;
    db::resource_pool::populate(&pool3, &mut txn, (1..=max[2]).collect::<Vec<_>>(), true).await?;
    for _ in 1..=5 {
        let _ = db::resource_pool::allocate(&pool1, &mut txn, OwnerType::Machine, "my_id", None)
            .await
            .unwrap();
    }

    // What we're testing
    let all = db::resource_pool::all(&mut txn).await?;

    // Verify
    assert_eq!(all.len(), 3);
    for (i, snapshot) in all.iter().enumerate() {
        assert_eq!(names[i], snapshot.name);
        assert_eq!(1, snapshot.min.parse::<i32>()?);
        assert_eq!(max[i], snapshot.max.parse::<i32>()?);
        if i == 0 {
            assert_eq!(5, snapshot.stats.used);
            assert_eq!(5, snapshot.stats.free);
        } else {
            assert_eq!(0, snapshot.stats.used);
        }
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_allocate(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let pool = ResourcePool::new("test_rollback".to_string(), ValueType::Integer);

    let mut txn = db_pool.begin().await?;
    db::resource_pool::populate(&pool, &mut txn, vec![1, 2], true).await?; // Auto-assign
    txn.commit().await?;

    // allocate in one transaction
    let mut txn1 = db_pool.begin().await?;
    let v1 =
        db::resource_pool::allocate(&pool, &mut txn1, OwnerType::Machine, "my_id", None).await?;
    assert_eq!(
        db::resource_pool::stats(&mut *txn1, pool.name()).await?,
        St {
            used: 1,
            free: 1,
            auto_assign_free: 1,
            auto_assign_used: 1,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );

    // allocate in second transaction
    let mut txn2 = db_pool.begin().await?;
    let v2 =
        db::resource_pool::allocate(&pool, &mut txn2, OwnerType::Machine, "my_id", None).await?;
    assert_eq!(
        db::resource_pool::stats(&mut *txn2, pool.name()).await?,
        St {
            used: 1,
            free: 1,
            auto_assign_free: 1,
            auto_assign_used: 1,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );
    // commit second transaction
    txn2.commit().await.expect("txn2 commit failed");
    txn1.commit().await.expect("txn1 commit failed");

    assert_eq!(
        db::resource_pool::stats(&db_pool, pool.name()).await?,
        St {
            used: 2,
            free: 0,
            auto_assign_free: 0,
            auto_assign_used: 2,
            non_auto_assign_free: 0,
            non_auto_assign_used: 0
        }
    );
    assert_ne!(v1, v2);
    Ok(())
}
