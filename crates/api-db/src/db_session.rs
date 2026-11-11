// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Pooled session ownership for database work that can be abandoned.

use std::panic::Location;
use std::time::Duration;

use crate::{DatabaseError, Transaction};

/// `DbSessionGuard` owns a pooled connection until a completed commit or
/// explicit rollback returns it through `return_to_pool`. Dropping it discards
/// the connection and immediately releases the local pool slot.
///
/// SQLx's normal cleanup can wait for a blocked query before rollback, so the
/// guard also covers setup and network stalls that the statement cap cannot.
/// PostgreSQL can keep an abandoned server session alive after local capacity
/// is released. Replacements can therefore exceed the local pool's connection count.
pub struct DbSessionGuard {
    connection: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
    retention_read_open: bool,
}

impl DbSessionGuard {
    /// Wraps the pooled connection before `BEGIN`, so failed or cancelled
    /// transaction startup still releases its pool slot. The caller must include
    /// acquisition in its deadline; this guard installs no separate client timer.
    /// Acquisition errors retain the location that created this future.
    #[track_caller]
    pub fn acquire(pool: &sqlx::PgPool) -> impl Future<Output = Result<Self, DatabaseError>> {
        let loc = Location::caller();
        async move {
            Ok(Self {
                connection: Some(
                    pool.acquire()
                        .await
                        .map_err(|error| DatabaseError::acquire_with_location(error, loc))?,
                ),
                retention_read_open: false,
            })
        }
    }

    /// Starts a top-level `READ COMMITTED` transaction on a newly acquired guard.
    /// Each statement sees intervening commits. The connection must not already
    /// have an open transaction.
    ///
    /// Enables PostgreSQL disconnection checks with a one-second interval,
    /// preserving a shorter nonzero `client_connection_check_interval`. The
    /// setting is transaction-local; it installs no query or total deadline.
    /// Requires server support for `client_connection_check_interval`;
    /// unsupported setup fails the transaction and is returned to the caller.
    /// Detection depends on the server kernel and interrupt processing, so this
    /// does not guarantee immediate cancellation or resolve an in-flight commit.
    #[track_caller]
    pub fn begin_read_committed(
        &mut self,
    ) -> impl Future<Output = Result<Transaction<'_>, DatabaseError>> {
        let loc = Location::caller();
        async move {
            // Keep setup in BEGIN's simple-query batch, without another roundtrip.
            // current_setting avoids materializing pg_settings on each request.
            let query = "BEGIN ISOLATION LEVEL READ COMMITTED;
                         SELECT set_config('client_connection_check_interval',
                             LEAST(NULLIF(EXTRACT(EPOCH FROM
                                 current_setting('client_connection_check_interval')::interval)
                                 * 1000, 0), 1000)::integer::text, true)";
            let inner = sqlx::Connection::begin_with(self.as_pgconn(), query)
                .await
                .map_err(|error| DatabaseError::txn_begin(error, loc))?;
            Ok(Transaction { inner })
        }
    }

    /// Start a read-committed, read-only transaction for retention membership.
    /// `statement_timeout` is truncated to whole milliseconds and must be in
    /// `1..=2147483647` milliseconds. After setup, each statement uses this cap
    /// or a shorter nonzero operator limit; it does not bound transaction lifetime.
    /// Transaction-local idle and transaction expiry are disabled while physical
    /// work owns exclusion. COMMIT restores the session settings. Administrative
    /// termination, connection loss and failover can still release server locks.
    ///
    /// This method borrows the guard: cancelling its future does not discard a
    /// guard the caller retains. Bound acquisition, setup and total work separately,
    /// and drop an abandoned guard so `Drop` discards its connection.
    pub async fn begin_retention_read(
        &mut self,
        statement_timeout: Duration,
    ) -> Result<(), DatabaseError> {
        let timeout_ms = statement_timeout.as_millis();
        if !(1..=i32::MAX as u128).contains(&timeout_ms) {
            return Err(DatabaseError::InvalidArgument(
                "statement timeout must be between 1 and 2147483647 milliseconds".into(),
            ));
        }
        // A cancelled BEGIN can have reached the server. Bare pool return must
        // discard this session until a consuming COMMIT establishes completion.
        self.retention_read_open = true;
        let query = "BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY";
        sqlx::query(query)
            .execute(self.as_pgconn())
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        let query = "SELECT set_config('statement_timeout', LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true) FROM pg_settings WHERE name = 'statement_timeout'";
        sqlx::query(query)
            .bind(timeout_ms.to_string())
            .execute(self.as_pgconn())
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        // PostgreSQL 15 has the idle timer; newer servers may also expose a
        // transaction timer. Neither may expire while physical work continues.
        let query = "SELECT set_config(name, '0', true) FROM pg_settings WHERE name IN ('idle_in_transaction_session_timeout', 'transaction_timeout')";
        sqlx::query(query)
            .execute(self.as_pgconn())
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        Ok(())
    }

    /// Borrow the connection while this guard owns its transaction lifecycle.
    pub fn as_pgconn(&mut self) -> &mut sqlx::PgConnection {
        self.connection
            .as_deref_mut()
            .expect("owned database session")
    }

    /// Consume this guard, commit its retention read and return the healthy
    /// connection to the pool. A failed commit or cancellation of this owned
    /// future drops the guard and discards the connection.
    pub async fn commit_retention_read(mut self) -> Result<(), DatabaseError> {
        let query = "COMMIT";
        sqlx::query(query)
            .execute(self.as_pgconn())
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        self.retention_read_open = false;
        self.return_to_pool();
        Ok(())
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

    /// Returns a healthy connection through SQLx after its transaction commits
    /// or explicitly rolls back.
    /// Call this only after a successful commit or rollback, including rollback
    /// of a completed rejected request. Dropping an abandoned guard discards
    /// its connection instead of waiting for SQLx rollback cleanup.
    /// An unfinished raw retention transaction is always discarded; SQLx has no
    /// `Transaction` owner to roll it back on normal pool return.
    pub fn return_to_pool(mut self) {
        if !self.retention_read_open {
            drop(self.connection.take());
        }
    }
}

impl Drop for DbSessionGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detaching frees the local pool slot immediately. Closing the
            // socket does not guarantee that PostgreSQL stops immediately.
            // Snapshot and retention readers cap statements after setup; write
            // callers retain their PostgreSQL timeout settings. Server sessions
            // can outlive local slots until PostgreSQL detects the disconnect.
            drop(connection.detach());
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Connection as _;

    use super::*;

    #[tokio::test]
    async fn acquisition_errors_retain_caller_location() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://unused/unused")
            .unwrap();
        pool.close().await;

        let caller_line = line!() + 1;
        let acquire = DbSessionGuard::acquire(&pool);
        let DatabaseError::Sqlx(error) = acquire
            .await
            .err()
            .expect("closed pool must fail acquisition")
        else {
            panic!("closed pool should return a SQLx error");
        };
        assert!(matches!(error.source, sqlx::Error::PoolClosed));
        assert_eq!(error.query, "acquire connection");
        assert_eq!(error.file, file!());
        assert_eq!(error.line, caller_line);
    }

    // Setup caps a slower disconnect check without extending an operator's
    // shorter interval. Rollback restores the same session's original setting.
    #[crate::sqlx_test]
    async fn preparation_connection_checks_are_capped_and_transaction_local(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let single = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(pool.connect_options().as_ref().clone())
            .await?;
        let read_ms = "SELECT (EXTRACT(EPOCH FROM
                           current_setting('client_connection_check_interval')::interval)
                           * 1000)::bigint";
        for (operator_value, original_ms, expected_ms) in
            [("2s", 2000_i64, 1000_i64), ("25ms", 25, 25)]
        {
            sqlx::query("SELECT set_config('client_connection_check_interval', $1, false)")
                .bind(operator_value)
                .execute(&single)
                .await?;
            let mut session = super::DbSessionGuard::acquire(&single).await?;
            let mut txn = session.begin_read_committed().await?;
            let actual: i64 = sqlx::query_scalar(read_ms).fetch_one(txn.as_mut()).await?;
            assert_eq!(actual, expected_ms, "{operator_value}");
            txn.rollback().await?;
            session.return_to_pool();
            let restored: i64 = sqlx::query_scalar(read_ms).fetch_one(&single).await?;
            assert_eq!(
                restored, original_ms,
                "{operator_value}: rollback restores the session"
            );
        }
        single.close().await;
        Ok(())
    }

    /// Pause BEGIN after PostgreSQL enters the transaction, while SQLx still
    /// waits for its final ReadyForQuery. Cancellation must discard that session
    /// and let a fresh borrower use the single local pool slot before gate release.
    #[crate::sqlx_test]
    async fn cancelled_begin_discards_connection_before_pool_reuse(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let single = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await?;
        let mut gate = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(8907052)")
            .execute(gate.as_mut())
            .await?;
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await?;
        let mut session = DbSessionGuard::acquire(&single).await?;
        let abandoned_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(session.as_pgconn())
            .await?;
        {
            // One BEGIN batch delays ReadyForQuery without a network proxy or
            // a timing guess. The local setting also detects accidental reuse.
            let startup = async move {
                let txn = session
                    .as_pgconn()
                    .begin_with(
                        "BEGIN; SET LOCAL nico.cancelled_begin = 'yes'; \
                         SELECT pg_advisory_xact_lock(8907052)",
                    )
                    .await?;
                txn.rollback().await?;
                session.return_to_pool();
                Ok::<_, sqlx::Error>(())
            };
            tokio::pin!(startup);
            let blocked_pid = tokio::select! {
                result = &mut startup => panic!("BEGIN must still be waiting: {result:?}"),
                pid = crate::test_support::postgres::wait_for_blocked_query(
                    &pool, gate_pid, "BEGIN; SET LOCAL nico.cancelled_begin",
                ) => pid,
            };
            assert_eq!(blocked_pid, abandoned_pid);
        }
        assert_eq!(single.size(), 0);
        let (replacement_pid, marker): (i32, Option<String>) = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_as(
                "SELECT pg_backend_pid(), current_setting('nico.cancelled_begin', true)",
            )
            .fetch_one(&single),
        )
        .await??;
        assert_ne!(replacement_pid, abandoned_pid);
        assert_eq!(marker, None);
        gate.rollback().await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid = $1)",
                )
                .bind(abandoned_pid)
                .fetch_one(&pool)
                .await?;
                if !exists {
                    return Ok::<_, sqlx::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        single.close().await;
        Ok(())
    }

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

    /// Retention reads use fresh statements, cap SQL without extending a tighter
    /// site limit, and disable expiry only until their consuming COMMIT. Bare
    /// pool return must never lend an unfinished raw transaction to a borrower.
    #[crate::sqlx_test]
    async fn retention_settings_reset_and_unfinished_sessions_are_discarded(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await?;
        for (configured_ms, cap, expected_ms) in [
            // A disabled limit uses the requested cap, truncated to whole ms.
            (0, Duration::from_micros(2_500_999), 2500_i64),
            // A broader operator limit cannot extend this read.
            (60000, Duration::from_secs(5), 5000),
            // A tighter operator limit remains authoritative for SQL statements.
            (1000, Duration::from_secs(5), 1000),
        ] {
            let mut session = DbSessionGuard::acquire(&pool).await?;
            sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                .bind(configured_ms.to_string())
                .execute(session.as_pgconn())
                .await?;
            sqlx::query("SELECT set_config(name, '250', false) FROM pg_settings WHERE name IN ('idle_in_transaction_session_timeout', 'transaction_timeout')")
                .execute(session.as_pgconn()).await?;
            let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(session.as_pgconn())
                .await?;
            session.begin_retention_read(cap).await?;
            let (read_only, isolation): (bool, String) = sqlx::query_as(
                "SELECT current_setting('transaction_read_only')::bool, current_setting('transaction_isolation')",
            ).fetch_one(session.as_pgconn()).await?;
            assert!(read_only);
            assert_eq!(isolation, "read committed");
            let effective_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(session.as_pgconn())
            .await?;
            assert_eq!(effective_ms, expected_ms);
            let expiry: Vec<(String, i64)> = sqlx::query_as(
                "SELECT name, setting::bigint FROM pg_settings WHERE name IN ('idle_in_transaction_session_timeout', 'transaction_timeout')",
            ).fetch_all(session.as_pgconn()).await?;
            assert!(!expiry.is_empty());
            assert!(expiry.iter().all(|(_, limit)| *limit == 0), "{expiry:?}");
            session.commit_retention_read().await?;

            // Check the same backend, not a replacement with fresh defaults.
            let (after, timeout, idle, read_only): (i32, i64, i64, bool) = sqlx::query_as(
                "SELECT pg_backend_pid(), (SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'), (SELECT setting::bigint FROM pg_settings WHERE name = 'idle_in_transaction_session_timeout'), current_setting('transaction_read_only')::bool",
            ).fetch_one(&pool).await?;
            assert_eq!(after, backend);
            assert_eq!(timeout, configured_ms);
            assert_eq!(idle, 250);
            assert!(!read_only);
        }
        // SQLx cannot roll back a raw BEGIN on ordinary pool return. This guard
        // must detach it even when a caller chooses the wrong completion method.
        let mut unfinished = DbSessionGuard::acquire(&pool).await?;
        let before: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(unfinished.as_pgconn())
            .await?;
        unfinished
            .begin_retention_read(Duration::from_secs(5))
            .await?;
        crate::nic_firmware_artifact::lock_retention(unfinished.as_pgconn()).await?;
        unfinished.return_to_pool();
        assert_eq!(pool.size(), 0);
        let (after, read_only): (i32, bool) = sqlx::query_as(
            "SELECT pg_backend_pid(), current_setting('transaction_read_only')::bool",
        )
        .fetch_one(&pool)
        .await?;
        assert_ne!(after, before);
        assert!(!read_only);
        pool.close().await;
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
            assert!(matches!(
                connection.begin_retention_read(cap).await,
                Err(DatabaseError::InvalidArgument(_))
            ));
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
