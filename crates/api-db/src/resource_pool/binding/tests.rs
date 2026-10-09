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

use std::collections::HashMap;
use std::time::Duration;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use futures::TryFutureExt;
use model::resource_pool::binding::INTEGRATED_AUTHORITY_ID;
use model::resource_pool::{
    ResourcePoolBackendKind, ResourcePoolSelection, ResourcePoolValueDomain,
};
use serde_json::Value;
use sqlx::PgPool;

use super::{BindingError, all, bind_or_verify, existing_pool_domains, load};

fn integrated(name: &str, value_domain: ResourcePoolValueDomain) -> ResourcePoolSelection {
    ResourcePoolSelection {
        pool_name: name.to_string(),
        value_domain,
        backend_name: None,
        backend_kind: ResourcePoolBackendKind::Integrated,
        authority_id: INTEGRATED_AUTHORITY_ID.to_string(),
        remote_pool: name.to_string(),
    }
}

#[crate::sqlx_test]
async fn repeated_bindings_preserve_identity_and_original_selector(pool: PgPool) {
    check_cases_async(
        [
            ("implicit", None),
            ("named", Some("original-integrated".to_string())),
        ]
        .map(|(name, backend_name)| Case {
            scenario: name,
            input: ResourcePoolSelection {
                backend_name,
                ..integrated(name, ResourcePoolValueDomain::Integer)
            },
            expect: Yields(()),
        }),
        |requested| {
            let pool = &pool;
            async move {
                assert_eq!(load(pool, &requested.pool_name).await?, None);
                let mut txn = pool.begin().await?;
                let original = bind_or_verify(&mut txn, &requested).await?;
                assert!(!original.binding_id.is_nil());
                assert_eq!(original.backend_name, requested.backend_name);
                assert_eq!(bind_or_verify(&mut txn, &requested).await?, original);
                txn.commit().await?;

                // Each transaction starts with only the persisted identity,
                // just as a later process would after loading configuration.
                for backend_name in [Some("integrated-alias".to_string()), None] {
                    let mut txn = pool.begin().await?;
                    let requested = ResourcePoolSelection {
                        backend_name,
                        ..requested.clone()
                    };
                    assert_eq!(bind_or_verify(&mut txn, &requested).await?, original);
                    txn.commit().await?;
                    assert_eq!(
                        load(pool, &requested.pool_name).await?,
                        Some(original.clone())
                    );
                }
                Ok::<_, eyre::Report>(())
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
}

#[crate::sqlx_test]
async fn immutable_changes_preserve_an_empty_pool_binding(pool: PgPool) -> eyre::Result<()> {
    let requested = integrated("empty-pool", ResourcePoolValueDomain::Integer);
    let mut txn = pool.begin().await?;
    let original = bind_or_verify(&mut txn, &requested).await?;
    txn.commit().await?;
    let resource_count: i64 = sqlx::query_scalar("SELECT count(*) FROM resource_pool")
        .fetch_one(&pool)
        .await?;
    assert_eq!(resource_count, 0);

    check_cases_async(
        [
            (
                "value domain",
                ResourcePoolSelection {
                    value_domain: ResourcePoolValueDomain::IpAddress,
                    ..requested.clone()
                },
            ),
            (
                "backend kind",
                ResourcePoolSelection {
                    backend_kind: ResourcePoolBackendKind::Grpc,
                    ..requested.clone()
                },
            ),
            (
                "authority",
                ResourcePoolSelection {
                    authority_id: "another-authority".to_string(),
                    ..requested.clone()
                },
            ),
            (
                "remote pool",
                ResourcePoolSelection {
                    remote_pool: "another-pool".to_string(),
                    ..requested.clone()
                },
            ),
        ]
        .map(|(scenario, requested)| Case {
            scenario,
            input: requested.clone(),
            expect: Yields((
                requested.pool_name.clone(),
                original.clone(),
                requested,
                Some(original.clone()),
            )),
        }),
        |requested| {
            let pool = &pool;
            async move {
                let mut txn = pool.begin().await?;
                let error = bind_or_verify(&mut txn, &requested)
                    .await
                    .expect_err("changing an immutable field must fail");
                let BindingError::Conflict {
                    name,
                    stored,
                    requested,
                } = error
                else {
                    return Err(eyre::eyre!("expected a binding conflict, got {error}"));
                };
                txn.commit().await?;
                let reloaded = load(pool, &name).await?;
                Ok((name, *stored, *requested, reloaded))
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn caller_rollback_removes_bindings_written_before_a_conflict(
    pool: PgPool,
) -> eyre::Result<()> {
    let existing = integrated("existing", ResourcePoolValueDomain::Integer);
    let mut txn = pool.begin().await?;
    let original = bind_or_verify(&mut txn, &existing).await?;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    let new = integrated("new", ResourcePoolValueDomain::IpAddress);
    let created = bind_or_verify(&mut txn, &new).await?;
    assert_eq!(load(&mut *txn, &new.pool_name).await?, Some(created));
    let conflicting = ResourcePoolSelection {
        authority_id: "different-authority".to_string(),
        ..existing
    };
    assert!(matches!(
        bind_or_verify(&mut txn, &conflicting).await,
        Err(BindingError::Conflict { .. })
    ));
    txn.rollback().await?;

    assert_eq!(load(&pool, &new.pool_name).await?, None);
    assert_eq!(all(&pool).await?, vec![original]);
    Ok(())
}

#[crate::sqlx_test]
async fn concurrent_first_bindings_return_the_committed_identity(pool: PgPool) -> eyre::Result<()> {
    let requested = integrated("concurrent", ResourcePoolValueDomain::Integer);
    let mut first_txn = pool.begin().await?;
    let first = bind_or_verify(&mut first_txn, &requested).await?;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first_txn)
        .await?;
    let mut second_txn = pool.begin().await?;
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second_txn)
        .await?;

    let second_binding = async {
        let binding = bind_or_verify(&mut second_txn, &requested).await?;
        second_txn.commit().await?;
        Ok::<_, eyre::Report>(binding)
    };
    let commit_first = async {
        // Commit only after PostgreSQL has made the other insert wait. This
        // exercises the read after an INSERT loses to a concurrent binding.
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                .bind(first_pid)
                .bind(second_pid)
                .fetch_one(&pool)
                .await?;
            if blocked {
                break;
            }
        }
        first_txn.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (second, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(second_binding, commit_first)
    })
    .await??;

    assert_eq!(second, first);
    assert_eq!(all(&pool).await?, vec![first]);
    Ok(())
}

#[crate::sqlx_test]
async fn migration_and_bootstrap_preserve_legacy_resources_and_snapshots(
    pool: PgPool,
) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    // The harness migrates an empty database. Remove only the new table to
    // exercise this migration against populated predecessor tables.
    sqlx::query("DROP TABLE resource_pool_binding")
        .execute(&mut *txn)
        .await?;
    sqlx::raw_sql(
        r#"INSERT INTO resource_pool
            (name, value, value_type, state, state_version, allocated, auto_assign)
           VALUES
            ('legacy-allocated', '73', 'integer',
             '{"state":"allocated","owner":"segment-a","owner_type":"network_segment"}',
             'V1', '2026-10-01T12:00:00Z', true),
            ('legacy-rows-only', '192.0.2.10', 'ipv4', '{"state":"free"}',
             'V2', NULL, false),
            ('legacy-mixed-ip', '192.0.2.20', 'ipv4', '{"state":"free"}',
             'V1', NULL, true),
            ('legacy-mixed-ip', '2001:db8::20', 'ipv6', '{"state":"free"}',
             'V1', NULL, true);
           INSERT INTO resource_pool_def (name, definition)
           VALUES
            ('legacy-allocated', '{"type":"ipv6","prefix":"2001:db8::/64"}'),
            ('legacy-snapshot-only', '{"type":"ipv6prefix","ranges":[]}');"#,
    )
    .execute(&mut *txn)
    .await?;
    let old_resources: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
    )
    .fetch_all(&mut *txn)
    .await?;
    let old_snapshots: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool_def) FROM resource_pool_def ORDER BY name",
    )
    .fetch_all(&mut *txn)
    .await?;

    sqlx::raw_sql(include_str!(
        "../../../migrations/20261007234129_resource_pool_binding.sql"
    ))
    .execute(&mut *txn)
    .await?;
    assert!(all(&mut *txn).await?.is_empty());
    let existing = existing_pool_domains(&mut *txn).await?;
    assert_eq!(
        existing,
        HashMap::from([
            (
                "legacy-allocated".to_string(),
                ResourcePoolValueDomain::Integer
            ),
            (
                "legacy-mixed-ip".to_string(),
                ResourcePoolValueDomain::IpAddress
            ),
            (
                "legacy-rows-only".to_string(),
                ResourcePoolValueDomain::IpAddress
            ),
            (
                "legacy-snapshot-only".to_string(),
                ResourcePoolValueDomain::Ipv6Prefix
            ),
        ])
    );
    let mut expected_bindings = Vec::new();
    for (name, value_domain) in existing {
        expected_bindings.push(bind_or_verify(&mut txn, &integrated(&name, value_domain)).await?);
    }
    expected_bindings.sort_by(|left, right| left.pool_name.cmp(&right.pool_name));
    txn.commit().await?;

    assert_eq!(all(&pool).await?, expected_bindings);
    let resources: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool) FROM resource_pool ORDER BY name, value",
    )
    .fetch_all(&pool)
    .await?;
    let snapshots: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(resource_pool_def) FROM resource_pool_def ORDER BY name",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(resources, old_resources);
    assert_eq!(snapshots, old_snapshots);
    Ok(())
}

#[crate::sqlx_test]
async fn mixed_legacy_resource_domains_fail_before_binding(pool: PgPool) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    sqlx::query(
        r#"INSERT INTO resource_pool (name, value, value_type, state)
           VALUES ('mixed', '1', 'integer', '{"state":"free"}'),
                  ('mixed', '192.0.2.1', 'ipv4', '{"state":"free"}')"#,
    )
    .execute(&mut *txn)
    .await?;

    let error = existing_pool_domains(&mut *txn)
        .await
        .expect_err("one logical pool cannot have two stored value domains");
    let BindingError::MixedValueDomains {
        name,
        first,
        second,
    } = error
    else {
        return Err(eyre::eyre!("expected mixed value domains, got {error}"));
    };
    assert_eq!(name, "mixed");
    assert!(
        (first == ResourcePoolValueDomain::Integer && second == ResourcePoolValueDomain::IpAddress)
            || (first == ResourcePoolValueDomain::IpAddress
                && second == ResourcePoolValueDomain::Integer)
    );
    assert!(all(&mut *txn).await?.is_empty());
    txn.rollback().await?;
    Ok(())
}
