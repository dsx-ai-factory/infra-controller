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
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::{Event, LabelValue, emit};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tracing::Instrument;

use crate::{DatabaseError, DatabaseResult};

pub type WorkKey = String;
pub type WorkerId = uuid::Uuid;

#[derive(Debug, thiserror::Error)]
#[error("WorkLockManager requires a writable database connection")]
struct ReadOnlyWorkLockConnection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum WorkLockOperation {
    Release,
    KeepAlive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum WorkLockFailure {
    Database,
    CommandDispatch,
    CommandReply,
    LockLost,
}

/// A work lock could not be kept or given back. Each variant is one boundary
/// where that happens, and pins the `(operation, failure)` pair that names it.
///
/// One pairing is worth remembering: `(KeepAlive, LockLost)` belongs to the
/// keepalive loop's `Err(KeepAliveError::LockLost)` arm, which returns before
/// the general `Err(e)` arm can see it. That arm ordering is what keeps
/// `Keepalive` from ever carrying `LockLost` -- reorder it and two boundaries
/// would land on one label pair with different wording.
#[derive(Event)]
#[event(
    event_name = "work_lock_failed",
    metric_name = "carbide_work_lock_failures_total",
    component = "nico-api",
    metric = counter,
    log = error,
    describe = "Number of work-lock lifecycle failures, by operation and failure kind.",
    labels(operation: WorkLockOperation, failure: WorkLockFailure),
)]
enum WorkLockFailed {
    /// The release write itself failed. `failure` is data here: a
    /// `FailedPrecondition` means the lock had already expired, anything else
    /// is a database problem.
    #[event(labels(operation = Release), message = "Could not release work lock")]
    Release {
        #[label]
        failure: WorkLockFailure,
        #[context]
        work_key: WorkKey,
        #[context]
        worker_id: WorkerId,
        #[context]
        error: String,
    },

    /// The release command could not be queued because the manager is gone.
    #[event(
        labels(operation = Release, failure = CommandDispatch),
        message = "Could not release work lock: the WorkLockManager has shut down"
    )]
    ReleaseDispatch {
        #[context]
        work_key: WorkKey,
        #[context]
        worker_id: WorkerId,
        #[context]
        error: String,
    },

    /// The keepalive loop learned the lock is no longer ours.
    #[event(
        labels(operation = KeepAlive, failure = LockLost),
        message = "worker lost lock"
    )]
    LockLost {
        #[context]
        work_key: WorkKey,
        #[context]
        worker_id: WorkerId,
        #[context]
        error: String,
    },

    /// A keepalive attempt failed for a reason that leaves the lock held, so
    /// the loop retries.
    #[event(
        labels(operation = KeepAlive),
        message = "Failed to send work-lock keepalive; retrying"
    )]
    Keepalive {
        #[label]
        failure: WorkLockFailure,
        #[context]
        work_key: WorkKey,
        #[context]
        worker_id: WorkerId,
        #[context]
        error: String,
    },
}

impl WorkLockFailure {
    /// How a release write failed, as the metric label.
    fn from_release_error(error: &DatabaseError) -> Self {
        match error {
            DatabaseError::FailedPrecondition(_) => Self::LockLost,
            _ => Self::Database,
        }
    }
}

/// A WorkLockManager buffers this many acquisition and keepalive commands: This would only be
/// exceeded if something goes very wrong with the database.
static COMMAND_BUFFER_SIZE: usize = 100;

/// A clone-able handle to a (singleton, global) [`crate::work_lock_manager`] work loop.
///
/// This is used to logically "lock" units of work so that they are only done once at a time,
/// without the overhead of using a postgres advisory lock for every unit of work. Advisory locks
/// require holding a long-running connection to postgres, and are released when the connection is
/// released, which leads to long-lived connections occupying slots in the sqlx pool. Since logical
/// "work" can take a long time, especially when we have to make calls to (unreliable) external
/// services while holding the lock, a WorkLockManager instead does an atomic write to a
/// `work_locks` table, vending [`WorkLock`] objects back, which release the lock on Drop. In case
/// of a crash where drop is not called, each work lock expires after a time interval.
///
/// This is a lease, not a fencing token. Exclusivity lasts while keepalives
/// retain the lease; after expiry, another worker can acquire the key while old
/// code is still running. PostgreSQL mutations can use
/// [`WorkLock::fence_transaction`]; external side effects need their own
/// fencing or idempotency mechanism.
///
/// This is returned by [`start`], and can be used to communicate to acquire [`WorkLock`] items for doing
#[derive(Clone)]
pub struct WorkLockManagerHandle {
    keepalive_interval: Duration,
    cmd_tx: mpsc::UnboundedSender<QueuedWorkLockManagerCommand>,
    command_slots: Arc<Semaphore>,
}

#[derive(Clone, Copy)]
pub struct KeepaliveConfig {
    /// For any WorkLocks held, they send a keep-alive for their lock at this interval until they're dropped.
    pub interval: Duration,
    /// For any WorkLocks held, if they haven't sent a keep-alive in this long, they've expired.
    pub timeout: Duration,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(60),
        }
    }
}

/// Start a work manager in the background. This should only be done once per carbide instance.
///
/// To actually interact with the global work manager, use [`WorkLockManagerHandle`] (returned by this
/// function.)
///
/// This exists as a singleton message loop (instead of just a collection of database methods) for
/// two reasons:
///
/// 1) So that a dedicated, single-connection pool can keep lock-table operations independent of
///    the main application pool
/// 2) To avoid race conditions, so that locks can be released effectively "immediately" in
///    [`WorkLock`]'s Drop impl (by placing the release command on the shared FIFO without consuming
///    a bounded command slot), such that the next call to
///    [`WorkLockManagerHandle::try_acquire_lock`] is guaranteed to be processed after the lock is
///    released.
pub async fn start(
    join_set: &mut JoinSet<()>,
    pool: PgPool,
    keepalive_config: KeepaliveConfig,
) -> DatabaseResult<WorkLockManagerHandle> {
    let pool = create_work_lock_pool(&pool).await?;

    let KeepaliveConfig {
        interval: keepalive_interval,
        timeout: keepalive_timeout,
    } = keepalive_config;

    // All operations share one FIFO so a release stays ordered before the caller's next acquire.
    // The channel is unbounded because `WorkLock::drop` cannot wait for capacity. A semaphore
    // retains bounded backpressure for acquisition and keepalive commands.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let command_slots = Arc::new(Semaphore::new(COMMAND_BUFFER_SIZE));
    join_set
        .build_task()
        .name("WorkLockManager")
        // Note: don't inherit the callers span, since child spans can't outlive their parent.
        // This prevents a crash in tracing-subscriber.
        .spawn(
            run_loop(pool, cmd_rx, keepalive_timeout)
                .instrument(tracing::debug_span!(parent: None, "WorklockManager::run_loop")),
        )
        .expect("failed to start work manager");

    Ok(WorkLockManagerHandle {
        cmd_tx,
        command_slots,
        keepalive_interval,
    })
}

/// Create a pool reserved for WorkLockManager operations.
///
/// The pool owns one eagerly opened connection and replaces it when SQLx detects a failed health
/// check or reaches a configured connection-lifecycle limit. Both new and reused connections must
/// be writable, so a connection to a server that has become a read-only standby is rejected before
/// it can be used to update the work-lock table.
async fn create_work_lock_pool(pool: &PgPool) -> DatabaseResult<PgPool> {
    let options = pool.options();
    PgPoolOptions::new()
        .min_connections(1)
        .max_connections(1)
        .acquire_timeout(options.get_acquire_timeout())
        .idle_timeout(options.get_idle_timeout())
        .max_lifetime(options.get_max_lifetime())
        // The writability query below also proves the connection is responsive.
        .test_before_acquire(false)
        .after_connect(|db, _metadata| {
            Box::pin(async move { ensure_work_lock_connection_is_writable(db).await })
        })
        .before_acquire(|db, _metadata| {
            Box::pin(async move {
                match ensure_work_lock_connection_is_writable(db).await {
                    Ok(()) => Ok(true),
                    Err(_) => Ok(false),
                }
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .map_err(DatabaseError::acquire)
}

async fn ensure_work_lock_connection_is_writable(db: &mut PgConnection) -> sqlx::Result<()> {
    let read_only = sqlx::query_scalar("SELECT current_setting('transaction_read_only')::bool")
        .fetch_one(db)
        .await?;

    if read_only {
        tracing::warn!("Rejecting read-only WorkLockManager database connection");
        return Err(sqlx::Error::Configuration(Box::new(
            ReadOnlyWorkLockConnection,
        )));
    }

    Ok(())
}

async fn run_loop(
    pool: PgPool,
    mut cmd_rx: mpsc::UnboundedReceiver<QueuedWorkLockManagerCommand>,
    keepalive_timeout: Duration,
) {
    while let Some(QueuedWorkLockManagerCommand {
        command,
        command_slot,
    }) = cmd_rx.recv().await
    {
        // Match bounded-channel behavior by returning capacity as soon as a command is dequeued.
        drop(command_slot);

        match command {
            WorkLockManagerCommand::AcquireLock { work_key, reply_tx } => {
                if reply_tx.is_closed() {
                    tracing::info!("Skipping AcquireLock command: caller already timed out");
                    continue;
                }
                match try_acquire_lock(&pool, &work_key, keepalive_timeout).await {
                    Ok(Some(worker_id)) => {
                        reply_tx.send(Ok(worker_id)).ok();
                        tracing::debug!(
                            work_key = %work_key,
                            "Acquired work lock",
                        );
                    }
                    Ok(None) => {
                        reply_tx
                            .send(Err(AcquireLockError::WorkAlreadyLocked(work_key)))
                            .ok();
                    }
                    Err(e) => {
                        reply_tx.send(Err(e.into())).ok();
                    }
                }
            }

            WorkLockManagerCommand::ReleaseLock(WorkLockReleaseCommand {
                work_key,
                worker_id,
                reply_tx,
            }) => {
                let result = release_lock(&pool, &work_key, worker_id)
                    .await
                    .inspect_err(|e| {
                        emit(WorkLockFailed::Release {
                            work_key: work_key.clone(),
                            worker_id,
                            failure: WorkLockFailure::from_release_error(e),
                            error: e.to_string(),
                        });
                    });
                if result.is_ok() {
                    tracing::debug!(%work_key, "Released work lock");
                }
                if let Some(reply_tx) = reply_tx {
                    reply_tx.send(result).ok();
                }
            }

            WorkLockManagerCommand::KeepLockAlive {
                work_key,
                worker_id,
                reply_tx,
            } => match keep_lock_alive(&pool, &work_key, worker_id).await {
                Ok(()) => {
                    reply_tx.send(Ok(())).ok();
                }
                Err(DatabaseError::FailedPrecondition(msg)) => {
                    reply_tx.send(Err(KeepAliveError::LockLost(msg))).ok();
                }
                Err(e) => {
                    reply_tx.send(Err(e.into())).ok();
                }
            },
        }
    }
    tracing::info!("WorkLockManager: all handles dropped, shutting down");
}

/// A lock representing exclusive ownership of a logical, named unit of work. Upon drop, the lock
/// will be released (assuming the global [`crate::work_lock_manager`] is healthy.)
pub struct WorkLock {
    // When this is dropped, the keepalive loop will exit.
    keepalive_stop_tx: Option<oneshot::Sender<()>>,
    #[cfg(test)]
    join_handle: tokio::task::JoinHandle<()>,
    manager: WorkLockManagerHandle,
    work_key: WorkKey,
    worker_id: WorkerId,
    release_on_drop: bool,
}

impl Drop for WorkLock {
    fn drop(&mut self) {
        // Let the keepalive loop stop.
        self.keepalive_stop_tx.take();
        if !self.release_on_drop {
            return;
        }

        tracing::debug!(
            work_key = %self.work_key,
            worker_id = %self.worker_id,
            "Releasing work lock",
        );

        // Queue the release. Callers that will immediately shut down the
        // manager can use `release` to wait for the database acknowledgment.
        self.manager
            .send_release_command(WorkLockReleaseCommand {
                work_key: self.work_key.clone(),
                worker_id: self.worker_id,
                reply_tx: None,
            })
            .inspect_err(|e| {
                emit(WorkLockFailed::ReleaseDispatch {
                    work_key: self.work_key.clone(),
                    worker_id: self.worker_id,
                    error: e.to_string(),
                });
            })
            .ok();
    }
}

impl WorkLock {
    fn new(
        manager: WorkLockManagerHandle,
        work_key: WorkKey,
        worker_id: WorkerId,
        keepalive_interval: Duration,
    ) -> Self {
        let (keepalive_stop_tx, mut keepalive_stop_rx) = oneshot::channel();
        let join_handle = tokio::task::Builder::new()
            .name(&format!("keepalive loop for {work_key} worker {worker_id}"))
            .spawn({
                let manager = manager.clone();
                let work_key = work_key.clone();
                let mut keepalive_timer = tokio::time::interval(keepalive_interval);
                keepalive_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
                let fut = async move {
                    loop {
                        tokio::select! {
                            biased;
                            _ = &mut keepalive_stop_rx => {
                                break;
                            }
                            _ = keepalive_timer.tick() => {
                                match manager.keep_lock_alive(work_key.clone(), worker_id).await {
                                    Ok(_) => {}
                                    Err(KeepAliveError::LockLost(msg)) => {
                                        // A late lock-loss response is no longer actionable once
                                        // the owning `WorkLock` is being dropped.
                                        if matches!(
                                            keepalive_stop_rx.try_recv(),
                                            Err(oneshot::error::TryRecvError::Empty)
                                        ) {
                                            emit(WorkLockFailed::LockLost { work_key, worker_id, error: msg });
                                        }
                                        return;
                                    }
                                    Err(e) => {
                                        emit(WorkLockFailed::Keepalive { failure: e.failure(), work_key: work_key.clone(), worker_id, error: e.to_string() });
                                    }
                                }
                            }
                        }
                    }
                };
                // Note: don't inherit the callers span, since child spans can't outlive their parent.
                // This prevents a crash in tracing-subscriber.
                fut.instrument(tracing::debug_span!(parent: None, "WorkLock keepalive loop"))
            })
            .expect("could not spawn tokio task");

        if !cfg!(test) {
            _ = join_handle;
        }

        WorkLock {
            keepalive_stop_tx: Some(keepalive_stop_tx),
            manager,
            work_key,
            worker_id,
            release_on_drop: true,
            #[cfg(test)]
            join_handle,
        }
    }

    /// Release this lock and wait until the manager processes the database
    /// deletion.
    ///
    /// Dropping a lock normally queues the same deletion. Use this method when
    /// the caller may shut down the manager immediately afterward.
    pub async fn release(mut self) -> Result<(), ReleaseLockError> {
        self.keepalive_stop_tx.take();

        let (reply_tx, reply_rx) = oneshot::channel();
        // Explicit release owns the attempt from here. A dispatch failure means
        // the receiver is gone, so Drop cannot recover by sending it again.
        self.release_on_drop = false;
        self.manager
            .send_release_command(WorkLockReleaseCommand {
                work_key: self.work_key.clone(),
                worker_id: self.worker_id,
                reply_tx: Some(reply_tx),
            })
            .inspect_err(|error| {
                emit(WorkLockFailed::ReleaseDispatch {
                    work_key: self.work_key.clone(),
                    worker_id: self.worker_id,
                    error: error.to_string(),
                });
            })
            .map_err(|error| ReleaseLockError::WorkLockManagerSend(error.to_string()))?;

        reply_rx.await??;
        Ok(())
    }

    /// Fence database writes performed under this lock.
    ///
    /// This takes a key-share lock on the `work_locks` row until `txn` ends and
    /// verifies that it still names this worker. A replacement acquisition
    /// changes `worker_id`, which is part of
    /// `idx_work_locks_on_worker_id_and_key`, so PostgreSQL must wait for the
    /// fence before it can update that key. Deletion waits too, while the
    /// manager's non-key `last_keepalive` updates can continue normally.
    ///
    /// Nominal lease expiry without takeover is allowed: locking this row
    /// serializes any later takeover behind the transaction, and callers must
    /// finish all protected writes before committing it.
    ///
    /// Keep the transaction short and free of external I/O. A replica that
    /// tries to acquire this key waits for the fence in its single manager
    /// loop, which also delays that replica's unrelated lock commands.
    pub async fn fence_transaction(&self, txn: &mut PgTransaction<'_>) -> DatabaseResult<()> {
        let query = r#"
SELECT true
FROM work_locks
WHERE work_key = $1 AND worker_id = $2
FOR KEY SHARE
        "#;
        let still_held: Option<bool> = sqlx::query_scalar(query)
            .bind(&self.work_key)
            .bind(self.worker_id)
            .fetch_optional(&mut **txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        if still_held.is_none() {
            return Err(DatabaseError::FailedPrecondition(format!(
                "work lock is no longer held for work_key={}, worker_id={}",
                self.work_key, self.worker_id,
            )));
        }

        Ok(())
    }

    #[cfg(test)]
    pub fn is_alive(&self) -> bool {
        !self.join_handle.is_finished()
    }
}

/// Try to acquire a lock for `work_key`o
///
/// Returns `Some(WorkerId)` if the lock was acquired, or `None` if the lock is already being held.
async fn try_acquire_lock(
    pool: &PgPool,
    work_key: &WorkKey,
    keepalive_timeout: Duration,
) -> DatabaseResult<Option<WorkerId>> {
    // Try to acquire the lock if it either doesn't exist, or exists but is expired.
    let query = r#"
WITH upsert AS (
    INSERT INTO work_locks (work_key)
    VALUES ($1)
    ON CONFLICT (work_key)
    DO UPDATE
        SET worker_id          = EXCLUDED.worker_id,
            started            = now(),
            last_keepalive     = now()
        WHERE work_locks.last_keepalive + $2::interval < now()
    RETURNING work_locks.worker_id AS worker_id
)
SELECT worker_id FROM upsert;
    "#;

    sqlx::query_scalar(query)
        .bind(work_key)
        .bind(keepalive_timeout)
        .fetch_optional(pool)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

async fn release_lock(
    pool: &PgPool,
    work_key: &WorkKey,
    worker_id: WorkerId,
) -> DatabaseResult<()> {
    let query = r#"
DELETE FROM work_locks WHERE work_key = $1 AND worker_id = $2 RETURNING work_key
    "#;

    let deleted = sqlx::query_scalar::<_, WorkKey>(query)
        .bind(work_key)
        .bind(worker_id)
        .fetch_all(pool)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if deleted.is_empty() {
        return Err(DatabaseError::FailedPrecondition(format!(
            "Tried to release nonexistent lock for work_key={}, worker_id={}",
            work_key, worker_id,
        )));
    }

    Ok(())
}

async fn keep_lock_alive(
    pool: &PgPool,
    work_key: &WorkKey,
    worker_id: WorkerId,
) -> DatabaseResult<()> {
    let query = r#"
UPDATE work_locks SET last_keepalive = now() WHERE work_key = $1 AND worker_id = $2 RETURNING work_key
    "#;

    let updated = sqlx::query_scalar::<_, WorkKey>(query)
        .bind(work_key)
        .bind(worker_id)
        .fetch_all(pool)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if updated.is_empty() {
        return Err(DatabaseError::FailedPrecondition(format!(
            // If this happens, the worker must have been alive (since the WorkLock was still in
            // scope), but didn't send keep-alives within the healthy ping interval. This is a bug,
            // becauase the ping interval should be tuned to account for the maximum amount of time
            // work should take (taking timeouts into account, etc.)
            "BUG: Tried to keep alive nonexistent lock for work_key={}, worker_id={} worker likely was not sending keep-alives frequently enough.",
            work_key, worker_id,
        )));
    }

    Ok(())
}

impl WorkLockManagerHandle {
    fn try_send_bounded_command(
        &self,
        command: WorkLockManagerCommand,
    ) -> Result<(), CommandDispatchError> {
        let command_slot = self
            .command_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| CommandDispatchError::NoCapacity)?;
        self.cmd_tx
            .send(QueuedWorkLockManagerCommand {
                command,
                command_slot: Some(command_slot),
            })
            .map_err(|_| CommandDispatchError::ManagerShutdown)
    }

    fn send_release_command(
        &self,
        command: WorkLockReleaseCommand,
    ) -> Result<(), CommandDispatchError> {
        self.cmd_tx
            .send(QueuedWorkLockManagerCommand {
                command: WorkLockManagerCommand::ReleaseLock(command),
                command_slot: None,
            })
            .map_err(|_| CommandDispatchError::ManagerShutdown)
    }

    pub async fn try_acquire_lock(&self, work_key: WorkKey) -> Result<WorkLock, AcquireLockError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.try_send_bounded_command(WorkLockManagerCommand::AcquireLock {
            work_key: work_key.clone(),
            reply_tx,
        })
        .map_err(|e| AcquireLockError::WorkLockManagerSend(e.to_string()))?;

        let worker_id = reply_rx.await??;

        Ok(WorkLock::new(
            self.clone(),
            work_key,
            worker_id,
            self.keepalive_interval,
        ))
    }

    async fn keep_lock_alive(
        &self,
        work_key: WorkKey,
        worker_id: WorkerId,
    ) -> Result<(), KeepAliveError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.try_send_bounded_command(WorkLockManagerCommand::KeepLockAlive {
            work_key,
            worker_id,
            reply_tx,
        })
        .map_err(|e| KeepAliveError::WorkLockManagerSend(e.to_string()))?;

        reply_rx.await??;

        Ok(())
    }
}

enum WorkLockManagerCommand {
    AcquireLock {
        work_key: WorkKey,
        reply_tx: oneshot::Sender<Result<WorkerId, AcquireLockError>>,
    },
    KeepLockAlive {
        work_key: WorkKey,
        worker_id: WorkerId,
        reply_tx: oneshot::Sender<Result<(), KeepAliveError>>,
    },
    ReleaseLock(WorkLockReleaseCommand),
}

struct QueuedWorkLockManagerCommand {
    command: WorkLockManagerCommand,
    command_slot: Option<OwnedSemaphorePermit>,
}

/// A command sent without a capacity slot so dropping a `WorkLock` cannot fail under load.
struct WorkLockReleaseCommand {
    work_key: WorkKey,
    worker_id: WorkerId,
    reply_tx: Option<oneshot::Sender<DatabaseResult<()>>>,
}

#[derive(Debug, thiserror::Error)]
enum CommandDispatchError {
    #[error("no available capacity; database is likely overloaded")]
    NoCapacity,
    #[error("the WorkLockManager has shut down")]
    ManagerShutdown,
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireLockError {
    #[error("work is already locked for {0}")]
    WorkAlreadyLocked(WorkKey),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// This happens when the bounded command capacity is exhausted or the WorkLockManager has shut
    /// down before the command can be delivered. Exhausting capacity means COMMAND_BUFFER_SIZE
    /// acquisition and keepalive commands are waiting for the WorkLockManager to process them.
    /// Since the manager owns a long-running database connection, that generally means the database
    /// is unavailable or simple updates to the table are blocked.
    #[error("error sending AcquireLock command to WorkLockManager: {0}")]
    WorkLockManagerSend(String),
    #[error(
        "BUG: error receiving AcquireLock reply from WorkLockManager, database connections are likely failing: {0}"
    )]
    WorkLockManagerReply(#[from] tokio::sync::oneshot::error::RecvError),
    #[error(transparent)]
    Timeout(#[from] tokio::time::error::Elapsed),
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseLockError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("error sending ReleaseLock command to WorkLockManager: {0}")]
    WorkLockManagerSend(String),
    #[error(
        "error receiving ReleaseLock reply from WorkLockManager, database connections are likely failing; the lease will expire instead: {0}"
    )]
    WorkLockManagerReply(#[from] tokio::sync::oneshot::error::RecvError),
}

#[derive(Debug, thiserror::Error)]
pub enum KeepAliveError {
    #[error("{0}")]
    LockLost(String),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// See notes in AcquireLockError::WorkLockManagerSend
    #[error("error sending KeepAlive command to WorkLockManager: {0}")]
    WorkLockManagerSend(String),
    #[error(
        "BUG: error receiving KeepAlive reply from WorkLockManager, database connections are likely failing: {0}"
    )]
    WorkLockManagerReply(#[from] tokio::sync::oneshot::error::RecvError),
}

impl KeepAliveError {
    fn failure(&self) -> WorkLockFailure {
        match self {
            Self::LockLost(_) => WorkLockFailure::LockLost,
            Self::Database(_) => WorkLockFailure::Database,
            Self::WorkLockManagerSend(_) => WorkLockFailure::CommandDispatch,
            Self::WorkLockManagerReply(_) => WorkLockFailure::CommandReply,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::time::Instant;

    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::{Check, check_values};
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    const WORK_LOCK_FAILURES_METRIC: &str = "carbide_work_lock_failures_total";

    #[tokio::test]
    async fn keepalive_errors_map_to_bounded_failures() {
        let (reply_tx, reply_rx) = oneshot::channel::<()>();
        drop(reply_tx);
        let reply_error = reply_rx
            .await
            .expect_err("closed reply channel should fail");

        check_values(
            [
                Check {
                    scenario: "database failure",
                    input: KeepAliveError::Database(DatabaseError::Internal {
                        message: "database unavailable".to_string(),
                    }),
                    expect: WorkLockFailure::Database,
                },
                Check {
                    scenario: "command dispatch failure",
                    input: KeepAliveError::WorkLockManagerSend("no available capacity".to_string()),
                    expect: WorkLockFailure::CommandDispatch,
                },
                Check {
                    scenario: "command reply failure",
                    input: KeepAliveError::WorkLockManagerReply(reply_error),
                    expect: WorkLockFailure::CommandReply,
                },
                Check {
                    scenario: "lock lost",
                    input: KeepAliveError::LockLost("lock expired".to_string()),
                    expect: WorkLockFailure::LockLost,
                },
            ],
            |error| error.failure(),
        );
    }

    #[tokio::test]
    async fn command_dispatch_errors_distinguish_capacity_and_shutdown() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let manager = WorkLockManagerHandle {
            keepalive_interval: Duration::from_secs(60),
            cmd_tx,
            command_slots: Arc::new(Semaphore::new(COMMAND_BUFFER_SIZE)),
        };
        let command = || {
            let (reply_tx, _reply_rx) = oneshot::channel();
            WorkLockManagerCommand::AcquireLock {
                work_key: "dispatch".to_string(),
                reply_tx,
            }
        };

        let command_slots = manager
            .command_slots
            .clone()
            .acquire_many_owned(COMMAND_BUFFER_SIZE as u32)
            .await
            .unwrap();
        assert_eq!(
            manager
                .try_send_bounded_command(command())
                .unwrap_err()
                .to_string(),
            "no available capacity; database is likely overloaded"
        );

        drop(command_slots);
        drop(cmd_rx);
        assert_eq!(
            manager
                .try_send_bounded_command(command())
                .unwrap_err()
                .to_string(),
            "the WorkLockManager has shut down"
        );
    }

    #[tokio::test]
    async fn dropping_work_lock_suppresses_in_flight_lock_lost() {
        let metrics = MetricsCapture::start();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let manager = WorkLockManagerHandle {
            keepalive_interval: Duration::from_secs(60),
            cmd_tx,
            command_slots: Arc::new(Semaphore::new(COMMAND_BUFFER_SIZE)),
        };
        let worker_id = WorkerId::nil();
        let lock = WorkLock::new(
            manager,
            "teardown".to_string(),
            worker_id,
            Duration::from_secs(60),
        );

        let QueuedWorkLockManagerCommand {
            command,
            command_slot,
        } = tokio::time::timeout(Duration::from_secs(3), cmd_rx.recv())
            .await
            .expect("keepalive command was not sent")
            .expect("keepalive command channel closed");
        drop(command_slot);
        let WorkLockManagerCommand::KeepLockAlive { reply_tx, .. } = command else {
            panic!("first command was not a keepalive");
        };

        drop(lock);
        let release = tokio::time::timeout(Duration::from_secs(3), cmd_rx.recv())
            .await
            .expect("release command was not sent")
            .expect("release command channel closed");
        assert!(matches!(
            release.command,
            WorkLockManagerCommand::ReleaseLock(_)
        ));

        reply_tx
            .send(Err(KeepAliveError::LockLost("lock expired".to_string())))
            .expect("keepalive task stopped before receiving its reply");
        assert!(
            tokio::time::timeout(Duration::from_secs(3), cmd_rx.recv())
                .await
                .expect("keepalive task did not stop")
                .is_none()
        );
        assert_eq!(
            metrics.counter_delta(
                WORK_LOCK_FAILURES_METRIC,
                &[("operation", "keep_alive"), ("failure", "lock_lost")],
            ),
            0.0
        );
    }

    #[crate::sqlx_test]
    async fn test_exclusivity(pool: PgPool) {
        let mut join_set = JoinSet::new();
        {
            let manager = start(&mut join_set, pool, Default::default())
                .await
                .unwrap();

            let lock_1 = manager.try_acquire_lock("work_key_1".into()).await.unwrap();
            assert!(
                manager.try_acquire_lock("work_key_1".into()).await.is_err(),
                "Should not be able to acquire another lock while one is active"
            );
            std::mem::drop(lock_1);

            let _lock_1 = manager.try_acquire_lock("work_key_1".into()).await.expect(
                "Should be able to acquire a lock again if the other has gone out of scope",
            );
            let _lock_2 = manager.try_acquire_lock("work_key_2".into()).await.expect(
                "Should be able to acquire a lock with a different key while another is active",
            );

            // Make sure drops release locks in-order, before acquires are seen, and that the command
            // buffer doesn't become full over the course (we should be awaiting the replies, which
            // should not cause it to grow.)
            for i in 0..(COMMAND_BUFFER_SIZE * 2) {
                if manager.try_acquire_lock("work_key_3".into()).await.is_err() {
                    panic!(
                        "Lock failed to be acquired after the previous was dropped, after {i} iterations"
                    )
                }
                // lock is already dropped
            }
        }

        // Test cooperative cancellation
        tokio::select! {
            _ = join_set.join_all() => {}
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                panic!("WorkLockManager did not shut down in a timely manner")
            }
        }
    }

    #[crate::sqlx_test]
    async fn explicit_release_waits_for_database_deletion(pool: PgPool) {
        let mut join_set = JoinSet::new();
        let manager = start(&mut join_set, pool.clone(), Default::default())
            .await
            .expect("start work lock manager");
        let work_key = "acknowledged-release".to_string();
        let work_lock = manager
            .try_acquire_lock(work_key.clone())
            .await
            .expect("acquire work lock");

        work_lock
            .release()
            .await
            .expect("release work lock with acknowledgment");
        join_set.abort_all();
        drop(join_set);

        let query = "SELECT count(*) FROM work_locks WHERE work_key = $1";
        let row_count: i64 = sqlx::query_scalar(query)
            .bind(work_key)
            .fetch_one(&pool)
            .await
            .expect("check released work lock");
        assert_eq!(
            row_count, 0,
            "release acknowledgment returned before the work_locks row was deleted"
        );
    }

    #[crate::sqlx_test]
    async fn commands_and_releases_keep_fifo_order(pool: PgPool) {
        let keepalive_config = KeepaliveConfig::default();
        let work_key = "ordered".to_string();
        let worker_id = try_acquire_lock(&pool, &work_key, keepalive_config.timeout)
            .await
            .unwrap()
            .unwrap();

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let manager = WorkLockManagerHandle {
            keepalive_interval: keepalive_config.interval,
            cmd_tx,
            command_slots: Arc::new(Semaphore::new(COMMAND_BUFFER_SIZE)),
        };

        let (keepalive_reply_tx, keepalive_reply_rx) = oneshot::channel();
        manager
            .try_send_bounded_command(WorkLockManagerCommand::KeepLockAlive {
                work_key: work_key.clone(),
                worker_id,
                reply_tx: keepalive_reply_tx,
            })
            .unwrap();
        manager
            .send_release_command(WorkLockReleaseCommand {
                work_key: work_key.clone(),
                worker_id,
                reply_tx: None,
            })
            .unwrap();
        let (acquire_reply_tx, acquire_reply_rx) = oneshot::channel();
        manager
            .try_send_bounded_command(WorkLockManagerCommand::AcquireLock {
                work_key: work_key.clone(),
                reply_tx: acquire_reply_tx,
            })
            .unwrap();

        let mut join_set = JoinSet::new();
        join_set.spawn(run_loop(pool, cmd_rx, keepalive_config.timeout));

        keepalive_reply_rx
            .await
            .unwrap()
            .expect("a keepalive queued before a release must run first");
        let replacement_worker_id = acquire_reply_rx
            .await
            .unwrap()
            .expect("a release queued before an acquire must run first");

        manager
            .send_release_command(WorkLockReleaseCommand {
                work_key,
                worker_id: replacement_worker_id,
                reply_tx: None,
            })
            .unwrap();
        drop(manager);
        tokio::time::timeout(Duration::from_secs(3), join_set.join_all())
            .await
            .expect("WorkLockManager did not shut down in a timely manner");
    }

    #[crate::sqlx_test]
    async fn release_is_not_dropped_when_command_queue_is_full(pool: PgPool) {
        // Dropping the lock with every command slot reserved can race a keepalive dispatch
        // failure, so isolate process-global metric deltas through teardown.
        let _metrics_guard = MetricsCapture::start();
        let mut join_set = JoinSet::new();
        let manager = start(&mut join_set, pool, Default::default())
            .await
            .unwrap();
        let lock = manager
            .try_acquire_lock("contended".to_string())
            .await
            .unwrap();

        // Reserve every bounded-command slot without sending a command.
        let command_slots = manager
            .command_slots
            .clone()
            .acquire_many_owned(COMMAND_BUFFER_SIZE as u32)
            .await
            .unwrap();

        drop(lock);
        drop(command_slots);

        let _lock = manager
            .try_acquire_lock("contended".to_string())
            .await
            .expect("work key stayed locked: the release was dropped by a full command queue");
    }

    #[crate::sqlx_test]
    async fn test_db_failure(pool: PgPool) {
        // Tests that can emit WorkLock failures hold this guard through teardown
        // so process-global counter deltas stay isolated.
        let _metrics_guard = MetricsCapture::start();
        let mut join_set = JoinSet::new();
        let manager = start(
            &mut join_set,
            pool.clone(),
            KeepaliveConfig {
                // Make the interval fast, to make sure reconnection works
                interval: Duration::from_millis(100),
                timeout: Duration::from_millis(500),
            },
        )
        .await
        .unwrap();

        let lock = manager.try_acquire_lock("work_key_1".into()).await.unwrap();

        let db_name = pool
            .connect_options()
            .get_database()
            .expect("Unknown database name")
            .to_string();

        // Kill all open db connections
        sqlx::query(
            r#"
SELECT pg_terminate_backend(pid)
FROM pg_stat_activity
WHERE datname = $1 AND pid <> pg_backend_pid()"#,
        )
        .bind(db_name)
        .execute(&pool)
        .await
        .expect("could not kill active database connections");

        tokio::time::sleep(Duration::from_millis(1000)).await;

        assert!(
            lock.is_alive(),
            "Lock should still be acquired even if the database connection died (it should have reconnected)"
        );

        assert!(
            manager.try_acquire_lock("work_key_1".into()).await.is_err(),
            "New locks should not be acquired even if the database connection died (it should have reconnected)"
        );
    }

    // Use tokio::test instead of sqlx::test here because we don't need to run migrations/etc
    #[tokio::test]
    async fn test_read_only_connection_is_replaced() {
        // Use a fast acquire timeout so that the test isn't too slow if it fails.
        let pool = sqlx::pool::PoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect(&env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();

        let work_lock_pool = create_work_lock_pool(&pool).await.unwrap();
        let mut db = work_lock_pool.acquire().await.unwrap();
        let original_backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        sqlx::query("SET default_transaction_read_only = on")
            .execute(&mut *db)
            .await
            .unwrap();
        drop(db);

        let mut db = work_lock_pool.acquire().await.expect(
            "sqlx should notice the read-only connection and close it, allowing a reconnect",
        );

        let (replacement_backend_pid, read_only): (i32, bool) = sqlx::query_as(
            "SELECT pg_backend_pid(), current_setting('transaction_read_only')::bool",
        )
        .fetch_one(&mut *db)
        .await
        .expect("read-only connection should be replaced");

        assert_ne!(replacement_backend_pid, original_backend_pid);
        assert!(!read_only);
    }

    #[crate::sqlx_test]
    async fn work_lock_pool_is_independent_from_main_pool(pool: PgPool) {
        let main_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .unwrap();
        let _main_connection = main_pool.acquire().await.unwrap();

        let mut join_set = JoinSet::new();
        let manager = tokio::time::timeout(
            Duration::from_secs(3),
            start(&mut join_set, main_pool, Default::default()),
        )
        .await
        .expect("WorkLockManager waited for the exhausted main pool")
        .expect("start WorkLockManager");

        manager
            .try_acquire_lock("independent-pool".into())
            .await
            .expect("dedicated pool should update locks while the main pool is exhausted");
    }

    #[crate::sqlx_test]
    async fn test_expiry(pool: PgPool) {
        let metrics = MetricsCapture::start();
        let mut join_set = JoinSet::new();
        let manager = start(
            &mut join_set,
            pool.clone(),
            KeepaliveConfig {
                // Make timeout lower than interval, to test keepalive timeouts
                interval: Duration::from_millis(500),
                timeout: Duration::from_millis(100),
            },
        )
        .await
        .unwrap();

        let old_lock = manager.try_acquire_lock("work_key_1".into()).await.unwrap();

        let start = Instant::now();
        let new_lock = loop {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if start.elapsed() > Duration::from_secs(2) {
                panic!("Lock should have expired by now");
            }
            match manager.try_acquire_lock("work_key_1".into()).await {
                Ok(lock) => break lock,
                Err(_) => continue,
            }
        };

        // Give the keep-alive time to fire again
        tokio::time::sleep(Duration::from_millis(1000)).await;

        assert!(
            !old_lock.is_alive(),
            "Old lock should be dead, since the new lock has taken its place."
        );
        assert!(new_lock.is_alive(), "New lock should be alive still");
        assert_eq!(
            metrics.counter_delta(
                WORK_LOCK_FAILURES_METRIC,
                &[("operation", "keep_alive"), ("failure", "lock_lost")],
            ),
            1.0,
            "the expired worker should report one keepalive lock loss",
        );
    }
}
