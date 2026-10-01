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

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use carbide_uuid::machine::MachineId;
use common::api_fixtures::{TestEnv, create_test_env};
use model::machine::ManagedHostState;
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection,
    ResourcePoolValueDomain,
};
use model::resource_pool::common::LOOPBACK_IP_V6;
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolError, ResourcePoolStats as St, ValueType,
};
use rpc::forge::forge_server::Forge;

use crate::cfg::file::{AuthConfig, CarbideConfig, ListenMode};
use crate::cfg::resource_pool::{ResourcePoolBackendConfig, ResourcePoolTlsConfig};
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
    assert_eq!(
        db::resource_pool::release(&pool, &mut txn, auto_allocated, OwnerType::Machine, "123")
            .await?,
        db::ConditionalWrite::Applied(()),
    );
    assert_eq!(
        db::resource_pool::release(
            &pool,
            &mut txn,
            non_auto_allocated,
            OwnerType::Machine,
            "123",
        )
        .await?,
        db::ConditionalWrite::Applied(()),
    );

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

#[crate::sqlx_test]
async fn startup_rejects_remote_selection_before_pool_effects(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    // A late rejection must not adopt a pool that predates backend bindings.
    let legacy_pool = ResourcePool::new("unbound-before-invalid".to_owned(), ValueType::Integer);
    let mut txn = db_pool.begin().await?;
    db::resource_pool::populate(&legacy_pool, &mut txn, vec![42], true).await?;
    txn.commit().await?;
    let provider = std::net::TcpListener::bind("127.0.0.1:0")?;
    provider.set_nonblocking(true)?;
    let original = pool_database_snapshot(&db_pool).await?;
    assert!(
        original
            .bindings
            .iter()
            .all(|binding| binding.pool_name != legacy_pool.name())
    );

    for (scenario, listen_only, pkey_selection) in [
        ("ordinary pool on authoritative startup", false, false),
        ("ordinary pool on listen-only startup", true, false),
        ("enabled IB PKey pool on authoritative startup", false, true),
        ("disabled IB PKey pool on listen-only startup", true, true),
    ] {
        let mut config = (*env.api.runtime_config).clone();
        config.listen_only = listen_only;
        config.pools = Some(HashMap::from([(
            "new-before-invalid".to_owned(),
            model::resource_pool::ResourcePoolDef {
                pool_type: model::resource_pool::ResourcePoolType::Ipv4,
                prefix: Some("192.0.2.0/30".to_owned()),
                ranges: Vec::new(),
                delegate_prefix_len: None,
            }
            .into(),
        )]));
        config.resource_pool_backends.insert(
            "remote".to_owned(),
            ResourcePoolBackendConfig::Grpc {
                endpoint: format!("http://{}", provider.local_addr()?).parse()?,
                tls: None,
            },
        );
        let rejected_name = if pkey_selection {
            let fabric = config.ib_fabrics.get_mut("default").unwrap();
            fabric.backend = Some("remote".to_owned());
            if !listen_only {
                config.ib_config = Some(carbide_ib_fabric::config::IBFabricConfig {
                    enabled: true,
                    ..Default::default()
                });
                fabric.pkeys = vec![model::resource_pool::Range {
                    start: "2000".to_owned(),
                    end: "2003".to_owned(),
                    auto_assign: true,
                }];
            }
            model::resource_pool::common::ib_pkey_pool_name("default")
        } else {
            let mut remote = config.pools.as_ref().unwrap()["new-before-invalid"].clone();
            remote.backend = Some("remote".to_owned());
            config
                .pools
                .as_mut()
                .unwrap()
                .insert("invalid-ordinary".to_owned(), remote);
            "invalid-ordinary".to_owned()
        };
        let mut join_set = tokio::task::JoinSet::new();
        let error = Box::pin(crate::start_runtime(crate::bootstrap::RuntimeInputs {
            carbide_config: Arc::new(config),
            initial_objects: None,
            meter: env.test_meter.meter(),
            per_object_metrics: None,
            join_set: &mut join_set,
            runtime_prelude: crate::bootstrap::RuntimePrelude {
                dynamic_settings: env.api.dynamic_settings.clone(),
            },
            credential_manager: env.api.credential_manager.clone(),
            certificate_provider: env.api.certificate_provider.clone(),
            db_pool: db_pool.clone(),
            work_lock_manager_handle: env.api.work_lock_manager_handle.clone(),
            secrets_context: None,
            admin_ui_routes_builder: Box::new(|_, _| {
                panic!("invalid pools must fail before routes")
            }),
            cancel_token: tokio_util::sync::CancellationToken::new(),
        }))
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("v2.4 requires integrated"),
            "{scenario}: {error}"
        );
        assert!(error.contains(&rejected_name), "{scenario}: {error}");
        assert!(join_set.is_empty());
        assert_eq!(
            pool_database_snapshot(&db_pool).await?,
            original,
            "{scenario}",
        );
    }
    assert_eq!(
        provider.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}

#[crate::sqlx_test]
async fn listen_only_restart_retains_bindings_without_seeding_or_contacting_unused_backend(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    // Add a pool as it existed before backend bindings. Listen-only startup
    // must adopt it without adding or changing any of its allocation values.
    let legacy_pool = ResourcePool::new("listen-only-legacy".to_owned(), ValueType::Integer);
    let mut txn = db_pool.begin().await?;
    db::resource_pool::populate(&legacy_pool, &mut txn, vec![42], true).await?;
    txn.commit().await?;

    let provider = std::net::TcpListener::bind("127.0.0.1:0")?;
    provider.set_nonblocking(true)?;
    let absent_certificate =
        std::env::temp_dir().join(format!("unused-resource-pool-{}.pem", uuid::Uuid::new_v4()));
    assert!(!absent_certificate.exists());
    let original_bindings = db::resource_pool::binding::all(&db_pool).await?;
    assert!(
        original_bindings
            .iter()
            .all(|binding| binding.pool_name != legacy_pool.name())
    );
    let original_inventory: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
    )
    .fetch_all(&db_pool)
    .await?;
    let original_definitions = db::resource_pool::all_stored_defs(&db_pool).await?;
    let pkey_name = model::resource_pool::common::ib_pkey_pool_name("default");
    let original_pkey_stats = db::resource_pool::stats(&db_pool, &pkey_name).await?;
    let mut adopted_binding = None;

    for restart in [false, true] {
        let mut config = pool_startup_config(&env);
        config.listen_only = true;
        config.ib_config = Some(carbide_ib_fabric::config::IBFabricConfig {
            enabled: true,
            ..Default::default()
        });
        config.ib_fabrics.get_mut("default").unwrap().pkeys = vec![model::resource_pool::Range {
            start: "2000".to_owned(),
            end: "2003".to_owned(),
            auto_assign: true,
        }];
        config.pools = if restart {
            config.resource_pool_backends.insert(
                "unused".to_owned(),
                ResourcePoolBackendConfig::Grpc {
                    endpoint: format!("https://{}", provider.local_addr()?).parse()?,
                    tls: Some(ResourcePoolTlsConfig {
                        ca_cert_file: Some(absent_certificate.clone()),
                        client_cert_file: Some(absent_certificate.clone()),
                        client_key_file: Some(absent_certificate.clone()),
                    }),
                },
            );
            None
        } else {
            config.resource_pool_backends.insert(
                "local-alias".to_owned(),
                ResourcePoolBackendConfig::Integrated,
            );
            Some(HashMap::from([(
                "listen-only-new".to_owned(),
                model::resource_pool::ResourcePoolConfig {
                    definition: model::resource_pool::ResourcePoolDef {
                        pool_type: model::resource_pool::ResourcePoolType::Ipv4,
                        prefix: Some("192.0.2.0/30".to_owned()),
                        ranges: Vec::new(),
                        delegate_prefix_len: None,
                    },
                    backend: Some("local-alias".to_owned()),
                    remote_pool: None,
                },
            )]))
        };

        // Seed declarations do not change an existing pool's value type.
        config.pools.get_or_insert_default().insert(
            model::resource_pool::common::LOOPBACK_IP.to_owned(),
            model::resource_pool::ResourcePoolDef {
                pool_type: model::resource_pool::ResourcePoolType::Integer,
                prefix: None,
                ranges: vec![model::resource_pool::Range {
                    start: "1".to_owned(),
                    end: "3".to_owned(),
                    auto_assign: true,
                }],
                delegate_prefix_len: None,
            }
            .into(),
        );

        assert_ne!(start_and_stop_runtime(&env, config).await?.port(), 0);
        let mut bindings = db::resource_pool::binding::all(&db_pool).await?;
        let legacy_index = bindings
            .iter()
            .position(|binding| binding.pool_name == legacy_pool.name())
            .expect("listen-only startup must bind the existing legacy pool");
        let legacy_binding = bindings.remove(legacy_index);
        assert_eq!(
            legacy_binding.value_domain,
            ResourcePoolValueDomain::Integer
        );
        assert_eq!(
            legacy_binding.backend_kind,
            ResourcePoolBackendKind::Integrated
        );
        assert_eq!(legacy_binding.backend_name, None);
        assert_eq!(legacy_binding.authority_id, INTEGRATED_AUTHORITY_ID);
        assert_eq!(legacy_binding.remote_pool, legacy_pool.name());
        assert!(!legacy_binding.binding_id.is_nil());
        if let Some(original) = &adopted_binding {
            assert_eq!(&legacy_binding, original);
        } else {
            adopted_binding = Some(legacy_binding);
        }
        assert_eq!(bindings, original_bindings);
        let inventory: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
        )
        .fetch_all(&db_pool)
        .await?;
        assert_eq!(inventory, original_inventory);
        assert_eq!(
            db::resource_pool::all_stored_defs(&db_pool).await?,
            original_definitions
        );
        assert_eq!(
            db::resource_pool::stats(&db_pool, &pkey_name).await?,
            original_pkey_stats
        );
        let seeded_rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM resource_pool WHERE name = 'listen-only-new'")
                .fetch_one(&db_pool)
                .await?;
        assert_eq!(seeded_rows, 0);
        assert_eq!(
            provider.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn authoritative_restart_grows_pkeys_and_preserves_allocations_and_seeded_pools(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let pkey_pool = &env.common_pools.infiniband.pkey_pools["default"];
    remove_fixture_pkey_pool(&db_pool, pkey_pool.name()).await?;
    let mut config = pool_startup_config(&env);
    config.listen_only = false;
    config.ib_config = Some(carbide_ib_fabric::config::IBFabricConfig {
        enabled: true,
        ..Default::default()
    });
    config.ib_fabrics.get_mut("default").unwrap().pkeys = vec![model::resource_pool::Range {
        start: "2000".to_owned(),
        end: "2001".to_owned(),
        auto_assign: true,
    }];
    config.pools = Some(HashMap::from([(
        "startup-seed".to_owned(),
        model::resource_pool::ResourcePoolDef {
            pool_type: model::resource_pool::ResourcePoolType::Ipv4,
            prefix: Some("192.0.2.0/30".to_owned()),
            ranges: Vec::new(),
            delegate_prefix_len: None,
        }
        .into(),
    )]));
    assert_ne!(
        start_and_stop_runtime(&env, config.clone()).await?.port(),
        0
    );
    let mut txn = db_pool.begin().await?;
    let pkey = db::resource_pool::allocate(
        pkey_pool,
        &mut txn,
        OwnerType::IBPartition,
        "existing-partition",
        None,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(pkey, 2000);
    let original = pool_database_snapshot(&db_pool).await?;
    let binding = original
        .bindings
        .iter()
        .find(|binding| binding.pool_name == pkey_pool.name())
        .unwrap();
    assert!(!binding.binding_id.is_nil());
    assert_eq!(binding.value_domain, ResourcePoolValueDomain::Integer);

    // PKey ranges grow on restart; ordinary pool declarations remain seeds.
    config.ib_fabrics.get_mut("default").unwrap().pkeys[0].end = "2002".to_owned();
    config
        .pools
        .as_mut()
        .unwrap()
        .get_mut("startup-seed")
        .unwrap()
        .definition
        .prefix = Some("198.51.100.0/30".to_owned());
    assert_ne!(start_and_stop_runtime(&env, config).await?.port(), 0);
    let mut restarted = pool_database_snapshot(&db_pool).await?;
    let new_pkey = restarted
        .inventory
        .iter()
        .position(|row| row["name"] == pkey_pool.name() && row["value"] == "2001")
        .expect("restart must extend the enabled fabric's PKey inventory");
    restarted.inventory.remove(new_pkey);
    assert_eq!(restarted, original);

    let mut txn = db_pool.begin().await?;
    let added_pkey = db::resource_pool::allocate(
        pkey_pool,
        &mut txn,
        OwnerType::IBPartition,
        "new-partition",
        None,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(added_pkey, 2001);
    Ok(())
}

#[crate::sqlx_test]
async fn authoritative_startup_rejects_persisted_ip_address_pkey_pool_without_changes(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let pkey_name = model::resource_pool::common::ib_pkey_pool_name("default");
    remove_fixture_pkey_pool(&db_pool, &pkey_name).await?;
    let mut txn = db_pool.begin().await?;
    let existing_pool = ResourcePool::new(pkey_name.clone(), ValueType::Ipv4);
    db::resource_pool::populate(
        &existing_pool,
        &mut txn,
        vec!["192.0.2.10".parse::<Ipv4Addr>()?],
        true,
    )
    .await?;
    txn.commit().await?;
    let original = pool_database_snapshot(&db_pool).await?;
    let mut config = pool_startup_config(&env);
    config.listen_only = false;
    config.pools = Some(HashMap::new());
    config.ib_config = Some(carbide_ib_fabric::config::IBFabricConfig {
        enabled: true,
        ..Default::default()
    });
    config.ib_fabrics.get_mut("default").unwrap().pkeys = vec![model::resource_pool::Range {
        start: "2000".to_owned(),
        end: "2001".to_owned(),
        auto_assign: true,
    }];
    let error = start_and_stop_runtime(&env, config)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&pkey_name), "{error}");
    assert!(
        error.contains("requires integer values, but its stored value domain is ip_address"),
        "{error}"
    );
    assert_eq!(pool_database_snapshot(&db_pool).await?, original);
    Ok(())
}

#[crate::sqlx_test]
async fn grow_preserves_alias_binding_and_binds_new_pool(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let mut txn = db_pool.begin().await?;
    let binding = db::resource_pool::binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: "alias-pool".to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("local-alias".to_owned()),
            backend_kind: ResourcePoolBackendKind::Integrated,
            authority_id: INTEGRATED_AUTHORITY_ID.to_owned(),
            remote_pool: "alias-pool".to_owned(),
        },
    )
    .await?;
    txn.commit().await?;
    env.api
        .admin_grow_resource_pool(tonic::Request::new(rpc::forge::GrowResourcePoolRequest {
            text: r#"
[alias-pool]
type = "integer"
ranges = [{ start = "1", end = "3" }]
[new-pool]
type = "integer"
ranges = [{ start = "5", end = "7" }]
"#
            .to_owned(),
        }))
        .await?;
    let bindings = db::resource_pool::binding::all(&db_pool).await?;
    assert_eq!(
        bindings
            .iter()
            .find(|stored| stored.pool_name == "alias-pool"),
        Some(&binding)
    );
    let new_binding = bindings
        .iter()
        .find(|stored| stored.pool_name == "new-pool")
        .unwrap();
    assert_eq!(
        new_binding.backend_kind,
        ResourcePoolBackendKind::Integrated
    );
    assert_eq!(new_binding.backend_name, None);
    assert_eq!(new_binding.remote_pool, "new-pool");
    assert_eq!(
        db::resource_pool::stats(&db_pool, "alias-pool").await?.free,
        2
    );
    assert_eq!(
        db::resource_pool::stats(&db_pool, "new-pool").await?.free,
        2
    );
    Ok(())
}

#[crate::sqlx_test]
async fn grow_preserves_binding_when_adding_another_ip_family(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let grow = |pool_type, start, end| {
        tonic::Request::new(rpc::forge::GrowResourcePoolRequest {
            text: format!(
                r#"
[mixed-family-growth]
type = "{pool_type}"
ranges = [{{ start = "{start}", end = "{end}" }}]
"#
            ),
        })
    };
    env.api
        .admin_grow_resource_pool(grow("ipv4", "192.0.2.10", "192.0.2.11"))
        .await?;
    let original_binding = db::resource_pool::binding::all(&db_pool)
        .await?
        .into_iter()
        .find(|binding| binding.pool_name == "mixed-family-growth")
        .unwrap();
    assert_eq!(
        original_binding.value_domain,
        ResourcePoolValueDomain::IpAddress
    );
    let pool = ResourcePool::<IpAddr>::new("mixed-family-growth".to_owned(), ValueType::Ipv4);
    let mut txn = db_pool.begin().await?;
    let ipv4 =
        db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "first-owner", None)
            .await?;
    txn.commit().await?;
    assert_eq!(ipv4, "192.0.2.10".parse::<IpAddr>()?);
    let original_allocation: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool) FROM resource_pool WHERE name = $1 AND value = $2",
    )
    .bind(pool.name())
    .bind(ipv4.to_string())
    .fetch_one(&db_pool)
    .await?;

    env.api
        .admin_grow_resource_pool(grow("ipv6", "2001:db8::10", "2001:db8::11"))
        .await?;
    assert_eq!(
        db::resource_pool::binding::all(&db_pool)
            .await?
            .into_iter()
            .find(|binding| binding.pool_name == "mixed-family-growth"),
        Some(original_binding),
    );
    let allocation: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool) FROM resource_pool WHERE name = $1 AND value = $2",
    )
    .bind(pool.name())
    .bind(ipv4.to_string())
    .fetch_one(&db_pool)
    .await?;
    assert_eq!(allocation, original_allocation);

    let mut txn = db_pool.begin().await?;
    let ipv6 =
        db::resource_pool::allocate(&pool, &mut txn, OwnerType::Machine, "second-owner", None)
            .await?;
    txn.commit().await?;
    assert_eq!(ipv6, "2001:db8::10".parse::<IpAddr>()?);
    Ok(())
}

#[crate::sqlx_test]
async fn grow_rejects_invalid_batches_without_changing_bindings_or_inventory(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let original = pool_database_snapshot(&db_pool).await?;
    for (scenario, text, expected_code, expected_error) in [
        (
            "invalid second range rolls back inventory and binding",
            r#"
[invalid-growth]
type = "integer"
ranges = [{ start = "1", end = "3" }, { start = "not-an-integer", end = "7" }]
"#,
            tonic::Code::InvalidArgument,
            "invalid digit found in string",
        ),
        (
            "blank logical name is rejected before binding",
            r#"
[valid-growth]
type = "integer"
ranges = [{ start = "1", end = "3" }]
["   "]
type = "integer"
ranges = [{ start = "5", end = "7" }]
"#,
            tonic::Code::InvalidArgument,
            "resource pool name must not be empty",
        ),
        (
            "growth cannot change an existing pool's value domain",
            r#"
[valid-growth]
type = "integer"
ranges = [{ start = "1", end = "3" }]
[vni]
type = "ipv4"
ranges = [{ start = "192.0.2.10", end = "192.0.2.11" }]
"#,
            tonic::Code::FailedPrecondition,
            "pool \"vni\" has stored value domain integer but configured value domain ip_address",
        ),
    ] {
        let error = env
            .api
            .admin_grow_resource_pool(tonic::Request::new(rpc::forge::GrowResourcePoolRequest {
                text: text.to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected_code, "{scenario}");
        assert!(
            error.message().contains(expected_error),
            "{scenario}: {error}"
        );
        assert_eq!(
            pool_database_snapshot(&db_pool).await?,
            original,
            "{scenario}",
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn grow_rejects_retained_remote_binding_without_inventory(
    db_pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(db_pool.clone()).await;
    let mut txn = db_pool.begin().await?;
    db::resource_pool::binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: "remote-pool".to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("remote".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "remote-authority".to_owned(),
            remote_pool: "remote-pool".to_owned(),
        },
    )
    .await?;
    txn.commit().await?;
    let original_bindings = db::resource_pool::binding::all(&db_pool).await?;
    let error = env
        .api
        .admin_grow_resource_pool(tonic::Request::new(rpc::forge::GrowResourcePoolRequest {
            text: r#"
[valid-growth]
type = "integer"
ranges = [{ start = "1", end = "3" }]
[remote-pool]
type = "integer"
ranges = [{ start = "5", end = "7" }]
"#
            .to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("remote-pool"));
    assert!(
        error.message().contains("unsupported allocation authority"),
        "{error}"
    );
    assert_eq!(
        db::resource_pool::binding::all(&db_pool).await?,
        original_bindings
    );
    let new_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_pool WHERE name IN ('valid-growth', 'remote-pool')",
    )
    .fetch_one(&db_pool)
    .await?;
    assert_eq!(new_rows, 0);
    Ok(())
}

#[derive(Debug, PartialEq)]
struct PoolDatabaseSnapshot {
    inventory: Vec<serde_json::Value>,
    definitions: Vec<serde_json::Value>,
    bindings: Vec<ResourcePoolBinding>,
}

fn pool_startup_config(env: &TestEnv) -> CarbideConfig {
    let mut config = (*env.api.runtime_config).clone();
    config.listen = SocketAddr::from(([127, 0, 0, 1], 0));
    config.listen_mode = ListenMode::PlaintextHttp2;
    config.tls = None;
    config.dpf.enabled = false;
    config.rms.api_url = None;
    config.component_manager = None;
    config.ssh_console_url = None;
    config.ewethers_config = None;
    config.enable_admin_ui = true;
    config.auth = Some(AuthConfig {
        permissive_mode: false,
        allow_machineatron_scout_stream: false,
        casbin_policy_file: None,
        cli_certs: None,
        trust: Some(carbide_authn::config::TrustConfig {
            spiffe_trust_domain: "resource-pool.test".to_owned(),
            spiffe_service_base_paths: vec!["/services".to_owned()],
            spiffe_machine_base_path: "/machines".to_owned(),
            additional_issuer_cns: Vec::new(),
        }),
    });
    config
}

async fn start_and_stop_runtime(env: &TestEnv, config: CarbideConfig) -> eyre::Result<SocketAddr> {
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let mut join_set = tokio::task::JoinSet::new();
    let started = Box::pin(crate::start_runtime(crate::bootstrap::RuntimeInputs {
        carbide_config: Arc::new(config),
        initial_objects: None,
        meter: env.test_meter.meter(),
        per_object_metrics: None,
        join_set: &mut join_set,
        runtime_prelude: crate::bootstrap::RuntimePrelude {
            dynamic_settings: env.api.dynamic_settings.clone(),
        },
        credential_manager: env.api.credential_manager.clone(),
        certificate_provider: env.api.certificate_provider.clone(),
        db_pool: env.pool.clone(),
        work_lock_manager_handle: env.api.work_lock_manager_handle.clone(),
        secrets_context: None,
        admin_ui_routes_builder: Box::new(|_, _| {
            Ok(tower_http::normalize_path::NormalizePath::trim_trailing_slash(axum::Router::new()))
        }),
        cancel_token: cancel_token.clone(),
    }))
    .await;
    cancel_token.cancel();
    join_set.join_all().await;
    started
}

async fn remove_fixture_pkey_pool(db_pool: &sqlx::PgPool, name: &str) -> eyre::Result<()> {
    let mut txn = db_pool.begin().await?;
    sqlx::query("DELETE FROM resource_pool WHERE name = $1")
        .bind(name)
        .execute(&mut *txn)
        .await?;
    sqlx::query("DELETE FROM resource_pool_def WHERE name = $1")
        .bind(name)
        .execute(&mut *txn)
        .await?;
    sqlx::query("DELETE FROM resource_pool_binding WHERE pool_name = $1")
        .bind(name)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

async fn pool_database_snapshot(db_pool: &sqlx::PgPool) -> eyre::Result<PoolDatabaseSnapshot> {
    Ok(PoolDatabaseSnapshot {
        inventory: sqlx::query_scalar(
            "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
        )
        .fetch_all(db_pool)
        .await?,
        definitions: sqlx::query_scalar(
            "SELECT to_jsonb(resource_pool_def) FROM resource_pool_def ORDER BY name",
        )
        .fetch_all(db_pool)
        .await?,
        bindings: db::resource_pool::binding::all(db_pool).await?,
    })
}
