// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use db::resource_pool::{binding, registration as stored_registration};
use futures::StreamExt;
use prost::Message;

use super::*;

/// Startup retries the saved definition after losing its acknowledgement. Edited
/// seed declarations still report drift, and optional Stats cannot block readiness.
/// Accepted registrations still require Describe to identify the original pool.
/// Renaming the backend alias keeps inspection on the saved binding.
#[crate::sqlx_test]
async fn registration_restart_keeps_seed_and_pending_request(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::start(database).await?;
    // Isolate readiness failures to this test's pending VPC pool.
    fixture.selections.remove(EXTERNAL_VPC_VNI);
    fixture.definitions.remove(EXTERNAL_VPC_VNI);
    fixture
        .provider
        .lose_define_reply
        .store(true, Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await
        .expect_err("first Define acknowledgement is lost");
    assert!(
        matches!(
            error.downcast_ref::<registration::RegistrationError>(),
            Some(registration::RegistrationError::Provider(GrpcResourcePoolError::Rpc(status)))
                if status.code() == Code::Unavailable
        ),
        "{error:?}"
    );
    let pending_binding = binding::load(&fixture.env.pool, VPC_VNI)
        .await?
        .expect("required VPC pool binding");
    assert!(error.to_string().contains(VPC_VNI), "{error:#}");
    assert!(
        error
            .to_string()
            .contains(&pending_binding.binding_id.to_string()),
        "{error:#}"
    );
    let pending = stored_registration::latest(&fixture.env.pool, pending_binding.binding_id)
        .await?
        .expect("pending VPC pool registration");
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
        db::resource_pool::stats(&fixture.env.pool, VPC_VNI)
            .await?
            .free,
        0
    );
    assert!(
        db::resource_pool::create_common_pools(fixture.env.pool.clone(), Default::default())
            .await
            .expect_err("pending registration is not ready")
            .to_string()
            .contains("no accepted definition")
    );

    // Listen-only startup cannot finish a pending definition or claim readiness.
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
            .bind(VPC_VNI)
            .bind(EXTERNAL_VPC_VNI)
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
    fixture
        .provider
        .describe_changed_pool
        .store(true, Ordering::SeqCst);
    let error = fixture
        .initialize(PoolDefinitionSource::ListenOnly)
        .await
        .expect_err("Describe identifies a different pool");
    fixture
        .provider
        .describe_changed_pool
        .store(false, Ordering::SeqCst);
    assert!(
        format!("{error:#}").contains("provider differs from its accepted definition"),
        "{error:#}"
    );
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, pending.binding_id).await?,
        Some(accepted.clone())
    );

    // Restoring the provider lets listen-only startup read accepted definitions
    // without replaying Define or treating a changed seed as a new revision.
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    assert_eq!(fixture.provider.defines.load(Ordering::SeqCst), defines);
    assert_eq!(
        stored_registration::latest(&fixture.env.pool, pending.binding_id).await?,
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
    monitor
        .resolve_registry(
            PoolDefinitionSource::ListenOnly,
            &mut txn,
            &fixture.selections,
            &fixture.env.api.work_lock_manager_handle,
        )
        .await?;
    txn.commit().await?;
    let inspected = monitor
        .observe(&[])
        .await?
        .collect::<Vec<_>>()
        .await
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

/// The remote readiness check does not exempt an Integrated `vpc-vni` pool
/// from having local inventory, even when its saved binding is present.
#[crate::sqlx_test]
async fn integrated_vpc_readiness_requires_inventory(database: PgPool) -> eyre::Result<()> {
    let env = create_test_env_with_overrides(
        database,
        TestEnvOverrides::no_network_segments().with_fnn_config(None),
    )
    .await;
    let binding = binding::load(&env.pool, VPC_VNI)
        .await?
        .expect("Integrated VPC pool binding");
    assert_eq!(binding.backend_kind, ResourcePoolBackendKind::Integrated);
    let removed = sqlx::query("DELETE FROM resource_pool WHERE name = $1")
        .bind(VPC_VNI)
        .execute(&env.pool)
        .await?;
    assert!(removed.rows_affected() > 0);

    let error = db::resource_pool::create_common_pools(env.pool.clone(), Default::default())
        .await
        .expect_err("required Integrated inventory is absent");
    assert_eq!(
        error.to_string(),
        "resource pool 'vpc-vni' missing or full. edit config file and restart",
    );
    Ok(())
}

/// `start_recovery` shares one worker between both VPC pools and starts one
/// cleanup sampler. The worker finishes queued releases without direct calls
/// to `reconcile`, and the supplied cancellation token stops both tasks.
#[crate::sqlx_test]
async fn startup_joins_shared_recovery_and_cleanup_sampler(database: PgPool) -> eyre::Result<()> {
    let fixture = RemoteFixture::new(database).await?;
    let vpc_id = VpcId::new();
    fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("startup-recovery")
                .id(vpc_id)
                .network_virtualization_type(forge::VpcVirtualizationType::Flat as i32)
                .tonic_request(),
        )
        .await?;
    let attached = fixture.receipt(vpc_id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("VPC must have an assigned remote receipt: {attached:?}");
    };

    // Deletion queues the release while the fixture's recovery worker is
    // stopped. Only the worker started below can finish the provider cleanup.
    fixture
        .env
        .api
        .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
        .await?;
    let releasing = fixture.receipt(vpc_id).await?;
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );

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
            let receipt = fixture.receipt(vpc_id).await?;
            if receipt.state == AllocationState::Released {
                return Ok::<_, eyre::Report>(receipt);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(released.allocation_id, attached.allocation_id);
    assert_eq!(released.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 1);
    assert!(
        !fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
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
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), tasks.join_all()).await?;
    fixture.stop().await
}
