// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use db::{DatabaseError, Transaction};
use sqlx::PgPool;

/// Maximum statement wait; callers separately bound the complete operation.
pub(crate) const DATABASE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// `database_transaction` bounds statements that save or recover provider work.
/// A caller still owns the overall deadline and must commit before provider I/O.
pub(crate) async fn database_transaction(pool: &PgPool) -> Result<Transaction<'_>, DatabaseError> {
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
