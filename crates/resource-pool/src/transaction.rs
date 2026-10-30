// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use db::{DatabaseError, Transaction};
use sqlx::PgPool;

/// Maximum statement wait; callers separately bound the complete operation.
pub(crate) const DATABASE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// `database_transaction` gives resource pool queries a five-second statement
/// limit, preserving a shorter database setting. The limit ends with the transaction.
/// This does not limit connection acquisition or the complete operation; callers
/// supply those deadlines when needed and commit before waiting for a provider.
/// After cancellation, a started statement can finish under its server-side limit
/// before PostgreSQL processes the queued rollback.
pub async fn database_transaction(pool: &PgPool) -> Result<Transaction<'_>, DatabaseError> {
    let mut txn = Transaction::begin(pool).await?;
    // A dropped SQLx query can retain its pooled connection and any work-lock
    // fence until PostgreSQL finishes the statement and runs the queued rollback.
    // Bound statements to five seconds, preserving any shorter operator limit.
    sqlx::query(
        "SELECT set_config('statement_timeout', \
            LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true) \
         FROM pg_settings WHERE name = 'statement_timeout'",
    )
    .bind(DATABASE_WAIT_TIMEOUT.as_millis().to_string())
    .execute(txn.as_pgconn())
    .await
    .map_err(|error| DatabaseError::query("set resource pool statement timeout", error))?;
    Ok(txn)
}
