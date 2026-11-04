// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use carbide_resource_pool_api::v1;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use chrono::Utc;
use db::test_support::postgres::wait_for_blocked_query;
use futures::StreamExt;
use ipnetwork::Ipv6Network;
use model::resource_pool::binding::INTEGRATED_AUTHORITY_ID;
use model::resource_pool::common::{LOOPBACK_IP_V6, VPC_VNI, ib_pkey_pool_name};
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::{
    OwnerType, ResourcePool as TypedPool, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain, ValueType,
};
use prost::Message;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Code, Request, Response, Status};
use uuid::Uuid;

use crate::cfg::resource_pool::ResourcePoolBackendConfig;
use crate::logging::service_health_metrics::{
    ServiceHealthContext, start_export_service_health_metrics,
};
use crate::resource_pool::ResourcePoolMonitor;
use crate::resource_pool::metrics::{PoolStats, start_pool_stats};
use crate::test_support::default_config;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_test_env, create_test_env_with_overrides,
};
use crate::tests::resource_pool::pool_database_snapshot;

/// Pools added after startup remain inspectable without a binding, including
/// dynamic PKey pools and IPv6 prefixes. Older clients keep their local counts.
#[crate::sqlx_test]
async fn inspection_preserves_local_inventory_and_numeric_list(
    database: PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(database.clone()).await;
    let pkey_name = model::resource_pool::common::ib_pkey_pool_name("after-startup");
    let pkeys = TypedPool::<i64>::new(pkey_name.clone(), ValueType::Integer);
    let prefixes = TypedPool::<Ipv6Network>::new("inspection-prefix".into(), ValueType::Ipv6Prefix);
    let mut txn = database.begin().await?;
    db::resource_pool::populate(&pkeys, &mut txn, vec![10, 11], true).await?;
    // Distinct counts in both partitions catch swapped fields in the public RPC.
    db::resource_pool::populate(
        &prefixes,
        &mut txn,
        vec![
            "2001:db8::/64".parse()?,
            "2001:db8:1::/64".parse()?,
            "2001:db8:2::/64".parse()?,
        ],
        true,
    )
    .await?;
    let manual_prefixes = (0x10..=0x17)
        .map(|subnet| format!("2001:db8:{subnet:x}::/64").parse::<Ipv6Network>())
        .collect::<Result<Vec<_>, _>>()?;
    db::resource_pool::populate(&prefixes, &mut txn, manual_prefixes.clone(), false).await?;
    db::resource_pool::allocate(
        &prefixes,
        &mut txn,
        OwnerType::Machine,
        "automatic-owner",
        None,
    )
    .await?;
    for prefix in manual_prefixes.iter().take(3) {
        db::resource_pool::allocate(
            &prefixes,
            &mut txn,
            OwnerType::Machine,
            &prefix.to_string(),
            Some(*prefix),
        )
        .await?;
    }
    let existing =
        db::resource_pool::binding::load(&mut *txn, model::resource_pool::common::VPC_VNI)
            .await?
            .expect("fixture VPC pool binding");
    let empty = db::resource_pool::binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: "inspection-empty".into(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("local-alias".into()),
            backend_kind: ResourcePoolBackendKind::Integrated,
            authority_id: INTEGRATED_AUTHORITY_ID.into(),
            remote_pool: "inspection-empty".into(),
        },
    )
    .await?;
    txn.commit().await?;

    let pools = env
        .api
        .admin_inspect_resource_pools(Request::new(forge::InspectResourcePoolsRequest {}))
        .await?
        .into_inner()
        .pools;
    assert!(pools.windows(2).all(|pair| pair[0].name < pair[1].name));
    let pkeys = pools
        .iter()
        .find(|pool| pool.name == pkey_name)
        .expect("dynamic PKey pool");
    assert_eq!(
        pkeys.backend_kind(),
        forge::ResourcePoolBackendKind::Integrated
    );
    assert_eq!(pkeys.binding_id, None);
    assert_eq!(pkeys.authority_id, INTEGRATED_AUTHORITY_ID);
    assert_eq!(pkeys.remote_pool, pkey_name);
    assert_eq!(
        (pkeys.min.as_deref(), pkeys.max.as_deref()),
        (Some("10"), Some("11"))
    );
    assert_eq!(pkeys.stats_status(), forge::ResourcePoolStatsStatus::Known);
    assert_eq!(pkeys.stats.as_ref().expect("local counts").free, 2);
    assert!(pkeys.observed_at.is_some());

    let prefixes = pools
        .iter()
        .find(|pool| pool.name == "inspection-prefix")
        .expect("prefix pool");
    assert_eq!(prefixes.binding_id, None);
    assert_eq!(prefixes.min.as_deref(), Some("2001:db8::/64"));
    assert_eq!(prefixes.max.as_deref(), Some("2001:db8:17::/64"));
    assert_eq!(
        prefixes.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    assert!(prefixes.observed_at.is_some());
    assert_eq!(
        prefixes.stats,
        Some(forge::ResourcePoolStats {
            used: 4,
            free: 7,
            auto_assign_used: 1,
            auto_assign_free: 2,
            non_auto_assign_used: 3,
            non_auto_assign_free: 5,
        })
    );

    let existing_pool = pools
        .iter()
        .find(|pool| pool.name == existing.pool_name)
        .expect("bound VPC pool");
    assert_eq!(
        existing_pool.backend_kind(),
        forge::ResourcePoolBackendKind::Integrated
    );
    assert_eq!(
        existing_pool.binding_id,
        Some(existing.binding_id.to_string())
    );
    assert_eq!(existing_pool.backend_name, existing.backend_name);
    assert_eq!(existing_pool.authority_id, existing.authority_id);
    assert_eq!(existing_pool.remote_pool, existing.remote_pool);
    assert_eq!(existing_pool.provider_pool_id, None);
    assert_eq!(existing_pool.accepted_revision, None);
    assert_eq!(existing_pool.stats_message, None);
    assert!(
        existing_pool
            .stats
            .as_ref()
            .expect("bound pool counts")
            .free
            > 0
    );
    assert!(existing_pool.observed_at.is_some());

    // A saved local binding with no inventory is empty, rather than unknown.
    let empty_pool = pools
        .iter()
        .find(|pool| pool.name == empty.pool_name)
        .expect("empty bound pool");
    assert_eq!(empty_pool.binding_id, Some(empty.binding_id.to_string()));
    assert_eq!(empty_pool.backend_name.as_deref(), Some("local-alias"));
    assert_eq!(
        empty_pool.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    assert_eq!(empty_pool.stats, Some(forge::ResourcePoolStats::default()));
    assert!(empty_pool.observed_at.is_some());
    assert_eq!(empty_pool.provider_pool_id, None);
    assert_eq!(empty_pool.accepted_revision, None);
    assert_eq!(
        (empty_pool.min.as_ref(), empty_pool.max.as_ref()),
        (None, None)
    );

    let old = env
        .api
        .admin_list_resource_pools(Request::new(forge::ListResourcePoolsRequest::default()))
        .await?
        .into_inner();
    assert_eq!(
        old.pools.iter().find(|pool| pool.name == pkey_name),
        Some(&forge::ResourcePool {
            name: pkey_name,
            min: "10".into(),
            max: "11".into(),
            total: 2,
            allocated: 0,
        })
    );
    assert_eq!(
        old.pools
            .iter()
            .find(|pool| pool.name == "inspection-prefix"),
        Some(&forge::ResourcePool {
            name: "inspection-prefix".into(),
            min: "2001:db8::/64".into(),
            max: "2001:db8:17::/64".into(),
            total: 11,
            allocated: 4,
        })
    );
    Ok(())
}

/// Provider errors stay on their own rows, and unknown or overflowing capacity
/// cannot become zero in either inspection or the older numeric-only RPC.
#[crate::sqlx_test]
async fn inspection_keeps_remote_status_and_saved_identity(database: PgPool) -> eyre::Result<()> {
    let healthy = v1::PoolStats {
        used: 7,
        free: 11,
        auto_assign_used: 3,
        auto_assign_free: 5,
        non_auto_assign_used: 4,
        non_auto_assign_free: 6,
    };
    let cases = [
        // A successful response preserves all six counts, not just capacity.
        (
            "remote-healthy",
            Ok(healthy),
            forge::ResourcePoolStatsStatus::Known,
        ),
        // Unsupported statistics are different from an empty provider pool.
        (
            "remote-unknown",
            Err(Code::Unimplemented),
            forge::ResourcePoolStatsStatus::Unknown,
        ),
        // A failed observation does not hide healthy pools from the same response.
        (
            "remote-unavailable",
            Err(Code::Unavailable),
            forge::ResourcePoolStatsStatus::Unavailable,
        ),
        // Valid individual counts can still overflow when a client adds capacity.
        (
            "remote-overflow",
            Ok(v1::PoolStats {
                used: u64::MAX,
                free: 1,
                auto_assign_used: u64::MAX,
                auto_assign_free: 1,
                non_auto_assign_used: 0,
                non_auto_assign_free: 0,
            }),
            forge::ResourcePoolStatsStatus::Unavailable,
        ),
    ];
    let requests = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = StatsProvider {
        replies: Arc::new(Mutex::new(
            cases
                .iter()
                .map(|(name, reply, _)| ((*name).to_owned(), StatsReply::Ready(*reply)))
                .collect(),
        )),
        requests: requests.clone(),
        calls: calls.clone(),
    };
    let shutdown = CancellationToken::new();
    let _shutdown_guard = shutdown.clone().drop_guard();
    let mut tasks = JoinSet::new();
    let (endpoint, _) = start_stats_provider(provider, &mut tasks, shutdown.clone()).await?;

    // The fixture starts with only Integrated selections. Saved remote bindings
    // are injected afterward; production startup still rejects such selections.
    let mut config = default_config::get();
    config.resource_pool_backends.insert(
        "inspection-provider".into(),
        ResourcePoolBackendConfig::Grpc {
            endpoint,
            tls: None,
        },
    );
    let env =
        create_test_env_with_overrides(database.clone(), TestEnvOverrides::with_config(config))
            .await;
    let mut expected_requests = Vec::new();
    for (name, _, _) in &cases {
        expected_requests
            .push(save_remote_definition(&database, name, "inspection-provider").await?);
    }
    // A bad saved definition must not be replayed or queried at the provider.
    let corrupt =
        save_remote_definition(&database, "remote-corrupt", "inspection-provider").await?;
    sqlx::query("UPDATE resource_pool_registration SET request = $1 WHERE binding_id = $2")
        .bind(vec![0xff_u8])
        .bind(
            corrupt
                .pool
                .as_ref()
                .expect("corrupt row identity")
                .binding_id
                .parse::<Uuid>()?,
        )
        .execute(&database)
        .await?;
    // Removing backend configuration keeps readable identity visible but cannot
    // cause a connection to a different configured provider.
    let missing =
        save_remote_definition(&database, "remote-unconfigured", "removed-provider").await?;
    let original_pools = pool_database_snapshot(&database).await?;
    let registration_query = "SELECT to_jsonb(resource_pool_registration) FROM resource_pool_registration ORDER BY binding_id, revision";
    let original_registrations: Vec<serde_json::Value> = sqlx::query_scalar(registration_query)
        .fetch_all(&database)
        .await?;
    let requested_at = Utc::now();
    let pools = env
        .api
        .admin_inspect_resource_pools(Request::new(forge::InspectResourcePoolsRequest {}))
        .await?
        .into_inner()
        .pools;
    assert!(pools.windows(2).all(|pair| pair[0].name < pair[1].name));
    for (name, _, status) in cases {
        let pool = pools.iter().find(|pool| pool.name == name).expect(name);
        let request = expected_requests
            .iter()
            .find(|request| request.pool.as_ref().expect("pool").logical_pool == name)
            .expect("saved request");
        let identity = request.pool.as_ref().expect("saved pool identity");
        assert_eq!(
            pool.backend_kind(),
            forge::ResourcePoolBackendKind::Grpc,
            "{name}"
        );
        assert_eq!(
            pool.backend_name.as_deref(),
            Some("inspection-provider"),
            "{name}"
        );
        assert_eq!(
            pool.binding_id.as_deref(),
            Some(identity.binding_id.as_str()),
            "{name}"
        );
        assert_eq!(pool.authority_id, identity.authority_id, "{name}");
        assert_eq!(pool.remote_pool, format!("alias/{name}"), "{name}");
        assert_eq!(
            pool.provider_pool_id.as_deref(),
            Some(identity.pool_id.as_str()),
            "{name}"
        );
        assert_eq!(pool.accepted_revision.as_deref(), Some("1"), "{name}");
        assert_eq!(
            (pool.min.as_ref(), pool.max.as_ref()),
            (None, None),
            "{name}"
        );
        assert_eq!(pool.stats_status(), status, "{name}");
        if status == forge::ResourcePoolStatsStatus::Known {
            assert_eq!(
                pool.stats,
                Some(forge::ResourcePoolStats {
                    used: healthy.used,
                    free: healthy.free,
                    auto_assign_used: healthy.auto_assign_used,
                    auto_assign_free: healthy.auto_assign_free,
                    non_auto_assign_used: healthy.non_auto_assign_used,
                    non_auto_assign_free: healthy.non_auto_assign_free,
                })
            );
            assert_eq!(pool.stats_message, None);
            let observed_at = pool
                .observed_at
                .as_ref()
                .expect("successful observation time");
            assert!(observed_at.seconds >= requested_at.timestamp());
            assert!(observed_at.seconds <= Utc::now().timestamp());
        } else {
            assert_eq!(pool.stats, None, "{name}");
            assert_eq!(pool.observed_at, None, "{name}");
            let message = pool.stats_message.as_ref().expect("failure explanation");
            assert!(!message.is_empty(), "{name}");
            assert!(!message.contains("provider-private-diagnostic"), "{name}");
        }
    }
    for (request, backend_name, readable) in [
        // The saved binding stays visible even when registration bytes cannot decode.
        (&corrupt, "inspection-provider", false),
        // Missing connection settings do not erase a readable provider identity.
        (&missing, "removed-provider", true),
    ] {
        let identity = request.pool.as_ref().expect("saved identity");
        let pool = pools
            .iter()
            .find(|pool| pool.name == identity.logical_pool)
            .expect("unavailable remote pool");
        assert_eq!(pool.backend_kind(), forge::ResourcePoolBackendKind::Grpc);
        assert_eq!(
            pool.stats_status(),
            forge::ResourcePoolStatsStatus::Unavailable
        );
        assert_eq!(pool.stats, None);
        assert_eq!(pool.observed_at, None);
        assert_eq!(pool.backend_name.as_deref(), Some(backend_name));
        assert_eq!(
            pool.binding_id.as_deref(),
            Some(identity.binding_id.as_str())
        );
        assert_eq!(pool.authority_id, identity.authority_id);
        assert_eq!(pool.remote_pool, format!("alias/{}", identity.logical_pool));
        assert_eq!(
            pool.provider_pool_id.as_deref(),
            readable.then_some(identity.pool_id.as_str())
        );
        assert_eq!(
            pool.accepted_revision.as_deref(),
            readable.then_some(request.eligibility_revision.as_str())
        );
        assert!(
            pool.stats_message
                .as_ref()
                .is_some_and(|message| !message.is_empty())
        );
    }
    let mut actual_requests = requests.lock().expect("recorded requests").clone();
    actual_requests
        .sort_by_key(|request| request.pool.as_ref().expect("pool").logical_pool.clone());
    expected_requests
        .sort_by_key(|request| request.pool.as_ref().expect("pool").logical_pool.clone());
    assert_eq!(actual_requests, expected_requests);

    let error = env
        .api
        .admin_list_resource_pools(Request::new(forge::ListResourcePoolsRequest::default()))
        .await
        .expect_err("numeric-only listing cannot describe remote pools");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error
            .metadata()
            .get("nico-error-code")
            .expect("API error code"),
        "NICO-API-412"
    );
    assert!(error.message().contains("AdminInspectResourcePools"));
    assert_eq!(
        *calls.lock().expect("recorded RPC calls"),
        vec!["Stats"; expected_requests.len()]
    );
    assert_eq!(pool_database_snapshot(&database).await?, original_pools);
    let registrations: Vec<serde_json::Value> = sqlx::query_scalar(registration_query)
        .fetch_all(&database)
        .await?;
    assert_eq!(registrations, original_registrations);
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), tasks.join_next())
        .await?
        .expect("provider task")??;
    Ok(())
}

/// A failed database snapshot hides counts without discarding the last sample.
/// Shutdown also stops waiting for SQL, and PostgreSQL returns the connection
/// after its statement limit expires.
#[crate::sqlx_test]
async fn polling_retains_samples_after_database_failure_and_cancels_sql(
    database: PgPool,
) -> eyre::Result<()> {
    let env = create_test_env(database.clone()).await;
    // A shorter operator limit makes PostgreSQL fail before the sampler's
    // five-second budget. One connection also exposes delayed SQL cleanup.
    let stats_database = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '1s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(database.connect_options().as_ref().clone())
        .await?;
    let monitor = ResourcePoolMonitor::new(stats_database.clone(), &HashMap::new())?;
    let metrics = MetricsCapture::start();
    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let mut tasks = JoinSet::new();
    let observations = start_pool_stats(monitor, &env.common_pools, &mut tasks, cancel.clone())?;
    start_export_service_health_metrics(ServiceHealthContext {
        meter: env.test_meter.meter(),
        database_pool: stats_database.clone(),
        resource_pool_stats: observations.clone(),
    });
    let first = wait_for_pool_observation(
        &observations,
        VPC_VNI,
        forge::ResourcePoolStatsStatus::Known,
    )
    .await?
    .expect("first successful database observation");

    let mut blocker = database.begin().await?;
    sqlx::query("LOCK TABLE resource_pool IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    advance_stats_poll().await;
    wait_for_blocked_query(&database, blocker_pid, "FROM resource_pool").await;

    assert_eq!(
        wait_for_pool_observation(
            &observations,
            VPC_VNI,
            forge::ResourcePoolStatsStatus::Unavailable,
        )
        .await?,
        Some(first.clone()),
        "a failed snapshot must retain both counts and their original timestamp"
    );
    assert!(
        observations
            .lock()
            .expect("pool observations")
            .values()
            .all(|sample| sample.latest_status == forge::ResourcePoolStatsStatus::Unavailable)
    );
    for metric in [
        "carbide_resourcepool_used_count",
        "carbide_resourcepool_free_count",
    ] {
        assert!(
            env.test_meter.formatted_metrics(metric).is_empty(),
            "{metric}"
        );
    }
    assert!(
        metrics.counter_delta(
            "carbide_resourcepool_stats_poll_failures_total",
            &[("backend", "integrated"), ("failure", "database")],
        ) >= 1.0
    );
    blocker.rollback().await?;

    // Removing the lock must allow a later scheduled pass to replace the sample.
    advance_stats_poll().await;
    let recovered = wait_for_pool_observation(
        &observations,
        VPC_VNI,
        forge::ResourcePoolStatsStatus::Known,
    )
    .await?
    .expect("database observations recover after the lock is removed");
    assert_eq!(recovered.stats, first.stats);
    assert_ne!(recovered.observed_at, first.observed_at);

    let mut blocker = database.begin().await?;
    sqlx::query("LOCK TABLE resource_pool IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    advance_stats_poll().await;
    wait_for_blocked_query(&database, blocker_pid, "FROM resource_pool").await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(500), async {
        while let Some(result) = tasks.join_next().await {
            result.expect("statistics sampler stops without panicking");
        }
    })
    .await?;

    // Dropping the future stops local waiting. PostgreSQL can still be running
    // the query until its statement timeout releases the only pooled connection.
    let mut connection =
        tokio::time::timeout(Duration::from_secs(2), stats_database.acquire()).await??;
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *connection)
            .await?,
        1
    );
    drop(connection);
    blocker.rollback().await?;
    stats_database.close().await;
    Ok(())
}

/// A slow provider must not hold up healthy counts. Failed observations keep
/// their last successful sample, and shutdown stops an RPC already in flight.
#[crate::sqlx_test]
async fn polling_keeps_healthy_pools_moving_and_stops_active_calls(
    database: PgPool,
) -> eyre::Result<()> {
    let counts = v1::PoolStats {
        used: 7,
        free: 11,
        auto_assign_used: 7,
        auto_assign_free: 11,
        non_auto_assign_used: 0,
        non_auto_assign_free: 0,
    };
    let timed_out_call = Arc::new(StalledStats::default());
    let replies = Arc::new(Mutex::new(HashMap::from([
        ("poll-healthy".into(), StatsReply::Ready(Ok(counts))),
        (
            "a-poll-stalled".into(),
            StatsReply::Stall(timed_out_call.clone()),
        ),
    ])));
    let provider = StatsProvider {
        replies: replies.clone(),
        requests: Arc::default(),
        calls: Arc::default(),
    };
    let requests = provider.requests.clone();
    let shutdown = CancellationToken::new();
    let _shutdown_guard = shutdown.clone().drop_guard();
    let mut provider_tasks = JoinSet::new();
    let (endpoint, connections) =
        start_stats_provider(provider, &mut provider_tasks, shutdown.clone()).await?;

    let mut config = default_config::get();
    config.resource_pool_backends.insert(
        "poll-provider".into(),
        ResourcePoolBackendConfig::Grpc {
            endpoint,
            tls: None,
        },
    );
    let env =
        create_test_env_with_overrides(database.clone(), TestEnvOverrides::with_config(config))
            .await;
    // Retained local inventory must not become a remote pool's apparent capacity
    // while its provider is stalled or unavailable.
    let mut txn = database.begin().await?;
    db::resource_pool::populate(
        &TypedPool::<i64>::new("a-poll-stalled".into(), ValueType::Integer),
        &mut txn,
        vec![999],
        true,
    )
    .await?;
    txn.commit().await?;
    // Add remote bindings after fixture startup, below the production gate
    // that still requires Integrated selections.
    for name in ["poll-healthy", "a-poll-stalled"] {
        save_remote_definition(&database, name, "poll-provider").await?;
    }
    let missing_pkey = ib_pkey_pool_name("missing");
    let common_pools = db::resource_pool::create_common_pools(
        database.clone(),
        ["default".to_owned(), "missing".to_owned()].into(),
    )
    .await?;
    let local_counts = db::resource_pool::stats(&database, VPC_VNI).await?;
    assert!(local_counts.free > 0);
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM resource_pool WHERE name = ANY($1))"
        )
        .bind(vec![LOOPBACK_IP_V6, missing_pkey.as_str()])
        .fetch_one(&database)
        .await?,
        "both optional local pools must be absent from inventory"
    );
    let metrics = MetricsCapture::start();
    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let mut tasks = JoinSet::new();
    let observations = start_pool_stats(
        env.api.resource_pool_monitor.clone(),
        &common_pools,
        &mut tasks,
        cancel.clone(),
    )?;

    // The stalled name sorts first. Healthy counts must arrive before its
    // ten-second deadline, so serial polling cannot satisfy this assertion.
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        timed_out_call.entered.cancelled().await;
        wait_for_pool_observation(
            &observations,
            "poll-healthy",
            forge::ResourcePoolStatsStatus::Known,
        )
        .await
    })
    .await??
    .expect("healthy pool has a successful sample");
    assert_eq!(first.stats.as_ref().expect("healthy counts").used, 7);
    assert!(first.observed_at.is_some());
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    // Real local counts replace empty defaults, while both absent Ethernet
    // and configured IB pools retain their known zero counts.
    assert_eq!(
        wait_for_pool_observation(
            &observations,
            VPC_VNI,
            forge::ResourcePoolStatsStatus::Known,
        )
        .await?
        .expect("local inventory has known counts")
        .stats,
        Some(local_counts.into())
    );
    for name in [LOOPBACK_IP_V6, missing_pkey.as_str()] {
        assert_eq!(
            wait_for_pool_observation(&observations, name, forge::ResourcePoolStatsStatus::Known)
                .await?
                .expect("missing optional local pool has known counts")
                .stats,
            Some(forge::ResourcePoolStats::default()),
            "{name}"
        );
    }

    assert!(!timed_out_call.stopped.is_cancelled());

    // The healthy observations are complete and the stalled call has entered
    // the provider. Advance its deadline without racing real connection setup.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();

    assert_eq!(
        wait_for_pool_observation(
            &observations,
            "a-poll-stalled",
            forge::ResourcePoolStatsStatus::Unavailable,
        )
        .await?,
        None,
        "a failed remote observation must not retain same-name local inventory"
    );
    tokio::time::timeout(Duration::from_secs(1), timed_out_call.stopped.cancelled()).await?;

    // Unsupported Stats and temporary failures retain the same successful
    // sample, but operators need their different status and failure labels.
    replies
        .lock()
        .expect("provider replies")
        .insert("a-poll-stalled".into(), StatsReply::Ready(Ok(counts)));
    let mut other_sample = None;
    for (code, status, failure) in [
        // A provider without Stats support reports unknown capacity.
        (
            Code::Unimplemented,
            forge::ResourcePoolStatsStatus::Unknown,
            "unknown",
        ),
        // A failed Stats attempt reports unavailable capacity.
        (
            Code::Unavailable,
            forge::ResourcePoolStatsStatus::Unavailable,
            "unavailable",
        ),
    ] {
        let labels = [("backend", "grpc"), ("failure", failure)];
        let before =
            metrics.counter_delta("carbide_resourcepool_stats_poll_failures_total", &labels);
        replies
            .lock()
            .expect("provider replies")
            .insert("poll-healthy".into(), StatsReply::Ready(Err(code)));
        advance_stats_poll().await;
        assert_eq!(
            wait_for_pool_observation(&observations, "poll-healthy", status).await?,
            Some(first.clone()),
            "{code:?}: retain the successful observation unchanged"
        );
        assert!(
            metrics.counter_delta("carbide_resourcepool_stats_poll_failures_total", &labels)
                > before,
            "{code:?}: the poller must count the matching failure category"
        );
        // Wait for both rows from this pass. Advancing the next interval while
        // the other RPC is still finishing would also advance its deadline.
        other_sample = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let sample = wait_for_pool_observation(
                    &observations,
                    "a-poll-stalled",
                    forge::ResourcePoolStatsStatus::Known,
                )
                .await?;
                if sample != other_sample {
                    return Ok::<_, eyre::Report>(sample);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
    }
    // Advancing time can expire HTTP/2 keepalive and reconnect the shared
    // channel. With this poll finished, inspection should reuse its connection.
    let connections_before_inspection = connections.load(Ordering::SeqCst);
    // Inspection must fetch new Stats even when the sampler has cached rows.
    let requests_before_inspection = requests.lock().expect("recorded requests").len();
    let inspected = env
        .api
        .admin_inspect_resource_pools(Request::new(forge::InspectResourcePoolsRequest {}))
        .await?
        .into_inner()
        .pools
        .into_iter()
        .find(|pool| pool.name == "a-poll-stalled")
        .expect("recovered provider pool");
    assert_eq!(
        requests.lock().expect("recorded requests").len(),
        requests_before_inspection + 2,
        "inspection must request fresh statistics for both remote pools"
    );
    assert_eq!(
        inspected.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    let inspected_counts = inspected.stats.expect("fresh provider counts");
    assert_eq!((inspected_counts.used, inspected_counts.free), (7, 11));
    assert_eq!(
        connections.load(Ordering::SeqCst),
        connections_before_inspection,
        "public inspection must reuse the poller's current provider connection"
    );

    // A successful retry replaces the retained sample. Stall the other pool
    // again so shutdown must stop provider I/O, not just an idle interval.
    let cancelled_call = Arc::new(StalledStats::default());
    {
        let mut replies = replies.lock().expect("provider replies");
        replies.insert(
            "poll-healthy".into(),
            StatsReply::Ready(Ok(v1::PoolStats {
                used: 8,
                free: 10,
                auto_assign_used: 8,
                auto_assign_free: 10,
                ..counts
            })),
        );
        replies.insert(
            "a-poll-stalled".into(),
            StatsReply::Stall(cancelled_call.clone()),
        );
    }
    advance_stats_poll().await;
    let recovered = wait_for_pool_observation(
        &observations,
        "poll-healthy",
        forge::ResourcePoolStatsStatus::Known,
    )
    .await?
    .expect("successful retry has a sample");
    assert_eq!(recovered.stats.as_ref().expect("retry counts").used, 8);
    assert_ne!(recovered.observed_at, first.observed_at);

    tokio::time::timeout(Duration::from_secs(5), cancelled_call.entered.cancelled()).await?;
    assert!(!cancelled_call.stopped.is_cancelled());
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(result) = tasks.join_next().await {
            result.expect("statistics sampler stops without panicking");
        }
        cancelled_call.stopped.cancelled().await;
    })
    .await?;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), provider_tasks.join_next())
        .await?
        .expect("provider task")??;
    Ok(())
}

/// The sampler admits at most eight pool observations at once and reuses a
/// released slot. Once the pass finishes, cancellation also stops its tick wait.
#[crate::sqlx_test]
async fn polling_bounds_active_calls_and_cancels_an_idle_sampler(
    database: PgPool,
) -> eyre::Result<()> {
    let calls: Vec<_> = (0..9).map(|_| Arc::new(StalledStats::default())).collect();
    let provider = StatsProvider {
        replies: Arc::new(Mutex::new(
            calls
                .iter()
                .enumerate()
                .map(|(index, call)| (format!("bounded-{index}"), StatsReply::Stall(call.clone())))
                .collect(),
        )),
        requests: Arc::default(),
        calls: Arc::default(),
    };
    let shutdown = CancellationToken::new();
    let _shutdown_guard = shutdown.clone().drop_guard();
    let mut provider_tasks = JoinSet::new();
    let (endpoint, _) =
        start_stats_provider(provider, &mut provider_tasks, shutdown.clone()).await?;
    let mut config = default_config::get();
    config.resource_pool_backends.insert(
        "bounded-provider".into(),
        ResourcePoolBackendConfig::Grpc {
            endpoint,
            tls: None,
        },
    );
    let env =
        create_test_env_with_overrides(database.clone(), TestEnvOverrides::with_config(config))
            .await;
    for index in 0..calls.len() {
        save_remote_definition(&database, &format!("bounded-{index}"), "bounded-provider").await?;
    }
    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let mut tasks = JoinSet::new();
    let observations = start_pool_stats(
        env.api.resource_pool_monitor.clone(),
        &env.common_pools,
        &mut tasks,
        cancel.clone(),
    )?;

    tokio::time::timeout(Duration::from_secs(5), async {
        for call in &calls[..8] {
            call.entered.cancelled().await;
        }
    })
    .await?;
    // Keep all eight RPCs blocked while waiting for an unwanted ninth entrant.
    // This observation window ends well before the client's ten-second limit.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), calls[8].entered.cancelled())
            .await
            .is_err()
    );
    assert!(calls[..8].iter().all(|call| !call.stopped.is_cancelled()));
    calls[0].release.cancel();
    tokio::time::timeout(Duration::from_secs(2), calls[8].entered.cancelled()).await?;
    assert!(calls[0].stopped.is_cancelled());
    assert!(calls[1..].iter().all(|call| !call.stopped.is_cancelled()));

    for call in &calls {
        call.release.cancel();
    }
    for index in 0..calls.len() {
        wait_for_pool_observation(
            &observations,
            &format!("bounded-{index}"),
            forge::ResourcePoolStatsStatus::Unavailable,
        )
        .await?;
    }
    // Every response has been published; stopping here protects cancellation
    // during the sixty-second wait separately from active-RPC cancellation.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(result) = tasks.join_next().await {
            result.expect("idle statistics sampler stops without panicking");
        }
    })
    .await?;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), provider_tasks.join_next())
        .await?
        .expect("provider task")??;
    Ok(())
}

/// Wait for the sampler to publish a status, then return its retained success.
/// Each read releases the mutex before waiting for another attempt.
async fn wait_for_pool_observation(
    observations: &PoolStats,
    name: &str,
    status: forge::ResourcePoolStatsStatus,
) -> eyre::Result<Option<forge::ResourcePoolInspection>> {
    Ok(tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            {
                let observations = observations.lock().expect("pool observations");
                if let Some(observation) = observations.get(name)
                    && observation.latest_status == status
                {
                    let mut sample = None;
                    observation
                        .last_success
                        .if_available(|last, _| sample = Some(last.clone()));
                    return sample;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?)
}

/// Advance the polling interval, then resume real time before waiting on SQL
/// or gRPC. This does not age the metrics holder's `std::time::Instant`.
async fn advance_stats_poll() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
}

/// Start the shared fixture on a real listener and count accepted connections.
/// The caller owns cancellation and joins the server through its task set.
async fn start_stats_provider(
    provider: StatsProvider,
    tasks: &mut JoinSet<Result<(), tonic::transport::Error>>,
    cancel: CancellationToken,
) -> eyre::Result<(url::Url, Arc<AtomicUsize>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?).parse()?;
    let connections = Arc::new(AtomicUsize::new(0));
    let accepted = connections.clone();
    let incoming = TcpListenerStream::new(listener).inspect(move |connection| {
        if connection.is_ok() {
            accepted.fetch_add(1, Ordering::SeqCst);
        }
    });
    tasks.spawn(
        Server::builder()
            .add_service(ResourcePoolServer::new(provider))
            .serve_with_incoming_shutdown(incoming, cancel.cancelled_owned()),
    );
    Ok((endpoint, connections))
}

/// Save an accepted registration without adding local inventory, so inspection uses
/// its provider identity, rather than resolving the configured alias again.
async fn save_remote_definition(
    database: &PgPool,
    name: &str,
    backend_name: &str,
) -> eyre::Result<v1::StatsRequest> {
    let mut txn = database.begin().await?;
    let binding = db::resource_pool::binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: name.into(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some(backend_name.into()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "inspection-authority".into(),
            remote_pool: format!("alias/{name}"),
        },
    )
    .await?;
    let definition = v1::DefineRequest {
        pool: Some(v1::PoolBinding {
            authority_id: binding.authority_id,
            pool_id: format!("canonical/{name}"),
            binding_id: binding.binding_id.to_string(),
            logical_pool: name.into(),
        }),
        eligibility_revision: "1".into(),
        previous_revision: None,
        value_domain: v1::ValueDomain::Integer as i32,
        eligibility: vec![v1::Eligibility {
            assignment_mode: v1::AssignmentMode::Automatic as i32,
            entries: Some(v1::eligibility::Entries::Range(v1::ValueRange {
                start: Some(1_i64.into()),
                end: Some(1000_i64.into()),
            })),
        }],
    };
    db::resource_pool::registration::insert(
        &mut txn,
        &ResourcePoolRegistration {
            binding_id: binding.binding_id,
            revision: 1,
            request: definition.encode_to_vec(),
            accepted: true,
        },
    )
    .await?;
    txn.commit().await?;
    Ok(v1::StatsRequest {
        pool: definition.pool,
        eligibility_revision: definition.eligibility_revision,
    })
}

/// `StatsProvider` records real gRPC requests and supplies each pool's observation.
/// Other operations fail so inspection cannot silently register or allocate values.
struct StatsProvider {
    replies: Arc<Mutex<HashMap<String, StatsReply>>>,
    requests: Arc<Mutex<Vec<v1::StatsRequest>>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

/// A pool can answer immediately or wait until the test releases its RPC or the
/// client stops it.
#[derive(Clone)]
enum StatsReply {
    Ready(Result<v1::PoolStats, Code>),
    Stall(Arc<StalledStats>),
}

/// Records when a stalled RPC starts and stops, and lets the test release it.
#[derive(Default)]
struct StalledStats {
    entered: CancellationToken,
    release: CancellationToken,
    stopped: CancellationToken,
}

#[tonic::async_trait]
impl ResourcePool for StatsProvider {
    async fn describe(
        &self,
        _: Request<v1::DescribeRequest>,
    ) -> Result<Response<v1::DescribeResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Describe");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn define(
        &self,
        _: Request<v1::DefineRequest>,
    ) -> Result<Response<v1::DefineResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Define");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn allocate(
        &self,
        _: Request<v1::AllocateRequest>,
    ) -> Result<Response<v1::AllocateResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Allocate");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn allocate_exact(
        &self,
        _: Request<v1::AllocateExactRequest>,
    ) -> Result<Response<v1::AllocateExactResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("AllocateExact");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn recover(
        &self,
        _: Request<v1::RecoverRequest>,
    ) -> Result<Response<v1::RecoverResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Recover");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn cancel(
        &self,
        _: Request<v1::CancelRequest>,
    ) -> Result<Response<v1::CancelResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Cancel");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn lookup(
        &self,
        _: Request<v1::LookupRequest>,
    ) -> Result<Response<v1::LookupResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Lookup");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn release(
        &self,
        _: Request<v1::ReleaseRequest>,
    ) -> Result<Response<v1::ReleaseResponse>, Status> {
        self.calls
            .lock()
            .expect("recorded RPC calls")
            .push("Release");
        Err(Status::unimplemented(
            "inspection must only request statistics",
        ))
    }
    async fn stats(
        &self,
        request: Request<v1::StatsRequest>,
    ) -> Result<Response<v1::StatsResponse>, Status> {
        let request = request.into_inner();
        self.calls.lock().expect("recorded RPC calls").push("Stats");
        self.requests
            .lock()
            .expect("recorded requests")
            .push(request.clone());
        let name = &request
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("missing pool"))?
            .logical_pool;
        let reply = self
            .replies
            .lock()
            .expect("provider replies")
            .get(name)
            .cloned();
        match reply {
            Some(StatsReply::Ready(Ok(stats))) => {
                Ok(Response::new(v1::StatsResponse { stats: Some(stats) }))
            }
            Some(StatsReply::Ready(Err(code))) => {
                Err(Status::new(code, "provider-private-diagnostic"))
            }
            Some(StatsReply::Stall(call)) => {
                let _stopped = call.stopped.clone().drop_guard();
                call.entered.cancel();
                call.release.cancelled().await;
                Err(Status::unavailable("test released the stalled request"))
            }
            None => Err(Status::not_found("unconfigured test pool")),
        }
    }
}
