// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Pooled session ownership for read-only transactions that can be abandoned.

use std::time::Duration;

use crate::{DatabaseError, Transaction};

/// `DbSessionGuard` owns a pooled connection until successful work explicitly
/// returns it through `return_to_pool`. Dropping the guard before that discards
/// the connection and immediately releases the local pool slot.
///
/// SQLx's normal cleanup can wait for a blocked query before rollback, so the
/// guard also covers setup and network stalls that the statement cap cannot.
/// PostgreSQL can keep an abandoned server session alive after local capacity
/// is released. Replacements can therefore exceed the local pool's connection count.
pub struct DbSessionGuard {
    connection: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
}

impl DbSessionGuard {
    /// Wraps the pooled connection before `BEGIN`, so failed or cancelled
    /// transaction startup still releases its pool slot. The caller must include
    /// acquisition in its deadline; this guard installs no separate client timer.
    pub async fn acquire(pool: &sqlx::PgPool) -> Result<Self, DatabaseError> {
        Ok(Self {
            connection: Some(pool.acquire().await.map_err(DatabaseError::acquire)?),
        })
    }

    /// Starts a repeatable-read, read-only transaction and caps each subsequent
    /// statement at `statement_timeout`, preserving a shorter operator limit.
    /// The cap is transaction-local and resets at transaction end.
    ///
    /// Durations are truncated to whole milliseconds and must be in
    /// `1..=i32::MAX` milliseconds; other values return `InvalidArgument`.
    /// Callers must separately bound acquisition, setup and total query time
    /// with their own cancellation/deadline scope.
    pub async fn begin_read_only_snapshot(
        &mut self,
        statement_timeout: Duration,
    ) -> Result<Transaction<'_>, DatabaseError> {
        let timeout_ms = statement_timeout.as_millis();
        if !(1..=i32::MAX as u128).contains(&timeout_ms) {
            return Err(DatabaseError::InvalidArgument(
                "statement timeout must be between 1 and 2147483647 milliseconds".into(),
            ));
        }
        let connection = self
            .connection
            .as_deref_mut()
            .expect("the guard retains its connection until return_to_pool consumes it");
        let mut txn = Transaction::begin_inner(connection).await?;
        crate::set_read_only_snapshot(txn.as_pgconn()).await?;

        // `pg_settings` reports milliseconds. `NULLIF` turns a disabled (zero)
        // timeout into NULL, which `LEAST` ignores when applying our cap. A
        // shorter limit remains unchanged. The `true` argument makes this
        // transaction-local, so the setting resets when the txn ends.
        sqlx::query(
            "SELECT set_config('statement_timeout', \
                LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true) \
             FROM pg_settings WHERE name = 'statement_timeout'",
        )
        .bind(timeout_ms.to_string())
        .execute(txn.as_pgconn())
        .await
        .map_err(|error| DatabaseError::query("set read session statement timeout", error))?;
        Ok(txn)
    }

    /// Returns a healthy connection through SQLx after its transaction commits.
    /// Call this only after successful work; dropping an abandoned guard discards
    /// its connection instead of waiting for SQLx rollback cleanup.
    pub fn return_to_pool(mut self) {
        drop(self.connection.take());
    }
}

impl Drop for DbSessionGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detaching frees the local pool slot immediately. Closing the
            // socket does not guarantee that PostgreSQL stops immediately;
            // after snapshot setup, its statement timeout separately bounds
            // each subsequent SQL statement. Discarded server sessions may
            // briefly remain alongside replacements and exceed the pool limit.
            drop(connection.detach());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapshot setup must configure read-only, repeatable-read queries and the
    /// server cap without extending a tighter operator limit or changing the
    /// setting used by later pool borrowers.
    #[crate::sqlx_test]
    async fn snapshot_statement_timeout_is_capped_and_transaction_local(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = DbSessionGuard::acquire(&pool).await?;

        for (configured_ms, cap, expected_ms) in [
            // A disabled server timeout still needs the snapshot's limit.
            (0, Duration::from_millis(5000), 5000_i64),
            // A broader site limit must not extend snapshot SQL work.
            (60000, Duration::from_millis(5000), 5000),
            // Preserve an operator's stricter limit.
            (1000, Duration::from_millis(5000), 1000),
            // A custom fractional cap must be truncated, not hard-coded or rounded up.
            (0, Duration::from_micros(2_500_999), 2500),
        ] {
            sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                .bind(configured_ms.to_string())
                .execute(
                    connection
                        .connection
                        .as_deref_mut()
                        .expect("snapshot lease"),
                )
                .await?;

            let mut txn = connection.begin_read_only_snapshot(cap).await?;
            let effective_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(txn.as_pgconn())
            .await?;
            assert_eq!(
                effective_ms, expected_ms,
                "configured {configured_ms}ms, cap {cap:?}"
            );
            let (read_only, isolation): (String, String) = sqlx::query_as(
                "SELECT current_setting('transaction_read_only'), \
                        current_setting('transaction_isolation')",
            )
            .fetch_one(txn.as_pgconn())
            .await?;
            assert_eq!(read_only, "on");
            assert_eq!(isolation, "repeatable read");
            txn.commit().await?;

            let restored_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(
                connection
                    .connection
                    .as_deref_mut()
                    .expect("snapshot lease"),
            )
            .await?;
            assert_eq!(
                restored_ms, configured_ms,
                "configured {configured_ms}ms, cap {cap:?}"
            );
        }
        connection.return_to_pool();

        Ok(())
    }

    /// Invalid durations must report argument errors rather than silently
    /// disabling the timeout, and leave the guard usable for a valid cap.
    #[crate::sqlx_test]
    async fn invalid_statement_caps_leave_the_guard_usable(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = DbSessionGuard::acquire(&pool).await?;
        for cap in [
            // Zero would disable PostgreSQL's timeout protection.
            Duration::ZERO,
            // A nonzero submillisecond value also becomes disabled after truncation.
            Duration::from_micros(999),
            // PostgreSQL stores the millisecond setting in a signed integer.
            Duration::from_millis(i32::MAX as u64 + 1),
        ] {
            let result = connection.begin_read_only_snapshot(cap).await;

            assert!(matches!(result, Err(DatabaseError::InvalidArgument(_))));
        }

        // The same guard remains usable after each argument validation failure.
        connection
            .begin_read_only_snapshot(Duration::from_secs(5))
            .await?
            .commit()
            .await?;
        connection.return_to_pool();
        Ok(())
    }
}
