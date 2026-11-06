// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_resource_pool::grpc::GrpcResourcePoolError;
use carbide_resource_pool::{PoolDefinitionSource, registration};
use carbide_resource_pool_api::v1;
use carbide_resource_pool_api::v1::resource_pool_server::ResourcePool as ProviderResourcePool;
use carbide_utils::test_support::test_meter::TestMeter;
use db::ConditionalWrite;
use db::resource_pool::{binding, registration as stored_registration, remote};
use futures::StreamExt;
use model::resource_pool::allocation::{AllocationSource, AllocationState};
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::remote::AllocationResult;
use model::resource_pool::{OwnerType, ResourcePool, ResourcePoolDef, ResourcePoolType, ValueType};
use prost::Message;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::{Code, Request};
use uuid::Uuid;

use crate::resource_pool::ResourcePoolMonitor;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::resource_pool::{REMOTE_POOL, RemotePoolFixture, SECOND_REMOTE_POOL};
use crate::tests::resource_pool::pool_database_snapshot;

/// Startup retries the saved definition after losing its acknowledgement. Edited
/// seed declarations still report drift, and unavailable Stats cannot prevent it.
/// Accepted registrations still require Describe to identify the original pool.
/// Renaming the backend alias keeps inspection on the saved binding.
#[crate::sqlx_test]
async fn registration_restart_keeps_seed_and_pending_request(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start(database).await?;
    // Only one pool should require registration while its reply is withheld.
    fixture.selections.remove(SECOND_REMOTE_POOL);
    fixture.definitions.remove(SECOND_REMOTE_POOL);
    fixture
        .provider
        .lose_define_reply
        .store(true, Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await
        .expect_err("first Define acknowledgement is lost");
    // The error must leave a pending request even though the provider has
    // already accepted it. Reload both sides before attempting any recovery.
    assert!(
        matches!(
            error.downcast_ref::<registration::RegistrationError>(),
            Some(registration::RegistrationError::Provider(GrpcResourcePoolError::Rpc(status)))
                if status.code() == Code::Unavailable
        ),
        "{error:?}"
    );
    let pending_binding = binding::load(&fixture.env.pool, REMOTE_POOL)
        .await?
        .expect("test pool binding");
    assert!(error.to_string().contains(REMOTE_POOL), "{error:#}");
    assert!(
        error
            .to_string()
            .contains(&pending_binding.binding_id.to_string()),
        "{error:#}"
    );
    let pending = stored_registration::latest(&fixture.env.pool, pending_binding.binding_id)
        .await?
        .expect("pending test pool registration");
    assert!(!pending.accepted);
    assert_eq!(pending.revision, 1);
    let definition = v1::DefineRequest::decode(pending.request.as_slice())?;
    let provider_stats = fixture
        .provider
        .local
        .stats(Request::new(v1::StatsRequest {
            pool: definition.pool,
            eligibility_revision: definition.eligibility_revision,
        }))
        .await?
        .into_inner()
        .stats
        .expect("provider accepted the definition");
    assert_eq!((provider_stats.used, provider_stats.free), (0, 2));
    let seeded = db::resource_pool::all_stored_defs(&fixture.env.pool).await?;
    assert_eq!(seeded, fixture.definitions);
    assert_eq!(
        db::resource_pool::stats(&fixture.env.pool, REMOTE_POOL)
            .await?
            .free,
        0
    );

    // Listen-only startup cannot finish a pending definition or acknowledge it.
    let defines = fixture.provider.defines.load(Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::ListenOnly)
        .await
        .expect_err("listen-only startup needs an accepted definition");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("no accepted definition"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&pending_binding.binding_id.to_string()),
        "{diagnostic}"
    );
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, pending.binding_id).await?,
        Some(pending.clone())
    );
    assert!(
        registration::latest_accepted(&fixture.env.pool, &pending_binding)
            .await?
            .is_none()
    );

    // The next startup must finish the saved request, rather than interpreting
    // an edited TOML range as permission to grow the provider's definition.
    fixture
        .definitions
        .get_mut(&pending_binding.pool_name)
        .expect("configured pool")
        .ranges[0]
        .end = "90000".into();
    fixture
        .provider
        .lose_define_reply
        .store(false, Ordering::SeqCst);
    // Stats is optional for readiness; losing it must not block registration.
    fixture
        .provider
        .stats_unavailable
        .store(true, Ordering::SeqCst);
    let logs = fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    assert!(logs.iter().any(|log| {
        log.metadata_name == "config_definition_drifted"
            && log.field("name") == Some(pending_binding.pool_name.as_str())
            && log.field("drift_kind") == Some("changed")
    }));
    let accepted = stored_registration::latest(&fixture.env.pool, pending.binding_id)
        .await?
        .expect("accepted retry");
    assert!(accepted.accepted);
    assert_eq!(accepted.request, pending.request);
    assert_eq!(accepted.revision, pending.revision);
    assert_eq!(
        db::resource_pool::all_stored_defs(&fixture.env.pool).await?,
        seeded
    );
    let local_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource_pool WHERE name IN ($1, $2)")
            .bind(REMOTE_POOL)
            .bind(SECOND_REMOTE_POOL)
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(local_rows, 0);
    let definition = registration::latest_accepted(&fixture.env.pool, &pending_binding)
        .await?
        .expect("accepted definition");
    let error = fixture
        .remote(&pending_binding.pool_name)
        .provider()
        .stats(v1::StatsRequest {
            pool: definition.pool,
            eligibility_revision: definition.eligibility_revision,
        })
        .await
        .expect_err("fixture statistics are unavailable");
    assert!(
        matches!(error, GrpcResourcePoolError::Rpc(status) if status.code() == Code::Unavailable)
    );

    // A saved acknowledgement must not let startup adopt a replacement pool.
    // Reject the changed identity without changing or resending the definition.
    let defines = fixture.provider.defines.load(Ordering::SeqCst);
    let description = fixture
        .provider
        .local
        .describe(Request::new(v1::DescribeRequest {
            pool_alias: pending_binding.remote_pool.clone(),
        }))
        .await?
        .into_inner();
    let replacements = [
        // Another authority cannot take over an already accepted binding.
        (
            "authority",
            v1::DescribeResponse {
                authority_id: "replacement-authority".into(),
                ..description.clone()
            },
        ),
        // The same authority must still resolve the original provider pool.
        (
            "pool",
            v1::DescribeResponse {
                pool_id: "replacement-integers".into(),
                ..description.clone()
            },
        ),
        // A valid but different domain is still incompatible with saved values.
        (
            "domain",
            v1::DescribeResponse {
                value_domain: v1::ValueDomain::IpAddress as i32,
                ..description
            },
        ),
    ];
    for (changed, replacement) in replacements {
        *fixture
            .provider
            .describe_response
            .lock()
            .expect("Describe response") = Some(replacement);
        let error = fixture
            .initialize(PoolDefinitionSource::ListenOnly)
            .await
            .expect_err("Describe identifies a different pool");
        assert!(
            format!("{error:#}").contains("provider differs from its accepted definition"),
            "{changed}: {error:#}"
        );
        assert_eq!(
            fixture.provider.defines.load(Ordering::SeqCst),
            defines,
            "{changed}"
        );
        assert_eq!(
            stored_registration::latest(&fixture.env.pool, pending.binding_id).await?,
            Some(accepted.clone()),
            "{changed}"
        );
        assert_eq!(
            binding::load(&fixture.env.pool, REMOTE_POOL).await?,
            Some(pending_binding.clone()),
            "{changed}"
        );
    }
    *fixture
        .provider
        .describe_response
        .lock()
        .expect("Describe response") = None;

    // Pending growth must leave the accepted revision usable by listen-only
    // startup, without sending Define or acknowledging the newer request.
    let mut growth = v1::DefineRequest::decode(accepted.request.as_slice())?;
    growth.previous_revision = Some(growth.eligibility_revision.clone());
    growth.eligibility_revision = "2".into();
    growth.eligibility.push(v1::Eligibility {
        assignment_mode: v1::AssignmentMode::Automatic as i32,
        entries: Some(v1::eligibility::Entries::Value(20_003_i64.into())),
    });
    let pending_growth = ResourcePoolRegistration {
        binding_id: pending_binding.binding_id,
        revision: 2,
        request: growth.encode_to_vec(),
        accepted: false,
    };
    let mut txn = fixture.env.pool.begin().await?;
    stored_registration::lock_binding(&mut txn, pending_binding.binding_id).await?;
    stored_registration::insert(&mut txn, &pending_growth).await?;
    txn.commit().await?;

    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, pending.binding_id).await?,
        Some(pending_growth)
    );
    assert_eq!(
        stored_registration::latest_accepted(&fixture.env.pool, pending.binding_id).await?,
        Some(accepted)
    );

    // Renaming a connection leaves its saved binding intact. Inspection must
    // use the same resolved client as allocations, rather than the old alias.
    let mut config = fixture.env.config.as_ref().clone();
    let backend = config
        .resource_pool_backends
        .remove("fabric")
        .expect("original backend");
    config
        .resource_pool_backends
        .insert("renamed-fabric".into(), backend);
    fixture.env.config = Arc::new(config);
    for selection in fixture.selections.values_mut() {
        selection.backend_name = Some("renamed-fabric".into());
    }
    fixture
        .provider
        .stats_unavailable
        .store(false, Ordering::SeqCst);
    let mut monitor = ResourcePoolMonitor::new(
        fixture.env.pool.clone(),
        &fixture.env.config.resource_pool_backends,
    )?;
    let mut txn = fixture.env.pool.begin().await?;
    let registry = monitor
        .resolve_registry(
            PoolDefinitionSource::ListenOnly,
            &mut txn,
            &fixture.selections,
            &fixture.env.api.work_lock_manager_handle,
        )
        .await?;
    txn.commit().await?;
    // Production keeps this resolving monitor, so it must retain the client
    // even though the binding's original alias is no longer configured.
    let observed = monitor
        .observe(&[])
        .await?
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .find(|pool| pool.name == pending_binding.pool_name)
        .expect("renamed backend pool in the resolving monitor");
    assert_eq!(
        observed.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    // Rebuilding the API must copy the registry's resolved authority client.
    // Its saved alias no longer exists in the current configuration.
    fixture.env.api = Arc::new(
        TestApiBuilder::new(
            fixture.env.pool.clone(),
            fixture.env.common_pools.clone(),
            fixture.env.api.work_lock_manager_handle.clone(),
        )
        .with_resource_pool_registry(registry)
        .with_runtime_config(fixture.env.config.clone())
        .build(),
    );
    let inspected = fixture
        .env
        .api
        .admin_inspect_resource_pools(Request::new(forge::InspectResourcePoolsRequest {}))
        .await?
        .into_inner()
        .pools
        .into_iter()
        .find(|pool| pool.name == pending_binding.pool_name)
        .expect("renamed backend pool");
    assert_eq!(
        inspected.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    assert_eq!(inspected.backend_name.as_deref(), Some("fabric"));
    assert_eq!(inspected.binding_id, Some(pending.binding_id.to_string()));
    assert_eq!(
        binding::load(&fixture.env.pool, &pending_binding.pool_name).await?,
        Some(pending_binding),
    );
    fixture.stop().await
}

/// Registration needs a usable saved seed before it can contact the provider.
/// Missing or empty seeds leave the committed binding and inventory unchanged.
#[crate::sqlx_test]
async fn registration_rejects_missing_or_empty_saved_seed(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start(database).await?;
    fixture.selections.remove(SECOND_REMOTE_POOL);
    let mut monitor = ResourcePoolMonitor::new(
        fixture.env.pool.clone(),
        &fixture.env.config.resource_pool_backends,
    )?;
    // Save only the binding, as a partially restored database could contain.
    let mut txn = fixture.env.pool.begin().await?;
    let registry = monitor
        .resolve_registry(
            PoolDefinitionSource::SeedConfiguration,
            &mut txn,
            &fixture.selections,
            &fixture.env.api.work_lock_manager_handle,
        )
        .await?;
    txn.commit().await?;
    let saved_binding = binding::load(&fixture.env.pool, REMOTE_POOL)
        .await?
        .expect("saved binding");
    let cases = [
        // A missing seed cannot be reconstructed from edited startup config.
        (None, "no seed definition"),
        // This is valid stored JSON, but does not describe any allowed values.
        (
            Some(ResourcePoolDef {
                pool_type: ResourcePoolType::Integer,
                ranges: Vec::new(),
                prefix: None,
                delegate_prefix_len: None,
            }),
            "provide exactly one prefix or nonempty ranges",
        ),
    ];
    for (seed, expected_error) in cases {
        if let Some(seed) = &seed {
            let mut txn = fixture.env.pool.begin().await?;
            db::resource_pool::insert_pool_def(&mut txn, REMOTE_POOL, seed).await?;
            txn.commit().await?;
        }
        let before = pool_database_snapshot(&fixture.env.pool).await?;
        let error = monitor
            .register_pools(
                &registry,
                PoolDefinitionSource::SeedConfiguration,
                &fixture.cancel,
            )
            .await
            .expect_err("saved seed is required");
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains(expected_error), "{diagnostic}");
        assert!(diagnostic.contains(REMOTE_POOL), "{diagnostic}");
        assert!(
            diagnostic.contains(&saved_binding.binding_id.to_string()),
            "{diagnostic}"
        );
        assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);
        assert_eq!(
            db::resource_pool::stored_def(&fixture.env.pool, REMOTE_POOL).await?,
            seed
        );
        assert!(
            stored_registration::latest(&fixture.env.pool, saved_binding.binding_id)
                .await?
                .is_none()
        );
        assert_eq!(fixture.provider.describes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), 0);
    }
    fixture.stop().await
}

/// Removing a seed declaration keeps its saved provider connection usable.
/// Removing that connection too must fail instead of selecting another authority.
#[crate::sqlx_test]
async fn registration_retains_removed_pool_until_its_connection_is_missing(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::new(database).await?;
    let saved_binding = binding::load(&fixture.env.pool, REMOTE_POOL)
        .await?
        .expect("saved binding");
    let registration = stored_registration::latest(&fixture.env.pool, saved_binding.binding_id)
        .await?
        .expect("accepted definition");
    let before = pool_database_snapshot(&fixture.env.pool).await?;
    let defines = fixture.provider.defines.load(Ordering::SeqCst);
    let describes = fixture.provider.describes.load(Ordering::SeqCst);

    // The saved binding supplies the alias after its declaration disappears.
    // Reconciliation should report the omission, without deleting its seed.
    fixture.selections.clear();
    fixture.definitions.remove(REMOTE_POOL);
    let logs = fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    assert!(
        fixture.provider.describes.load(Ordering::SeqCst) > describes,
        "startup must still check the providers for retained bindings"
    );
    assert!(logs.iter().any(|log| {
        log.metadata_name == "config_definition_drifted"
            && log.field("name") == Some(REMOTE_POOL)
            && log.field("resource_kind") == Some("resource_pool")
            && log.field("drift_kind") == Some("dropped")
    }));
    let inspected = fixture
        .env
        .api
        .admin_inspect_resource_pools(Request::new(forge::InspectResourcePoolsRequest {}))
        .await?
        .into_inner()
        .pools
        .into_iter()
        .find(|pool| pool.name == REMOTE_POOL)
        .expect("retained pool");
    assert_eq!(
        inspected.stats_status(),
        forge::ResourcePoolStatsStatus::Known
    );
    assert_eq!(inspected.backend_name, saved_binding.backend_name);
    assert_eq!(
        inspected.binding_id,
        Some(saved_binding.binding_id.to_string())
    );
    assert_eq!(inspected.authority_id, saved_binding.authority_id);
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, saved_binding.binding_id).await?,
        Some(registration.clone())
    );
    let inventory_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource_pool WHERE name = $1")
            .bind(REMOTE_POOL)
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(inventory_rows, 0);

    // Leave an unrelated configured alias behind. It must not silently replace
    // the removed authority's connection, even if it has a usable endpoint.
    let mut config = fixture.env.config.as_ref().clone();
    let backend = config
        .resource_pool_backends
        .remove("fabric")
        .expect("saved alias");
    config
        .resource_pool_backends
        .insert("unrelated".into(), backend);
    fixture.env.config = Arc::new(config);
    let describes = fixture.provider.describes.load(Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::ListenOnly)
        .await
        .expect_err("retained alias is required");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("no configured gRPC backend matching alias"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains(REMOTE_POOL), "{diagnostic}");
    assert!(
        diagnostic.contains(&saved_binding.authority_id),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("fabric"), "{diagnostic}");
    assert_eq!(fixture.provider.describes.load(Ordering::SeqCst), describes);
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, saved_binding.binding_id).await?,
        Some(registration)
    );
    fixture.stop().await
}

/// A later pool's rejected definition must not undo earlier registrations.
/// Restart resumes the rejected request exactly, even if its configured seed changes.
#[crate::sqlx_test]
async fn registration_retries_only_the_rejected_pool_after_partial_startup(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start(database).await?;
    // Registry iteration is unordered, so fail the second observed call instead
    // of assuming one of the two pool names will be visited first.
    fixture
        .provider
        .reject_define_call
        .store(2, Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await
        .expect_err("second Define rejected");
    assert!(
        matches!(
            error.downcast_ref::<registration::RegistrationError>(),
            Some(registration::RegistrationError::Provider(GrpcResourcePoolError::Rpc(status)))
                if status.code() == Code::PermissionDenied
        ),
        "{error:#}"
    );
    let mut saved = Vec::new();
    for name in [REMOTE_POOL, SECOND_REMOTE_POOL] {
        let binding = binding::load(&fixture.env.pool, name)
            .await?
            .expect("committed binding");
        let registration = stored_registration::latest(&fixture.env.pool, binding.binding_id)
            .await?
            .expect("saved request");
        saved.push((binding, registration));
    }
    assert_eq!(saved.iter().filter(|(_, row)| row.accepted).count(), 1);
    let (rejected_binding, rejected) = saved
        .iter()
        .find(|(_, row)| !row.accepted)
        .expect("pending rejected request");
    assert!(
        error.to_string().contains(&rejected_binding.pool_name),
        "{error:#}"
    );
    assert!(
        error
            .to_string()
            .contains(&rejected_binding.binding_id.to_string()),
        "{error:#}"
    );
    assert_eq!(rejected.revision, 1);
    let before = pool_database_snapshot(&fixture.env.pool).await?;
    fixture
        .definitions
        .get_mut(&rejected_binding.pool_name)
        .expect("configured seed")
        .ranges[0]
        .end = "90000".into();

    // The provider rejected the request before accepting it. Retry must use
    // those saved bytes, while the already accepted pool needs no new Define.
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), 3);
    assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);
    let requests = fixture
        .provider
        .define_requests
        .lock()
        .expect("Define requests")
        .clone();
    assert_eq!(
        requests[1],
        v1::DefineRequest::decode(rejected.request.as_slice())?
    );
    assert_eq!(requests[2], requests[1]);
    for (binding, mut original) in saved {
        original.accepted = true;
        assert_eq!(
            stored_registration::latest(&fixture.env.pool, binding.binding_id).await?,
            Some(original.clone())
        );
        let request = v1::DefineRequest::decode(original.request.as_slice())?;
        fixture
            .provider
            .local
            .stats(Request::new(v1::StatsRequest {
                pool: request.pool,
                eligibility_revision: request.eligibility_revision,
            }))
            .await?;
    }
    fixture.stop().await
}

/// Cancelling startup after Define has taken effect leaves its request replayable.
/// A new monitor must acknowledge the same request without changing the seed.
#[crate::sqlx_test]
async fn registration_cancellation_preserves_provider_acceptance(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start(database).await?;
    fixture.selections.remove(SECOND_REMOTE_POOL);
    fixture.definitions.remove(SECOND_REMOTE_POOL);
    let mut monitor = ResourcePoolMonitor::new(
        fixture.env.pool.clone(),
        &fixture.env.config.resource_pool_backends,
    )?;
    let mut txn = fixture.env.pool.begin().await?;
    let registry = monitor
        .resolve_registry(
            PoolDefinitionSource::SeedConfiguration,
            &mut txn,
            &fixture.selections,
            &fixture.env.api.work_lock_manager_handle,
        )
        .await?;
    db::resource_pool::reconcile_pool_defs(&mut txn, &fixture.definitions).await?;
    txn.commit().await?;
    let before = pool_database_snapshot(&fixture.env.pool).await?;
    let binding = binding::load(&fixture.env.pool, REMOTE_POOL)
        .await?
        .expect("committed binding");

    // Cancel only the registration waiter. The provider stays running after it
    // accepts Define, so dropping the RPC cannot be mistaken for undoing it.
    fixture
        .provider
        .define_pause
        .pause_next
        .store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let mut registration = Box::pin(monitor.register_pools(
        &registry,
        PoolDefinitionSource::SeedConfiguration,
        &cancel,
    ));
    tokio::select! {
        result = &mut registration => panic!("Define passed its pause: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), fixture.provider.define_pause.entered.notified()) => { result?; }
    }
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), registration)
        .await?
        .expect_err("registration cancelled");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("resource pool registration cancelled"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&binding.binding_id.to_string()),
        "{diagnostic}"
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.provider.define_pause.dropped.cancelled(),
    )
    .await?;
    let pending = stored_registration::latest(&fixture.env.pool, binding.binding_id)
        .await?
        .expect("pending accepted request");
    assert!(!pending.accepted);
    assert_eq!(pending.revision, 1);
    let request = v1::DefineRequest::decode(pending.request.as_slice())?;
    fixture
        .provider
        .local
        .stats(Request::new(v1::StatsRequest {
            pool: request.pool,
            eligibility_revision: request.eligibility_revision,
        }))
        .await?;
    assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);

    // Reconstruct the monitor and replay before marking the stored request
    // accepted. The existing provider definition must remain usable.
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let mut accepted = pending;
    accepted.accepted = true;
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, binding.binding_id).await?,
        Some(accepted.clone())
    );
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), 2);
    let requests = fixture
        .provider
        .define_requests
        .lock()
        .expect("Define requests")
        .clone();
    assert_eq!(
        requests[0],
        v1::DefineRequest::decode(accepted.request.as_slice())?
    );
    assert_eq!(requests[1], requests[0]);
    assert_eq!(pool_database_snapshot(&fixture.env.pool).await?, before);
    fixture.stop().await
}

/// `start_recovery` shares one worker between two pools using the same provider
/// and starts one cleanup sampler. Only those tasks finish the queued release.
/// Cancellation stops an active release; restarting finishes that same request.
#[crate::sqlx_test]
async fn startup_joins_shared_recovery_and_cleanup_sampler(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::new(database).await?;
    // This owner exercises shared pool operations without a consumer handler.
    let owner_id = Uuid::new_v4().to_string();
    let pool = ResourcePool::<i32>::new(REMOTE_POOL.to_owned(), ValueType::Integer);
    let registry = &fixture.env.api.resource_pool_registry;
    let mut txn = fixture.env.pool.begin().await?;
    let selected = registry
        .select(&mut txn, &pool)
        .await?
        .expect("registered test pool");
    txn.commit().await?;
    let allocation = selected
        .prepare_allocation(
            &fixture.env.pool,
            OwnerType::Machine,
            &owner_id,
            "test-value",
            None,
            &fixture.cancel,
        )
        .await?;
    let mut txn = fixture.env.pool.begin().await?;
    assert_eq!(allocation.allocate(&mut txn).await?, 20_001);
    txn.commit().await?;
    let attached = remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::Machine,
        &owner_id,
        "test-value",
    )
    .await?
    .expect("attached allocation");
    assert_eq!(attached.state, AllocationState::Attached);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("test pool must save a remote allocation: {attached:?}");
    };

    // Commit the release while recovery is stopped. The test calls only the
    // shared pool interface; the worker below must finish the provider cleanup.
    let mut txn = fixture.env.pool.begin().await?;
    let owned = registry
        .find_owned(&pool, &mut txn, OwnerType::Machine, &owner_id, "test-value")
        .await?
        .expect("owner's attached allocation");
    assert!(matches!(
        owned.release(&mut txn).await?,
        ConditionalWrite::Applied(())
    ));
    txn.commit().await?;
    let releasing = remote::load(&fixture.env.pool, attached.allocation_id)
        .await?
        .expect("queued release");
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );

    // Start after committing the release, then interrupt the actual RPC before
    // the provider removes anything. Both API-owned tasks must join promptly.
    fixture
        .provider
        .release_pause
        .pause_next
        .store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let mut tasks = JoinSet::new();
    fixture.env.api.resource_pool_monitor.start_recovery(
        registry,
        fixture.env.test_meter.meter(),
        &mut tasks,
        cancel.clone(),
    )?;
    assert_eq!(tasks.len(), 2);
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.provider.release_pause.entered.notified(),
    )
    .await?;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), tasks.join_all()).await?;
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.provider.release_pause.dropped.cancelled(),
    )
    .await?;
    assert_eq!(
        remote::load(&fixture.env.pool, attached.allocation_id).await?,
        Some(releasing)
    );
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 1);

    // Rebuild the registry and monitor as a new API process would. A fresh
    // meter avoids retaining the stopped process's observable callback.
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture.env.test_meter = TestMeter::default();
    let cancel = CancellationToken::new();
    let mut tasks = JoinSet::new();
    fixture.env.api.resource_pool_monitor.start_recovery(
        &fixture.env.api.resource_pool_registry,
        fixture.env.test_meter.meter(),
        &mut tasks,
        cancel.clone(),
    )?;
    assert_eq!(tasks.len(), 2);
    let released = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let receipt = remote::load(&fixture.env.pool, attached.allocation_id)
                .await?
                .expect("saved allocation");
            if receipt.state == AllocationState::Released {
                return Ok::<_, eyre::Report>(receipt);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    // Cleanup is complete only after both the receipt and provider agree.
    assert_eq!(released.allocation_id, attached.allocation_id);
    assert_eq!(released.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 2);
    assert!(
        !fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    // The sampler must report the completed database state on its own cadence.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fixture
                .env
                .test_meter
                .formatted_metric("carbide_resource_pool_cleanup_pending")
                .as_deref()
                == Some("{fresh=\"true\"} 0")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    // Joining the restarted tasks also checks shutdown after a completed pass.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), tasks.join_all()).await?;
    fixture.stop().await
}
