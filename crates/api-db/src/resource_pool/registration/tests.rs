// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use futures::TryFutureExt;
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::{
    ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolSelection, ResourcePoolValueDomain,
};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{acknowledge, get_or_create_site_id, insert, latest, load, lock_binding};
use crate::DatabaseError;
use crate::resource_pool::binding::bind_or_verify;
use crate::test_support::postgres::wait_for_blocked_query;

/// Create a remote integer pool binding without seeding inventory or registrations.
async fn new_binding(txn: &mut PgConnection, name: &str) -> eyre::Result<ResourcePoolBinding> {
    Ok(bind_or_verify(
        txn,
        &ResourcePoolSelection {
            pool_name: name.to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("provider".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "test-authority".to_owned(),
            remote_pool: name.to_owned(),
        },
    )
    .await?)
}

/// Replicas that race to initialize the site must adopt the committed UUID.
#[crate::sqlx_test]
async fn concurrent_creators_share_the_committed_site_identity(pool: PgPool) -> eyre::Result<()> {
    let mut first_txn = pool.begin().await?;
    let first = get_or_create_site_id(&mut first_txn).await?;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first_txn)
        .await?;
    let mut second_txn = pool.begin().await?;

    let second_identity = async {
        let second = get_or_create_site_id(&mut second_txn).await?;
        second_txn.commit().await?;
        Ok::<_, eyre::Report>(second)
    };
    let commit_first = async {
        // Make the losing INSERT wait so its later SELECT must see the commit.
        wait_for_blocked_query(&pool, first_pid, "INSERT INTO resource_pool_site").await;
        first_txn.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (second, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(second_identity, commit_first)
    })
    .await??;
    assert!(!first.is_nil());
    assert_eq!(second, first);

    let mut txn = pool.begin().await?;
    assert_eq!(get_or_create_site_id(&mut txn).await?, first);
    txn.commit().await?;
    Ok(())
}

/// Database constraints reject each invalid write without replacing saved bytes.
#[crate::sqlx_test]
async fn registration_constraints_preserve_the_saved_request(pool: PgPool) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    let binding = new_binding(&mut txn, "constraints").await?;
    let saved = ResourcePoolRegistration {
        binding_id: binding.binding_id,
        revision: 1,
        request: vec![0, 255, 128, 42],
        accepted: false,
    };
    insert(&mut txn, &saved).await?;
    txn.commit().await?;

    check_cases_async(
        [
            (
                "one pending revision per binding",
                ResourcePoolRegistration {
                    revision: 2,
                    ..saved.clone()
                },
                "resource_pool_registration_pending",
            ),
            (
                "accepted rows cannot replace a revision",
                ResourcePoolRegistration {
                    accepted: true,
                    ..saved.clone()
                },
                "resource_pool_registration_pkey",
            ),
            (
                "revisions start above zero",
                ResourcePoolRegistration {
                    revision: 0,
                    accepted: true,
                    ..saved.clone()
                },
                "resource_pool_registration_revision_check",
            ),
            (
                "registration requires a binding",
                ResourcePoolRegistration {
                    binding_id: Uuid::new_v4(),
                    ..saved.clone()
                },
                "resource_pool_registration_binding_id_fkey",
            ),
        ]
        .map(|(scenario, registration, constraint)| Case {
            scenario,
            input: (registration, constraint),
            expect: Yields(true),
        }),
        |(registration, constraint)| {
            let pool = &pool;
            async move {
                let mut txn = pool.begin().await?;
                let error = insert(&mut txn, &registration)
                    .await
                    .expect_err("invalid registration must fail");
                txn.rollback().await?;
                Ok::<_, eyre::Report>(error.violates_constraint(constraint))
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;

    assert_eq!(load(&pool, saved.binding_id, 1).await?, Some(saved.clone()));
    assert_eq!(latest(&pool, saved.binding_id).await?, Some(saved));
    Ok(())
}

/// Registration writes follow the caller's commit boundary and retain old revisions.
#[crate::sqlx_test]
async fn caller_transactions_preserve_history_and_pending_revisions(
    pool: PgPool,
) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    let binding = new_binding(&mut txn, "history").await?;
    let other_binding = new_binding(&mut txn, "other").await?;
    txn.commit().await?;
    let pending = ResourcePoolRegistration {
        binding_id: binding.binding_id,
        revision: 1,
        request: vec![0, 255, 128, 42],
        accepted: false,
    };
    assert_eq!(latest(&pool, binding.binding_id).await?, None);

    // Abandoning preparation must leave no request for a later worker to send.
    let mut txn = pool.begin().await?;
    assert!(matches!(
        lock_binding(&mut txn, Uuid::new_v4()).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    assert_eq!(lock_binding(&mut txn, binding.binding_id).await?, binding);
    insert(&mut txn, &pending).await?;
    assert_eq!(
        load(&mut *txn, binding.binding_id, 1).await?,
        Some(pending.clone())
    );
    txn.rollback().await?;
    assert_eq!(latest(&pool, binding.binding_id).await?, None);

    let mut txn = pool.begin().await?;
    insert(&mut txn, &pending).await?;
    // Another binding can register while this one still awaits its provider.
    let other = ResourcePoolRegistration {
        binding_id: other_binding.binding_id,
        ..pending.clone()
    };
    insert(&mut txn, &other).await?;
    txn.commit().await?;

    // An uncommitted acknowledgement must leave the saved request pending.
    let accepted = ResourcePoolRegistration {
        accepted: true,
        ..pending.clone()
    };
    let mut txn = pool.begin().await?;
    acknowledge(&mut txn, binding.binding_id, 1).await?;
    assert_eq!(
        load(&mut *txn, binding.binding_id, 1).await?,
        Some(accepted.clone())
    );
    txn.rollback().await?;
    assert_eq!(load(&pool, binding.binding_id, 1).await?, Some(pending));

    // Acceptance frees the pending slot while keeping the old request for retries.
    let mut txn = pool.begin().await?;
    acknowledge(&mut txn, binding.binding_id, 1).await?;
    acknowledge(&mut txn, binding.binding_id, 1).await?;
    let successor = ResourcePoolRegistration {
        revision: 2,
        request: vec![18, 2, 8, 1],
        accepted: false,
        ..accepted.clone()
    };
    insert(&mut txn, &successor).await?;
    assert!(matches!(
        acknowledge(&mut txn, binding.binding_id, 3).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    txn.commit().await?;

    assert_eq!(load(&pool, binding.binding_id, 1).await?, Some(accepted));
    assert_eq!(latest(&pool, binding.binding_id).await?, Some(successor));
    assert_eq!(load(&pool, binding.binding_id, 3).await?, None);
    assert_eq!(latest(&pool, other_binding.binding_id).await?, Some(other));
    Ok(())
}

/// Registration locks exclude another preparation without blocking foreign-key reads.
#[crate::sqlx_test]
async fn binding_lock_serializes_registration_and_allows_key_shares(
    pool: PgPool,
) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    let binding = new_binding(&mut txn, "locking").await?;
    txn.commit().await?;

    let mut first_txn = pool.begin().await?;
    assert_eq!(
        lock_binding(&mut first_txn, binding.binding_id).await?,
        binding
    );
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first_txn)
        .await?;

    // Foreign-key checks take this lock when an allocation references the binding.
    let mut key_share_txn = pool.begin().await?;
    let referenced_id: Uuid = sqlx::query_scalar(
        "SELECT binding_id FROM resource_pool_binding
         WHERE binding_id = $1 FOR KEY SHARE NOWAIT",
    )
    .bind(binding.binding_id)
    .fetch_one(&mut *key_share_txn)
    .await?;
    assert_eq!(referenced_id, binding.binding_id);
    key_share_txn.commit().await?;

    let mut second_txn = pool.begin().await?;
    let second_binding = async {
        let second = lock_binding(&mut second_txn, binding.binding_id).await?;
        second_txn.commit().await?;
        Ok::<_, eyre::Report>(second)
    };
    let commit_first = async {
        wait_for_blocked_query(&pool, first_pid, "FOR NO KEY UPDATE").await;
        first_txn.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (second, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(second_binding, commit_first)
    })
    .await??;
    assert_eq!(second, binding);
    Ok(())
}

/// Adding registration storage preserves existing pools and enforces one site row.
#[crate::sqlx_test]
async fn migration_preserves_existing_pools_and_constrains_site_identity(
    pool: PgPool,
) -> eyre::Result<()> {
    crate::migrations::migrate_before(&pool, 20261021212048).await;
    sqlx::raw_sql(
        r#"INSERT INTO resource_pool_binding
            (binding_id, pool_name, value_domain, backend_kind, authority_id, remote_pool)
           VALUES (gen_random_uuid(), 'existing', 'integer', 'integrated', 'local-db', 'existing');
           INSERT INTO resource_pool (name, value, value_type, state, auto_assign, allocated)
           VALUES ('existing', '73', 'integer',
                   '{"state":"allocated","owner":"vpc-a","owner_type":"vpc"}',
                   true, '2026-10-01T12:00:00Z')"#,
    )
    .execute(&pool)
    .await?;
    let binding = crate::resource_pool::binding::load(&pool, "existing")
        .await?
        .expect("predecessor binding");
    let inventory_query = "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value";
    let before: Vec<Value> = sqlx::query_scalar(inventory_query).fetch_all(&pool).await?;

    let mut txn = pool.begin().await?;
    sqlx::raw_sql(include_str!(
        "../../../migrations/20261021212048_resource_pool_registration.sql"
    ))
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    let after: Vec<Value> = sqlx::query_scalar(inventory_query).fetch_all(&pool).await?;
    assert_eq!(after, before);
    assert_eq!(
        crate::resource_pool::binding::load(&pool, "existing").await?,
        Some(binding.clone())
    );
    assert_eq!(latest(&pool, binding.binding_id).await?, None);

    let error =
        sqlx::query("INSERT INTO resource_pool_site (singleton, site_id) VALUES (false, $1)")
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .expect_err("the singleton key cannot have a second value");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("resource_pool_site_singleton_check")
    );
    let mut txn = pool.begin().await?;
    assert!(!get_or_create_site_id(&mut txn).await?.is_nil());
    txn.commit().await?;
    Ok(())
}
