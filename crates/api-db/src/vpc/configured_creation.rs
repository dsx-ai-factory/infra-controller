// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Keep a configured VPC's generated IDs until creation or allocation cleanup finishes.

use carbide_uuid::vpc::VpcId;
use model::vpc::ConfiguredVpcCreation;
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// `begin` saves generated IDs once and locks the existing or inserted row.
/// Commit this row with any allocation intent before resolving the pool request.
pub async fn begin(
    txn: &mut PgConnection,
    name: &str,
    organization_id: Option<&str>,
) -> Result<ConfiguredVpcCreation, DatabaseError> {
    // Return the IDs while locking the row; a separate lookup can lose the row
    // to concurrent removal. Conflicts keep the original IDs.
    let query = "INSERT INTO configured_vpc_creation (name, vpc_id, tenant_organization_id)
        VALUES ($1, $2, $3)
        ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
        RETURNING name, vpc_id, tenant_organization_id";
    sqlx::query_as(query)
        .bind(name)
        .bind(VpcId::new())
        .bind(
            organization_id
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        )
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `lock` keeps creation and removal of one configured request serialized.
/// `None` means another startup already completed or removed the request.
pub async fn lock(
    txn: &mut PgConnection,
    name: &str,
) -> Result<Option<ConfiguredVpcCreation>, DatabaseError> {
    let query = "SELECT name, vpc_id, tenant_organization_id FROM configured_vpc_creation
        WHERE name = $1 FOR UPDATE";
    sqlx::query_as(query)
        .bind(name)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `list` returns unfinished configured requests so removed definitions can cancel.
pub async fn list(db: impl DbReader<'_>) -> Result<Vec<ConfiguredVpcCreation>, DatabaseError> {
    let query =
        "SELECT name, vpc_id, tenant_organization_id FROM configured_vpc_creation ORDER BY name";
    sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `remove` forgets the IDs after VPC creation or confirmed allocation cleanup.
/// The caller holds the row lock and commits both changes in one transaction.
pub async fn remove(txn: &mut PgConnection, name: &str) -> Result<(), DatabaseError> {
    let query = "DELETE FROM configured_vpc_creation WHERE name = $1";
    sqlx::query(query)
        .bind(name)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::{begin, list, lock, remove};
    use crate::test_support::postgres::wait_for_blocked_query;

    /// Retries keep the saved IDs, while creation overlapping removal receives
    /// fresh IDs instead of failing after the old row disappears.
    #[crate::sqlx_test]
    async fn begin_preserves_ids_and_recreates_after_concurrent_removal(
        pool: PgPool,
    ) -> eyre::Result<()> {
        let name = "configured-vpc";
        let mut txn = pool.begin().await?;
        let original = begin(&mut txn, name, None).await?;
        txn.commit().await?;

        // A retry must retain both IDs even when it supplies a different tenant.
        let mut txn = pool.begin().await?;
        assert_eq!(begin(&mut txn, name, Some("other-tenant")).await?, original);
        txn.commit().await?;

        let mut deleting = pool.begin().await?;
        assert_eq!(lock(&mut deleting, name).await?, Some(original.clone()));
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *deleting)
            .await?;
        let create_again = async {
            let mut txn = pool.begin().await?;
            let created = begin(&mut txn, name, None).await?;
            txn.commit().await?;
            Ok::<_, eyre::Report>(created)
        };
        let finish_removal = async {
            // Release the lock only after the competing begin reaches the row.
            wait_for_blocked_query(&pool, blocker_pid, "configured_vpc_creation").await;
            remove(&mut deleting, name).await?;
            deleting.commit().await?;
            Ok::<_, eyre::Report>(())
        };
        let (created, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(create_again, finish_removal)
        })
        .await??;

        assert_eq!(created.name, name);
        assert_ne!(created.vpc_id, original.vpc_id);
        assert_ne!(
            created.tenant_organization_id,
            original.tenant_organization_id
        );
        assert_eq!(list(&pool).await?, vec![created]);
        Ok(())
    }
}
