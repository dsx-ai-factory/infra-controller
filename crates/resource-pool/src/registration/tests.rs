// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_resource_pool_api::v1;
use db::resource_pool::{binding, registration as storage};
use db::test_support::postgres::{wait_for_blocked_queries, wait_for_blocked_query};
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::{
    ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection, ResourcePoolValueDomain,
};
use prost::{Message, encoding};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tonic::Code;
use uuid::Uuid;

use super::{
    PreparedRegistration, RegistrationError, accepted, accepted_in_transaction, define,
    latest_accepted, prepare, resume,
};
use crate::grpc::test_server::{Fault, FaultProvider, Gate, TestServer, bounded, pool, provider};
use crate::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError, MAX_MESSAGE_BYTES};
use crate::transaction::DATABASE_WAIT_TIMEOUT;
use crate::value::ResourcePoolEligibility;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `Harness` keeps a saved binding, real gRPC server, and client together while
/// exposing persisted registration state independently of provider observations.
struct Harness {
    database: PgPool,
    binding: ResourcePoolBinding,
    server: TestServer,
    backend: GrpcResourcePoolBackend,
    provider: FaultProvider,
}

impl Harness {
    /// Save the binding without any pool definitions so registration must write
    /// its own request.
    async fn new(database: PgPool, fault: Fault) -> Self {
        let mut txn = database.begin().await.expect("begin binding transaction");
        let binding = binding::bind_or_verify(&mut txn, &selection())
            .await
            .expect("save pool binding");
        txn.commit().await.expect("commit pool binding");
        let provider = FaultProvider::new(provider(), fault);
        let server = TestServer::start(provider.clone()).await;
        let backend = GrpcResourcePoolBackend::new(server.endpoint.clone(), None)
            .expect("configure provider client");
        Self {
            database,
            binding,
            server,
            backend,
            provider,
        }
    }

    /// Reload the newest row so assertions observe committed database state.
    async fn latest(&self) -> ResourcePoolRegistration {
        storage::latest(&self.database, self.binding.binding_id)
            .await
            .expect("load latest registration")
            .expect("saved registration")
    }

    /// Snapshot complete provider requests, including calls whose replies failed.
    fn requests(&self) -> Vec<v1::DefineRequest> {
        self.provider
            .define_requests
            .lock()
            .expect("definition requests lock")
            .clone()
    }

    /// Drop the client and join the server before the test releases its fixture.
    async fn stop(self) {
        drop(self.backend);
        self.server.stop().await;
    }
}

/// Select the shared fixture's provider while leaving the binding UUID to Core.
fn selection() -> ResourcePoolSelection {
    ResourcePoolSelection {
        pool_name: pool().logical_pool,
        value_domain: ResourcePoolValueDomain::Integer,
        backend_name: Some("fabric".into()),
        backend_kind: ResourcePoolBackendKind::Grpc,
        authority_id: pool().authority_id,
        remote_pool: pool().pool_id,
    }
}

/// Build one compact declaration without expanding values into provider inventory.
fn range(start: i64, end: i64, mode: v1::AssignmentMode) -> v1::Eligibility {
    v1::Eligibility {
        assignment_mode: mode as i32,
        entries: Some(v1::eligibility::Entries::Range(v1::ValueRange {
            start: Some(start.into()),
            end: Some(end.into()),
        })),
    }
}

/// Keep an overlapping manual declaration first so reordering changes allocation policy.
fn eligibility() -> ResourcePoolEligibility {
    ResourcePoolEligibility {
        domain: ResourcePoolValueDomain::Integer,
        // The overlap makes order meaningful: the manual declaration wins.
        entries: vec![
            range(1, 2, v1::AssignmentMode::Manual),
            range(1, 5, v1::AssignmentMode::Automatic),
        ],
    }
}

/// Describe resolves the configured alias while leaving the binding lock and
/// only database connection available. The definition keeps the saved binding UUID.
#[carbide_macros::sqlx_test]
async fn describe_uses_the_alias_without_holding_the_binding_lock(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(database, Fault::StallDescription(gate.clone())).await;
    let single_connection = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(harness.database.connect_options().as_ref().clone())
        .await?;
    let eligibility = eligibility();
    let mut pending = Box::pin(define(
        &single_connection,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility,
    ));
    tokio::select! {
        result = &mut pending => panic!("definition passed the description gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    assert_eq!(
        *harness
            .provider
            .describe_requests
            .lock()
            .expect("description requests lock"),
        vec![v1::DescribeRequest {
            pool_alias: harness.binding.remote_pool.clone(),
        }]
    );
    sqlx::query(
        "SELECT binding_id FROM resource_pool_binding WHERE binding_id = $1 FOR UPDATE NOWAIT",
    )
    .bind(harness.binding.binding_id)
    .fetch_one(&harness.database)
    .await?;
    assert!(
        storage::latest(&harness.database, harness.binding.binding_id)
            .await?
            .is_none()
    );
    assert!(harness.requests().is_empty());
    let value: i32 = bounded(sqlx::query_scalar("SELECT 1").fetch_one(&single_connection)).await?;
    assert_eq!(value, 1);
    gate.release.notify_one();
    let request = bounded(pending).await?;
    assert_eq!(
        request.pool,
        Some(v1::PoolBinding {
            authority_id: harness.binding.authority_id.clone(),
            pool_id: pool().pool_id,
            binding_id: harness.binding.binding_id.to_string(),
            logical_pool: harness.binding.pool_name.clone(),
        })
    );
    single_connection.close().await;
    harness.stop().await;
    Ok(())
}

/// The saved request must be committed before provider Define runs, with the
/// binding lock and only database connection free for unrelated work.
#[carbide_macros::sqlx_test]
async fn pending_definition_is_committed_before_provider_work(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(database, Fault::StallDefine(gate.clone())).await;
    let single_connection = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(harness.database.connect_options().as_ref().clone())
        .await?;
    let eligibility = eligibility();
    let mut pending = Box::pin(define(
        &single_connection,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility,
    ));
    tokio::select! {
        result = &mut pending => panic!("definition passed its provider gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    let saved = harness.latest().await;
    let request = harness.requests().pop().expect("received Define request");
    assert!(!saved.accepted);
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.request, request.encode_to_vec());
    sqlx::query(
        "SELECT binding_id FROM resource_pool_binding WHERE binding_id = $1 FOR UPDATE NOWAIT",
    )
    .bind(harness.binding.binding_id)
    .fetch_one(&harness.database)
    .await?;
    let value: i32 = bounded(sqlx::query_scalar("SELECT 1").fetch_one(&single_connection)).await?;
    assert_eq!(value, 1);
    gate.release.notify_one();
    assert_eq!(bounded(pending).await?, request);
    assert!(harness.latest().await.accepted);
    single_connection.close().await;
    harness.stop().await;
    Ok(())
}

/// A caller returning from initial discovery must adopt a matching registration
/// another replica accepted while Describe was in flight.
#[carbide_macros::sqlx_test]
async fn initial_registration_reuses_the_winner_after_describe(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(database, Fault::StallDescription(gate.clone())).await;
    let second_provider = FaultProvider::new(harness.provider.provider.clone(), Fault::None);
    let second_server = TestServer::start(second_provider.clone()).await;
    let second_backend = GrpcResourcePoolBackend::new(second_server.endpoint.clone(), None)?;
    let eligibility = eligibility();
    let mut first = Box::pin(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility,
    ));
    tokio::select! {
        result = &mut first => panic!("first registration passed the description gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    let winner = bounded(define(
        &harness.database,
        &second_backend,
        &harness.binding,
        None,
        &eligibility,
    ))
    .await?;
    let saved = harness.latest().await;
    assert!(saved.accepted);
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.request, winner.encode_to_vec());
    gate.release.notify_one();
    assert_eq!(bounded(first).await?, winner);
    assert_eq!(harness.latest().await, saved);
    assert!(harness.requests().is_empty());
    assert_eq!(
        *second_provider
            .define_requests
            .lock()
            .expect("definition requests lock"),
        vec![winner],
    );
    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource_pool_registration WHERE binding_id = $1")
            .bind(harness.binding.binding_id)
            .fetch_one(&harness.database)
            .await?;
    assert_eq!(revisions, 1);
    drop(second_backend);
    second_server.stop().await;
    harness.stop().await;
    Ok(())
}

/// An ambiguous Define stays unavailable to allocation callers until a restarted
/// client receives and saves acknowledgement. The database keeps the original
/// request bytes, including the removed site field. Decoding and sending that
/// request again omits the site field from the wire message.
#[carbide_macros::sqlx_test]
async fn lost_define_reply_resumes_saved_bytes_after_client_restart(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, Fault::DefineReplyUnavailableOnce).await;
    let eligibility = eligibility();
    let error = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility,
    ))
    .await
    .expect_err("provider accepted definition but lost its reply");
    assert!(
        matches!(error, RegistrationError::Provider(GrpcResourcePoolError::Rpc(error))
        if error.code() == Code::Unavailable)
    );
    let mut pending = harness.latest().await;
    assert_eq!(pending.revision, 1);
    assert!(!pending.accepted);
    let request = v1::DefineRequest::decode(pending.request.as_slice())?;
    assert_eq!(harness.requests(), vec![request.clone()]);

    // Recreate a saved definition from before `PoolBinding.site_id` was removed.
    // Protobuf should ignore that field without requiring a database rewrite.
    let mut old_pool = request.pool.as_ref().expect("request pool").encode_to_vec();
    encoding::string::encode(1, &Uuid::new_v4().to_string(), &mut old_pool);
    let mut old_request = request.clone();
    old_request.pool = None;
    pending.request = old_request.encode_to_vec();
    encoding::bytes::encode(1, &old_pool, &mut pending.request);
    sqlx::query(
        "UPDATE resource_pool_registration SET request = $1 WHERE binding_id = $2 AND revision = 1",
    )
    .bind(&pending.request)
    .bind(harness.binding.binding_id)
    .execute(&harness.database)
    .await?;

    // Stats resolves a saved provider definition, proving the lost reply
    // happened after acceptance rather than before the provider applied it.
    bounded(harness.backend.stats(v1::StatsRequest {
        pool: request.pool.clone(),
        eligibility_revision: request.eligibility_revision.clone(),
    }))
    .await?;
    assert_eq!(
        accepted(&harness.database, &harness.binding, 1).await?,
        None
    );

    let mut growth = eligibility.clone();
    growth
        .entries
        .push(range(6, 10, v1::AssignmentMode::Automatic));
    assert!(matches!(
        bounded(define(
            &harness.database,
            &harness.backend,
            &harness.binding,
            Some(1),
            &growth,
        ))
        .await,
        Err(RegistrationError::Conflict)
    ));
    assert_eq!(harness.latest().await, pending);

    harness.backend = GrpcResourcePoolBackend::new(harness.server.endpoint.clone(), None)?;
    assert_eq!(
        bounded(resume(
            &harness.database,
            &harness.backend,
            &harness.binding,
            1,
        ))
        .await?,
        request
    );
    let saved = harness.latest().await;
    assert_eq!(saved.request, pending.request);
    assert_eq!(saved.revision, pending.revision);
    assert!(saved.accepted);
    assert_eq!(
        accepted(&harness.database, &harness.binding, 1).await?,
        Some(request.clone())
    );
    assert_eq!(harness.requests(), vec![request.clone(), request.clone()]);

    assert_eq!(
        bounded(resume(
            &harness.database,
            &harness.backend,
            &harness.binding,
            1,
        ))
        .await?,
        request
    );
    assert_eq!(harness.requests().len(), 2);
    assert_eq!(harness.provider.describe_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

/// Provider acceptance precedes the local acknowledgement; a failed growth
/// acknowledgement must leave the prior policy readable and the new one replayable.
#[carbide_macros::sqlx_test]
async fn failed_acknowledgement_leaves_provider_acceptance_replayable(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, Fault::None).await;
    let original = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility(),
    ))
    .await?;
    let original_record = harness.latest().await;
    sqlx::query(
        "ALTER TABLE resource_pool_registration ADD CONSTRAINT reject_acknowledgement
         CHECK (NOT accepted OR revision = 1)",
    )
    .execute(&harness.database)
    .await?;
    let mut growth = eligibility();
    growth
        .entries
        .push(range(6, 10, v1::AssignmentMode::Automatic));
    let error = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        Some(1),
        &growth,
    ))
    .await
    .expect_err("reject local acknowledgement after provider acceptance");
    assert!(matches!(error, RegistrationError::Database(error)
        if error.violates_constraint("reject_acknowledgement")));
    let pending = harness.latest().await;
    assert_eq!(pending.revision, 2);
    assert!(!pending.accepted);
    let request = v1::DefineRequest::decode(pending.request.as_slice())?;
    assert_eq!(request.eligibility, growth.entries);
    assert_eq!(request.previous_revision.as_deref(), Some("1"));
    assert_eq!(harness.requests(), vec![original.clone(), request.clone()]);
    assert_eq!(
        accepted(&harness.database, &harness.binding, 1).await?,
        Some(original.clone())
    );
    assert_eq!(
        accepted(&harness.database, &harness.binding, pending.revision).await?,
        None
    );

    sqlx::query("ALTER TABLE resource_pool_registration DROP CONSTRAINT reject_acknowledgement")
        .execute(&harness.database)
        .await?;
    harness.backend = GrpcResourcePoolBackend::new(harness.server.endpoint.clone(), None)?;
    assert_eq!(
        bounded(define(
            &harness.database,
            &harness.backend,
            &harness.binding,
            Some(1),
            &growth,
        ))
        .await?,
        request
    );
    let saved = harness.latest().await;
    assert!(saved.accepted);
    assert_eq!(saved.request, pending.request);
    assert_eq!(saved.revision, pending.revision);
    assert_eq!(harness.requests(), vec![original, request.clone(), request]);
    assert_eq!(harness.provider.describe_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        storage::load(&harness.database, harness.binding.binding_id, 1).await?,
        Some(original_record)
    );
    harness.stop().await;
    Ok(())
}

/// Inspection keeps using the provider's accepted pool identity while a newer
/// definition awaits acknowledgement, and switches only after that acknowledgement.
/// Allocation preparation reads use the caller's transaction so they can see an accepted
/// definition before it commits.
#[carbide_macros::sqlx_test]
async fn latest_accepted_keeps_the_previous_revision_during_pending_growth(
    database: PgPool,
) -> TestResult {
    let mut txn = database.begin().await?;
    let binding = binding::bind_or_verify(&mut txn, &selection()).await?;
    txn.commit().await?;
    assert_eq!(latest_accepted(&database, &binding).await?, None);

    // The provider's saved pool ID differs from its configured lookup alias.
    // Inspection must return the saved ID so Stats queries the original pool.
    let original = v1::DefineRequest {
        pool: Some(v1::PoolBinding {
            authority_id: binding.authority_id.clone(),
            pool_id: "provider-pool-id".into(),
            binding_id: binding.binding_id.to_string(),
            logical_pool: binding.pool_name.clone(),
        }),
        eligibility_revision: "1".into(),
        previous_revision: None,
        value_domain: v1::ValueDomain::Integer as i32,
        eligibility: eligibility().entries,
    };
    let mut grown = original.clone();
    grown.eligibility_revision = "2".into();
    grown.previous_revision = Some("1".into());
    grown
        .eligibility
        .push(range(6, 10, v1::AssignmentMode::Automatic));

    let mut txn = database.begin().await?;
    storage::lock_binding(&mut txn, binding.binding_id).await?;
    storage::insert(
        &mut txn,
        &ResourcePoolRegistration {
            binding_id: binding.binding_id,
            revision: 1,
            request: original.encode_to_vec(),
            accepted: true,
        },
    )
    .await?;
    storage::insert(
        &mut txn,
        &ResourcePoolRegistration {
            binding_id: binding.binding_id,
            revision: 2,
            request: grown.encode_to_vec(),
            accepted: false,
        },
    )
    .await?;
    // Preparation must use this transaction's view. A second connection
    // would miss the accepted definition until this transaction commits.
    for (scenario, revision, expected) in [
        ("accepted revision", Some(1), Some(&original)),
        ("latest accepted revision", None, Some(&original)),
        ("pending revision", Some(2), None),
    ] {
        assert_eq!(
            accepted_in_transaction(&mut txn, &binding, revision)
                .await?
                .as_ref(),
            expected,
            "{scenario}"
        );
    }
    txn.commit().await?;
    assert_eq!(latest_accepted(&database, &binding).await?, Some(original));

    // Committing acceptance makes the newer revision available to inspection.
    let mut txn = database.begin().await?;
    storage::acknowledge(&mut txn, binding.binding_id, 2).await?;
    txn.commit().await?;
    assert_eq!(latest_accepted(&database, &binding).await?, Some(grown));
    Ok(())
}

/// Growth appends declarations while retaining the original pool identity and
/// accepted history; matching retries cannot create extra revisions or RPCs.
#[carbide_macros::sqlx_test]
async fn growth_preserves_ordered_history_and_reuses_matching_revisions(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, Fault::None).await;
    let original = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility(),
    ))
    .await?;
    let original_record = harness.latest().await;
    let mut growth = eligibility();
    growth
        .entries
        .push(range(6, 10, v1::AssignmentMode::Automatic));
    let grown = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        Some(1),
        &growth,
    ))
    .await?;
    assert_eq!(grown.eligibility_revision, "2");
    assert_eq!(grown.previous_revision.as_deref(), Some("1"));
    assert_eq!(grown.pool, original.pool);
    assert_eq!(grown.eligibility, growth.entries);
    assert_eq!(
        accepted(&harness.database, &harness.binding, 1).await?,
        Some(original.clone())
    );
    assert_eq!(
        storage::load(&harness.database, harness.binding.binding_id, 1).await?,
        Some(original_record)
    );

    let mut reordered = growth.clone();
    reordered.entries.swap(0, 1);
    let mut stale_growth = growth.clone();
    stale_growth
        .entries
        .push(range(11, 15, v1::AssignmentMode::Automatic));
    struct Case {
        name: &'static str,
        previous_revision: i64,
        eligibility: ResourcePoolEligibility,
        check: fn(RegistrationError) -> bool,
    }
    for case in [
        Case {
            name: "changing an earlier declaration's precedence",
            previous_revision: 2,
            eligibility: reordered,
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid(
                        "growth must keep previous eligibility entries unchanged"
                    )
                )
            },
        },
        Case {
            name: "appending to a stale predecessor",
            previous_revision: 1,
            eligibility: stale_growth,
            check: |error| matches!(error, RegistrationError::Conflict),
        },
    ] {
        let error = bounded(define(
            &harness.database,
            &harness.backend,
            &harness.binding,
            Some(case.previous_revision),
            &case.eligibility,
        ))
        .await
        .expect_err(case.name);
        assert!((case.check)(error), "{}", case.name);
    }
    // Both a replay and an unchanged full policy use the accepted revision.
    for predecessor in [1, 2] {
        assert_eq!(
            bounded(define(
                &harness.database,
                &harness.backend,
                &harness.binding,
                Some(predecessor),
                &growth,
            ))
            .await?,
            grown
        );
    }
    assert_eq!(
        bounded(resume(
            &harness.database,
            &harness.backend,
            &harness.binding,
            1
        ))
        .await?,
        original,
    );
    assert_eq!(harness.latest().await.request, grown.encode_to_vec());
    assert_eq!(harness.requests(), vec![original, grown]);
    assert_eq!(harness.provider.describe_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

/// Preparation must reread the latest revision after a competing writer commits,
/// rather than inserting from the stale predecessor observed before the wait.
#[carbide_macros::sqlx_test]
async fn growth_waits_for_the_competing_successor_before_checking_its_predecessor(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, Fault::None).await;
    let original = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility(),
    ))
    .await?;
    let original_record = harness.latest().await;
    let mut winning_request = original.clone();
    winning_request.eligibility_revision = "2".into();
    winning_request.previous_revision = Some("1".into());
    winning_request
        .eligibility
        .push(range(6, 10, v1::AssignmentMode::Automatic));
    let winner = ResourcePoolRegistration {
        binding_id: harness.binding.binding_id,
        revision: 2,
        request: winning_request.encode_to_vec(),
        accepted: false,
    };
    let mut writer = harness.database.begin().await?;
    storage::lock_binding(&mut writer, harness.binding.binding_id).await?;
    storage::insert(&mut writer, &winner).await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let mut competing_growth = eligibility();
    competing_growth
        .entries
        .push(range(11, 15, v1::AssignmentMode::Automatic));
    let mut competing = Box::pin(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        Some(1),
        &competing_growth,
    ));
    tokio::select! {
        result = &mut competing => panic!("growth did not wait for the competing writer: {result:?}"),
        _ = bounded(wait_for_blocked_query(&harness.database, writer_pid, "resource_pool")) => {}
    }
    writer.commit().await?;
    assert!(matches!(
        bounded(competing).await,
        Err(RegistrationError::Conflict)
    ));
    assert_eq!(harness.latest().await, winner);
    assert_eq!(
        storage::load(&harness.database, harness.binding.binding_id, 1).await?,
        Some(original_record),
    );
    assert_eq!(harness.requests(), vec![original]);
    harness.stop().await;
    Ok(())
}

/// Invalid input must fail before occupying the binding's pending slot;
/// errors detectable locally must also avoid provider discovery.
#[carbide_macros::sqlx_test]
async fn invalid_registration_input_never_saves_or_defines(database: PgPool) -> TestResult {
    #[derive(Clone)]
    struct Case {
        name: &'static str,
        backend_kind: ResourcePoolBackendKind,
        change: fn(&mut ResourcePoolBinding, &mut ResourcePoolEligibility),
        previous_revision: Option<i64>,
        fault: Fault,
        descriptions: usize,
        check: fn(&RegistrationError) -> bool,
    }
    let description = v1::DescribeResponse {
        authority_id: pool().authority_id,
        pool_id: pool().pool_id,
        value_domain: v1::ValueDomain::Integer as i32,
        supports_stats: true,
    };
    let defaults = Case {
        name: "",
        backend_kind: ResourcePoolBackendKind::Grpc,
        change: |_, _| {},
        previous_revision: None,
        fault: Fault::None,
        descriptions: 0,
        check: |error| {
            matches!(
                error,
                RegistrationError::Invalid(
                    "eligibility must match the pool's value domain and contain entries"
                )
            )
        },
    };
    for case in [
        Case {
            name: "eligibility-domain",
            change: |_, eligibility| eligibility.domain = ResourcePoolValueDomain::IpAddress,
            ..defaults.clone()
        },
        Case {
            name: "provider-authority",
            fault: Fault::Description(v1::DescribeResponse {
                authority_id: "another-provider".into(),
                ..description.clone()
            }),
            descriptions: 1,
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid(
                        "provider identity or value domain differs from the binding"
                    )
                )
            },
            ..defaults.clone()
        },
        Case {
            name: "provider-domain",
            fault: Fault::Description(v1::DescribeResponse {
                value_domain: v1::ValueDomain::IpAddress as i32,
                ..description.clone()
            }),
            descriptions: 1,
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid(
                        "provider identity or value domain differs from the binding"
                    )
                )
            },
            ..defaults.clone()
        },
        Case {
            name: "integrated-binding",
            backend_kind: ResourcePoolBackendKind::Integrated,
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid("registration requires a named gRPC pool binding")
                )
            },
            ..defaults.clone()
        },
        Case {
            name: "empty-eligibility",
            change: |_, eligibility| eligibility.entries.clear(),
            ..defaults.clone()
        },
        Case {
            name: "malformed-entry",
            change: |_, eligibility| eligibility.entries[0].entries = None,
            check: |error| matches!(error, RegistrationError::Value(_)),
            ..defaults.clone()
        },
        Case {
            name: "supplied-binding-differs",
            change: |binding, _| binding.pool_name = "different-logical-pool".into(),
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid("binding differs from its saved selection")
                )
            },
            ..defaults.clone()
        },
        Case {
            name: "predecessor-without-history",
            previous_revision: Some(1),
            check: |error| matches!(error, RegistrationError::Conflict),
            ..defaults
        },
    ] {
        let mut selection = selection();
        selection.pool_name = case.name.into();
        selection.backend_kind = case.backend_kind;
        if case.backend_kind == ResourcePoolBackendKind::Integrated {
            selection.authority_id = model::resource_pool::binding::INTEGRATED_AUTHORITY_ID.into();
            selection.remote_pool = selection.pool_name.clone();
            selection.backend_name = None;
        }
        let mut txn = database.begin().await?;
        let mut binding = binding::bind_or_verify(&mut txn, &selection).await?;
        txn.commit().await?;
        let provider = FaultProvider::new(provider(), case.fault);
        let server = TestServer::start(provider.clone()).await;
        let backend = GrpcResourcePoolBackend::new(server.endpoint.clone(), None)?;
        let mut eligibility = eligibility();
        (case.change)(&mut binding, &mut eligibility);
        let error = bounded(define(
            &database,
            &backend,
            &binding,
            case.previous_revision,
            &eligibility,
        ))
        .await
        .expect_err(case.name);
        assert!((case.check)(&error), "{}: {error:?}", case.name);
        assert!(
            storage::latest(&database, binding.binding_id)
                .await?
                .is_none(),
            "{}",
            case.name
        );
        assert!(
            provider
                .define_requests
                .lock()
                .expect("definition requests lock")
                .is_empty(),
            "{}",
            case.name
        );
        assert_eq!(
            provider.describe_calls.load(Ordering::SeqCst),
            case.descriptions,
            "{}",
            case.name
        );
        drop(backend);
        server.stop().await;
    }
    Ok(())
}

/// Admission must accept the client's exact encoded limit and reject the next
/// byte before returning a request that a caller could persist.
#[test]
fn preparation_obeys_the_encoded_message_limit() {
    let selected = selection();
    let mut binding = ResourcePoolBinding {
        binding_id: Uuid::new_v4(),
        pool_name: selected.pool_name,
        value_domain: selected.value_domain,
        backend_name: selected.backend_name,
        backend_kind: selected.backend_kind,
        authority_id: selected.authority_id,
        remote_pool: selected.remote_pool,
    };
    let eligibility = eligibility();
    for size in [MAX_MESSAGE_BYTES, MAX_MESSAGE_BYTES + 1] {
        let mut request = v1::DefineRequest {
            pool: Some(v1::PoolBinding {
                authority_id: binding.authority_id.clone(),
                pool_id: binding.remote_pool.clone(),
                binding_id: binding.binding_id.to_string(),
                logical_pool: "p".repeat(size),
            }),
            eligibility_revision: "1".into(),
            previous_revision: None,
            value_domain: v1::ValueDomain::Integer as i32,
            eligibility: eligibility.entries.clone(),
        };
        let overhead = request.encoded_len() - size;
        let pool = request.pool.as_mut().expect("request pool");
        pool.logical_pool.truncate(size - overhead);
        binding.pool_name = pool.logical_pool.clone();
        assert_eq!(request.encoded_len(), size);
        match prepare(
            &binding,
            request.pool.clone().expect("request pool"),
            None,
            None,
            &eligibility,
        ) {
            Ok(PreparedRegistration::New(saved)) if size == MAX_MESSAGE_BYTES => {
                assert_eq!(saved.request, request.encode_to_vec());
                assert!(!saved.accepted);
            }
            Err(RegistrationError::Invalid(reason)) if size > MAX_MESSAGE_BYTES => {
                assert_eq!(reason, "definition exceeds the gRPC message size limit");
            }
            _ => panic!("unexpected preparation outcome for an encoded length of {size}"),
        }
    }
}

/// Recovery rejects both undecodable bytes and a decoded request belonging to
/// another binding without altering the corrupt evidence or calling the provider.
#[carbide_macros::sqlx_test]
async fn corrupt_saved_requests_are_not_replayed(database: PgPool) -> TestResult {
    let harness = Harness::new(database, Fault::None).await;
    let original = bounded(define(
        &harness.database,
        &harness.backend,
        &harness.binding,
        None,
        &eligibility(),
    ))
    .await?;
    let mut other_binding = original.clone();
    other_binding
        .pool
        .as_mut()
        .expect("request pool")
        .binding_id = Uuid::new_v4().to_string();
    struct Case {
        name: &'static str,
        request: Vec<u8>,
        check: fn(&RegistrationError) -> bool,
    }
    for case in [
        Case {
            name: "truncated protobuf field",
            request: vec![0x80],
            check: |error| matches!(error, RegistrationError::Decode(_)),
        },
        Case {
            name: "request belongs to another binding",
            request: other_binding.encode_to_vec(),
            check: |error| {
                matches!(
                    error,
                    RegistrationError::Invalid("saved definition does not match its pool binding")
                )
            },
        },
    ] {
        sqlx::query("UPDATE resource_pool_registration SET request = $1 WHERE binding_id = $2 AND revision = 1")
            .bind(&case.request).bind(harness.binding.binding_id)
            .execute(&harness.database).await?;
        let error = bounded(resume(
            &harness.database,
            &harness.backend,
            &harness.binding,
            1,
        ))
        .await
        .expect_err(case.name);
        assert!((case.check)(&error), "{}: {error:?}", case.name);
        assert_eq!(
            harness.latest().await.request,
            case.request,
            "{}",
            case.name
        );
        assert_eq!(harness.requests(), vec![original.clone()], "{}", case.name);
    }
    harness.stop().await;
    Ok(())
}

/// Cancelling each public lookup must eventually release its pooled connection
/// even when PostgreSQL cannot finish the read until its statement timeout expires.
#[carbide_macros::sqlx_test]
async fn cancelled_registration_reads_return_the_only_connection(database: PgPool) -> TestResult {
    #[derive(Clone, Copy, Debug)]
    enum Read {
        Resume,
        Accepted,
        LatestAccepted,
    }
    let harness = Harness::new(database, Fault::None).await;
    let mut blocker = harness.database.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("LOCK TABLE resource_pool_registration IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    // Configure every pool before starting reads so fixture setup does not
    // consume their PostgreSQL statement deadlines.
    let mut connections = Vec::new();
    for operation in [Read::Resume, Read::Accepted, Read::LatestAccepted] {
        let single_connection = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(harness.database.connect_options().as_ref().clone())
            .await?;
        sqlx::query("SET statement_timeout = 0")
            .execute(&single_connection)
            .await?;
        connections.push((operation, single_connection));
    }
    let mut readers = JoinSet::new();
    for (operation, single_connection) in &connections {
        let operation = *operation;
        let single_connection = single_connection.clone();
        let backend = harness.backend.clone();
        let binding = harness.binding.clone();
        readers.spawn(async move {
            let result = match operation {
                Read::Resume => resume(&single_connection, &backend, &binding, 1)
                    .await
                    .map(drop),
                Read::Accepted => accepted(&single_connection, &binding, 1).await.map(drop),
                Read::LatestAccepted => latest_accepted(&single_connection, &binding)
                    .await
                    .map(drop),
            };
            (operation, result)
        });
    }
    // Each read has its own only connection. Observe all three blocked before
    // cancelling them so their independent server deadlines can overlap.
    tokio::select! {
        result = readers.join_next() => panic!("a registration read did not stay blocked: {result:?}"),
        count = bounded(wait_for_blocked_queries(
            &harness.database, blocker_pid, "FROM resource_pool_registration", connections.len(),
        )) => assert_eq!(count, connections.len()),
    }
    readers.abort_all();
    while let Some(result) = readers.join_next().await {
        assert!(
            result
                .expect_err("the blocked read must be cancelled")
                .is_cancelled()
        );
    }
    for (operation, single_connection) in connections {
        // SQLx queues rollback after the blocked statement. Keep the lock held
        // until the server-side timeout has freed the only service connection.
        let value: i32 = tokio::time::timeout(
            DATABASE_WAIT_TIMEOUT + Duration::from_secs(3),
            sqlx::query_scalar("SELECT 1").fetch_one(&single_connection),
        )
        .await??;
        assert_eq!(value, 1, "{operation:?}");
        assert!(harness.requests().is_empty(), "{operation:?}");
        single_connection.close().await;
    }
    blocker.rollback().await?;
    harness.stop().await;
    Ok(())
}
