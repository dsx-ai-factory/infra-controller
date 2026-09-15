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

//! Per-SPIFFE-caller BMC Redfish session token manager.
//!
//! Every call to [`BmcSessionManager::issue_credentials`] mints a **fresh**
//! Redfish session by calling `nv-redfish` directly, and never touches a
//! session it did not just create. Redfish's `SessionService` supports
//! concurrent sessions, so replicas that share a SPIFFE identity each hold
//! their own token instead of revoking each other's -- the failure mode a
//! one-session-per-identity discipline created, where two `bmc-proxy`
//! replicas alternately invalidated one another and looped on 401s.
//!
//! ## Slot bounding
//!
//! Session slots on a BMC are finite, so minting is paired with a cap:
//! after a successful mint, the caller's oldest sessions beyond
//! `max_sessions_per_caller` (per `(SPIFFE service id, BMC MAC)`) are
//! best-effort revoked, oldest first. Sessions whose owners vanished
//! without revocation expire via the BMC's own idle timeout.
//!
//! ## Persistence model
//!
//! Each outstanding session's `@odata.id` is persisted as one row in the
//! `bmc_redfish_sessions` Postgres table behind the [`BmcSessionStore`]
//! trait. The `X-Auth-Token` itself is returned to the caller once and is
//! never stored anywhere by this manager. The rows exist purely so a later
//! revoke -- cap enforcement or [`BmcSessionManager::flush_mac`] -- knows
//! which session resources to `DELETE` on the BMC.
//!
//! Multiple API replicas may concurrently mint for the same pair; nothing
//! needs to serialize across replicas, since no replica touches a session
//! it did not create. Within a single replica, a per-BMC
//! `tokio::sync::Mutex` serializes all mints against the same MAC.
//!
//! ## Lifecycle hooks
//!
//! * [`BmcSessionManager::flush_mac`] -- intended for use when the BMC root
//!   credentials are deleted. Drops all rows for that MAC; does not contact
//!   the BMC (the credentials needed to authenticate the DELETE were just
//!   wiped). Orphans expire via the BMC idle timer.
//! * [`BmcSessionManager::note_credentials_updated`] -- intended for use
//!   when the BMC root credentials are set or rotated. Rows are
//!   intentionally retained so a later mint's cap enforcement can clean up
//!   the now-stale sessions with the new credentials.
//!
//! ## Lockout-avoidance circuit breaker
//!
//! Each [`BmcSessionManager`] tracks an in-memory per-BMC counter of
//! consecutive HTTP 401/403 responses returned during session creation.
//! Once that counter reaches the configured threshold the breaker trips and
//! any subsequent mint for the same BMC short-circuits with
//! [`BmcSessionError::AvoidLockout`] rather than attempting another login
//! (which could exhaust the BMC root account's retry budget). The breaker
//! is cleared by:
//!   * a successful mint (online recovery),
//!   * [`BmcSessionManager::flush_mac`] (credentials deleted), or
//!   * [`BmcSessionManager::note_credentials_updated`] (credentials set or
//!     rotated).
//!
//! Breaker state is intentionally not persisted: after a process restart a
//! single login attempt per BMC may be burned before the breaker re-trips,
//! and other replicas track lockouts independently. Network errors,
//! timeouts, 5xx responses, and deserialization failures do **not** count
//! against the threshold.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use carbide_redfish::nv_redfish::{BmcError, NvRedfishClientPool, RedfishBmc};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use db::bmc_redfish_session;
use mac_address::MacAddress;
use model::bmc_redfish_session::StoredSession;
use nv_redfish::Error as NvError;
use nv_redfish::core::{EntityTypeRef as _, ODataId};
use nv_redfish::session_service::{SessionCollection, SessionCreate};
use sqlx::PgPool;
use tokio::sync::Mutex;

/// Errors surfaced by [`BmcSessionManager`].
#[derive(thiserror::Error, Debug)]
pub(crate) enum BmcSessionError {
    /// No BMC root credentials are stored for this MAC; cannot create a
    /// session.
    #[error("BMC root credentials are not configured for MAC {0}")]
    MissingRootCredentials(MacAddress),

    /// Failure interacting with the BMC via nv-redfish (connect, create,
    /// or delete failed for a reason other than auth).
    #[error("redfish error talking to BMC at {bmc_addr}: {detail}")]
    Redfish {
        bmc_addr: SocketAddr,
        detail: String,
    },

    /// Failure reading the BMC root credentials from the credential store.
    #[error("credential store error: {0}")]
    CredentialStore(String),

    /// Failure persisting or reading session metadata from the
    /// [`BmcSessionStore`].
    #[error("BMC session store error: {0}")]
    Store(String),

    /// The BMC's Redfish ServiceRoot does not expose a `SessionService`.
    #[error("BMC at {bmc_addr} does not expose redfish SessionService")]
    NoSessionService { bmc_addr: SocketAddr },

    /// The lockout-avoidance circuit breaker is tripped for this BMC and
    /// we refuse to attempt another session creation until the BMC root
    /// credentials are deleted or updated.
    #[error(
        "BMC {bmc_mac} is locked out after {consecutive_unauthorized} consecutive \
         unauthorized responses (last HTTP status {last_status}); update BMC root \
         credentials to recover"
    )]
    AvoidLockout {
        bmc_mac: MacAddress,
        consecutive_unauthorized: u32,
        last_status: u16,
    },
}

/// A live Redfish session that we issued to a caller. The `token` is
/// transient: it is returned exactly once and never persisted by us.
#[derive(Clone)]
pub(crate) struct SessionEntry {
    /// `X-Auth-Token` value returned by the BMC on session creation.
    pub(crate) token: String,
    /// `@odata.id` of the session resource on the BMC; used to revoke the
    /// session via `DELETE` on the next rotate.
    session_odata_id: ODataId,
}

impl fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEntry")
            .field("token", &"[REDACTED]")
            .field("session_odata_id", &self.session_odata_id)
            .finish()
    }
}

pub(crate) enum BmcAuthMaterial {
    Session(SessionEntry),
    Basic(Credentials),
}

impl fmt::Debug for BmcAuthMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(entry) => f.debug_tuple("Session").field(entry).finish(),
            // `Credentials` does not yet implement a redacted Debug, so
            // print only the variant name to keep credentials out of logs.
            Self::Basic(_) => f
                .debug_struct("Basic")
                .field("creds", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Which best-effort BMC session cleanup step failed.
///
/// `operation` is the only metric label. BMCs, callers, sessions, and errors
/// stay on the log record instead of creating a new series for each failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum BmcSessionCleanupOperation {
    RevokePriorSession,
    ListSessionsForRevoke,
    RevokeUnpersistedSession,
    DeleteSessionRows,
}

/// A BMC session outlived the work that created it and could not be cleaned
/// up. `operation` names which cleanup failed and picks the wording operators
/// already receive. `spiffe_service_id` and `session` are absent on the paths
/// that never had one -- `flush_mac` works from the MAC alone, and a failed
/// session listing never reached a specific session.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "bmc_session_cleanup_failed",
    metric_name = "carbide_bmc_session_cleanup_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = dynamic,
    describe = "Number of BMC session cleanup failures, by operation."
)]
struct BmcSessionCleanupFailed {
    #[label]
    operation: BmcSessionCleanupOperation,
    #[context]
    bmc_mac_address: MacAddress,
    #[context]
    spiffe_service_id: Option<String>,
    #[context]
    session: Option<ODataId>,
    #[context]
    error: String,
}

impl carbide_instrument::DynamicMessage for BmcSessionCleanupFailed {
    fn message(&self) -> &'static str {
        match self.operation {
            BmcSessionCleanupOperation::RevokePriorSession => {
                "failed to revoke an excess BMC session; it will leak until BMC idle timeout"
            }
            BmcSessionCleanupOperation::ListSessionsForRevoke => {
                "failed to list BMC sessions for excess-session revoke; continuing"
            }
            BmcSessionCleanupOperation::RevokeUnpersistedSession => {
                "failed to revoke just-created session after store upsert failed; it will leak until BMC idle timeout"
            }
            BmcSessionCleanupOperation::DeleteSessionRows => {
                "failed to delete BMC session rows during flush_mac; continuing"
            }
        }
    }
}

/// The actual lockout-avoidance state change, as the bounded `transition`
/// label shared by the trip and clear Events below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum BmcSessionLockoutBreakerTransition {
    Tripped,
    Cleared,
}

/// The one metric the Events below record.
#[derive(carbide_instrument::MetricFamily)]
#[metric(
    name = "carbide_bmc_session_lockout_breaker_transitions_total",
    kind = counter,
    component = "nico-api",
    describe = "Number of BMC session lockout-avoidance breaker transitions."
)]
struct BmcSessionLockoutBreakerTransitions {
    transition: BmcSessionLockoutBreakerTransition,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "bmc_session_lockout_breaker_tripped",
    metric_family = BmcSessionLockoutBreakerTransitions,
    log = warn,
    message = "BmcSessionManager: lockout-avoidance breaker tripped"
)]
struct BmcSessionLockoutBreakerTripped {
    #[label]
    transition: BmcSessionLockoutBreakerTransition,
    #[context]
    bmc_mac_address: MacAddress,
    #[context(value)]
    http_status: i64,
    #[context(value)]
    consecutive_unauthorized_count: i64,
    #[context(value)]
    lockout_threshold_count: i64,
}

impl BmcSessionLockoutBreakerTripped {
    fn new(
        bmc_mac_address: MacAddress,
        http_status: u16,
        consecutive_unauthorized_count: u32,
        lockout_threshold_count: u32,
    ) -> Self {
        Self {
            transition: BmcSessionLockoutBreakerTransition::Tripped,
            bmc_mac_address,
            http_status: i64::from(http_status),
            consecutive_unauthorized_count: i64::from(consecutive_unauthorized_count),
            lockout_threshold_count: i64::from(lockout_threshold_count),
        }
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "bmc_session_lockout_breaker_cleared",
    metric_family = BmcSessionLockoutBreakerTransitions,
    log = info,
    message = "BmcSessionManager: lockout-avoidance breaker cleared"
)]
struct BmcSessionLockoutBreakerCleared {
    #[label]
    transition: BmcSessionLockoutBreakerTransition,
    #[context]
    bmc_mac_address: MacAddress,
}

impl BmcSessionLockoutBreakerCleared {
    fn new(bmc_mac_address: MacAddress) -> Self {
        Self {
            transition: BmcSessionLockoutBreakerTransition::Cleared,
            bmc_mac_address,
        }
    }
}

/// Per-BMC lockout-avoidance state.
#[derive(Debug, Clone)]
struct LockoutState {
    consecutive_unauthorized: u32,
    last_status: u16,
    /// `Some(when)` once the breaker has tripped; subsequent rotate calls
    /// short-circuit until the state is cleared.
    tripped_at: Option<Instant>,
}

/// Persistence layer for outstanding Redfish sessions. Wraps DB errors as
/// [`BmcSessionError::Store`] so the manager's surface stays uniform.
#[async_trait]
pub(crate) trait BmcSessionStore: Send + Sync {
    /// Every outstanding session for `(spiffe_service_id, bmc_mac)`,
    /// oldest first.
    async fn find_by_owner(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
    ) -> Result<Vec<StoredSession>, BmcSessionError>;

    /// Records a newly created session as one more row for its owner.
    /// A row already naming this `(bmc_mac, session_odata_id)` describes a
    /// session the BMC has since replaced, so the insert takes it over.
    async fn insert(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        session_odata_id: &str,
    ) -> Result<(), BmcSessionError>;

    /// Deletes one session row, scoped to its owner, returning whether a row
    /// was removed. `false` means an [`BmcSessionStore::insert`] takeover of
    /// a reused `@odata.id` got there first: the row -- and the session it
    /// now describes -- belong to another identity.
    async fn delete_session(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        session_odata_id: &str,
    ) -> Result<bool, BmcSessionError>;

    async fn delete_by_mac(&self, bmc_mac: MacAddress) -> Result<(), BmcSessionError>;
}

/// Postgres-backed [`BmcSessionStore`] used in production.
pub(crate) struct PgBmcSessionStore {
    pool: PgPool,
}

impl PgBmcSessionStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BmcSessionStore for PgBmcSessionStore {
    async fn find_by_owner(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
    ) -> Result<Vec<StoredSession>, BmcSessionError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))?;
        bmc_redfish_session::find_by_owner(conn.as_mut(), spiffe_service_id, bmc_mac)
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))
    }

    async fn insert(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        session_odata_id: &str,
    ) -> Result<(), BmcSessionError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))?;
        bmc_redfish_session::insert(conn.as_mut(), spiffe_service_id, bmc_mac, session_odata_id)
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))
    }

    async fn delete_session(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        session_odata_id: &str,
    ) -> Result<bool, BmcSessionError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))?;
        bmc_redfish_session::delete_session(
            conn.as_mut(),
            spiffe_service_id,
            bmc_mac,
            session_odata_id,
        )
        .await
        .map_err(|err| BmcSessionError::Store(err.to_string()))
    }

    async fn delete_by_mac(&self, bmc_mac: MacAddress) -> Result<(), BmcSessionError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|err| BmcSessionError::Store(err.to_string()))?;
        bmc_redfish_session::delete_by_mac(conn.as_mut(), bmc_mac)
            .await
            .map(|_| ())
            .map_err(|err| BmcSessionError::Store(err.to_string()))
    }
}

pub(crate) struct BmcSessionManager {
    redfish_pool: Arc<NvRedfishClientPool>,
    credential_manager: Arc<dyn CredentialManager>,
    store: Arc<dyn BmcSessionStore>,
    mac_locks: Mutex<HashMap<MacAddress, Arc<Mutex<()>>>>,
    lockouts: Mutex<HashMap<MacAddress, LockoutState>>,
    lockout_threshold: u32,
    allow_basic_auth_fallback: bool,
    /// Cap on outstanding sessions per `(SPIFFE service id, BMC MAC)`;
    /// a mint that pushes past it revokes the caller's oldest sessions.
    max_sessions_per_caller: usize,
    no_session_service: Mutex<HashSet<MacAddress>>,
}

impl BmcSessionManager {
    pub(crate) fn new(
        redfish_pool: Arc<NvRedfishClientPool>,
        credential_manager: Arc<dyn CredentialManager>,
        store: Arc<dyn BmcSessionStore>,
        lockout_threshold: u32,
        allow_basic_auth_fallback: bool,
        max_sessions_per_caller: usize,
    ) -> Self {
        Self {
            redfish_pool,
            credential_manager,
            store,
            mac_locks: Mutex::new(HashMap::new()),
            lockouts: Mutex::new(HashMap::new()),
            lockout_threshold: lockout_threshold.max(1),
            allow_basic_auth_fallback,
            max_sessions_per_caller: max_sessions_per_caller.max(1),
            no_session_service: Mutex::new(HashSet::new()),
        }
    }

    /// Create a brand new session against the BMC at `bmc_addr` and return
    /// its token, then revoke this caller's oldest sessions beyond the cap.
    ///
    /// Never touches a session another mint created below the cap, so any
    /// number of callers sharing `spiffe_service_id` can hold live tokens
    /// concurrently.
    async fn mint_session(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        bmc_addr: SocketAddr,
    ) -> Result<SessionEntry, BmcSessionError> {
        let mac_lock = self.acquire_mac_lock(bmc_mac).await;
        let mac_guard = mac_lock.lock().await;

        if let Some(err) = self.check_not_locked_out(bmc_mac).await {
            return Err(err);
        }

        let creds = self.bmc_root_credentials(bmc_mac).await?;
        let (username, password) = match &creds {
            Credentials::UsernamePassword { username, password } => {
                (username.clone(), password.clone())
            }
        };

        let service_root = match self.redfish_pool.service_root(bmc_addr, Some(creds)).await {
            Ok(root) => root,
            Err(err) => return Err(self.classify_and_map(err, bmc_mac, bmc_addr).await),
        };

        let session_service = match service_root.session_service().await {
            Ok(Some(svc)) => svc,
            Ok(None) => return Err(BmcSessionError::NoSessionService { bmc_addr }),
            Err(err) => return Err(self.classify_and_map(err, bmc_mac, bmc_addr).await),
        };

        let sessions = match session_service.sessions().await {
            Ok(Some(coll)) => coll,
            Ok(None) => {
                return Err(BmcSessionError::Redfish {
                    bmc_addr,
                    detail: "BMC SessionService does not expose a Sessions collection".to_string(),
                });
            }
            Err(err) => return Err(self.classify_and_map(err, bmc_mac, bmc_addr).await),
        };

        let created = match sessions
            .create_session(&SessionCreate::builder(username, password).build())
            .await
        {
            Ok(s) => s,
            Err(err) => return Err(self.classify_and_map(err, bmc_mac, bmc_addr).await),
        };

        let token = created
            .auth_token()
            .ok_or_else(|| BmcSessionError::Redfish {
                bmc_addr,
                detail: "BMC did not return an X-Auth-Token on session creation".to_string(),
            })?
            .to_string();
        let location = created
            .location()
            .cloned()
            .ok_or_else(|| BmcSessionError::Redfish {
                bmc_addr,
                detail: "BMC did not return a session @odata.id on session creation".to_string(),
            })?;

        // If persist fails we revoke token to avoid exhaust of session limit
        if let Err(store_err) = self
            .store
            .insert(spiffe_service_id, bmc_mac, &location.to_string())
            .await
        {
            if let Err(revoke_err) = created.delete().await {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::RevokeUnpersistedSession,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(spiffe_service_id.to_owned()),
                    session: Some(location),
                    error: format!("{revoke_err:?}"),
                });
            }
            return Err(store_err);
        }

        self.clear_lockout(bmc_mac).await;

        // Cap enforcement is best-effort housekeeping that cannot change the
        // token being returned, and on BMCs without $expand it costs one GET
        // per live session -- so release the per-MAC lock first rather than
        // stalling every concurrent mint for this BMC behind it.
        drop(mac_guard);

        self.revoke_sessions_beyond_cap(spiffe_service_id, bmc_mac, &location, &sessions)
            .await;

        Ok(SessionEntry {
            token,
            session_odata_id: location,
        })
    }

    /// Best-effort revoke of this caller's oldest sessions beyond
    /// `max_sessions_per_caller`, so a caller that refetches -- restarts,
    /// 401 recoveries, extra replicas -- cannot grow its session count
    /// without bound. Failures are counted and logged, never propagated:
    /// the fresh session was already minted and belongs to the caller
    /// regardless.
    ///
    /// Runs outside the per-MAC lock; concurrent passes at worst revoke the
    /// same already-dead session, which the missing-member check tolerates.
    ///
    /// Claiming a row before its remote `DELETE` leaves one residual window
    /// (a single request round-trip wide): a concurrent mint can be handed
    /// the same reused `@odata.id` between the two, and the `DELETE` then
    /// hits that fresh session. All that costs is one 401 on a token whose
    /// caller refetches and re-mints -- the recovery every caller already
    /// implements. Closing the window would take either a cross-instance
    /// per-MAC lock held across BMC I/O, or `If-Match` preconditions on
    /// nv-redfish's session delete; neither is worth it for that failure.
    async fn revoke_sessions_beyond_cap(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        just_minted: &ODataId,
        sessions: &SessionCollection<RedfishBmc>,
    ) {
        let outstanding = match self.store.find_by_owner(spiffe_service_id, bmc_mac).await {
            Ok(rows) => rows,
            Err(err) => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::ListSessionsForRevoke,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(spiffe_service_id.to_owned()),
                    session: None,
                    error: err.to_string(),
                });
                return;
            }
        };

        let excess = sessions_beyond_cap(outstanding, self.max_sessions_per_caller, just_minted);
        if excess.is_empty() {
            return;
        }

        let members = match sessions.members().await {
            Ok(members) => members,
            Err(err) => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::ListSessionsForRevoke,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(spiffe_service_id.to_owned()),
                    session: None,
                    error: format!("{err:?}"),
                });
                return;
            }
        };

        for row in excess {
            let session_id = ODataId::from(row.session_odata_id);

            // Claim the row before touching the BMC. If the delete removed
            // nothing, a concurrent mint took the row over after the BMC
            // reused this @odata.id -- the session behind it is the new
            // owner's live one and must not be revoked. (The old session is
            // dead regardless: the BMC only reuses an id it has released.)
            match self
                .store
                .delete_session(spiffe_service_id, bmc_mac, &session_id.to_string())
                .await
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(err) => {
                    carbide_instrument::emit(BmcSessionCleanupFailed {
                        operation: BmcSessionCleanupOperation::DeleteSessionRows,
                        bmc_mac_address: bmc_mac,
                        spiffe_service_id: Some(spiffe_service_id.to_owned()),
                        session: Some(session_id),
                        error: err.to_string(),
                    });
                    continue;
                }
            }

            // A missing member means the BMC already expired the session. A
            // failed delete leaks it until the BMC idle timeout -- the row is
            // already claimed, and re-inserting it could stomp a takeover, so
            // best effort ends here.
            if let Some(session) = members.iter().find(|m| m.raw().odata_id() == &session_id)
                && let Err(err) = session.delete().await
            {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::RevokePriorSession,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(spiffe_service_id.to_owned()),
                    session: Some(session_id),
                    error: format!("{err:?}"),
                });
            }
        }
    }

    pub(crate) async fn issue_credentials(
        &self,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        bmc_addr: SocketAddr,
    ) -> Result<BmcAuthMaterial, BmcSessionError> {
        if !self.allow_basic_auth_fallback {
            return self
                .mint_session(spiffe_service_id, bmc_mac, bmc_addr)
                .await
                .map(BmcAuthMaterial::Session);
        }

        // Fast path: we already discovered this BMC has no SessionService
        if self.no_session_service.lock().await.contains(&bmc_mac) {
            let creds = self.bmc_root_credentials(bmc_mac).await?;
            return Ok(BmcAuthMaterial::Basic(creds));
        }

        match self
            .mint_session(spiffe_service_id, bmc_mac, bmc_addr)
            .await
        {
            Ok(entry) => Ok(BmcAuthMaterial::Session(entry)),
            Err(BmcSessionError::NoSessionService { .. }) => {
                let newly_cached = self.no_session_service.lock().await.insert(bmc_mac);
                if newly_cached {
                    tracing::info!(
                        bmc_mac_address = %bmc_mac,
                        bmc_address = %bmc_addr,
                        "BMC does not expose Redfish SessionService; serving basic-auth credentials for the remainder of this process lifetime"
                    );
                }
                let creds = self.bmc_root_credentials(bmc_mac).await?;
                Ok(BmcAuthMaterial::Basic(creds))
            }
            Err(other) => Err(other),
        }
    }

    async fn classify_and_map(
        &self,
        err: NvError<RedfishBmc>,
        bmc_mac: MacAddress,
        bmc_addr: SocketAddr,
    ) -> BmcSessionError {
        if let Some(status) = classify_unauthorized(&err)
            && let Some(lockout_err) = self.record_unauthorized(bmc_mac, status).await
        {
            return lockout_err;
        }
        BmcSessionError::Redfish {
            bmc_addr,
            detail: err.to_string(),
        }
    }

    /// Drop all session rows for `bmc_mac` and clear any lockout state.
    pub(crate) async fn flush_mac(&self, bmc_mac: MacAddress) {
        if let Err(err) = self.store.delete_by_mac(bmc_mac).await {
            carbide_instrument::emit(BmcSessionCleanupFailed {
                operation: BmcSessionCleanupOperation::DeleteSessionRows,
                bmc_mac_address: bmc_mac,
                spiffe_service_id: None,
                session: None,
                error: err.to_string(),
            });
        }
        self.clear_lockout(bmc_mac).await;
        self.clear_no_session_service(bmc_mac).await;
    }

    /// Reset Circtuit Breaker
    pub(crate) async fn note_credentials_updated(&self, bmc_mac: MacAddress) {
        self.clear_lockout(bmc_mac).await;
        self.clear_no_session_service(bmc_mac).await;
    }

    async fn clear_no_session_service(&self, bmc_mac: MacAddress) {
        if self.no_session_service.lock().await.remove(&bmc_mac) {
            tracing::info!(
                bmc_mac_address = %bmc_mac,
                "BmcSessionManager: forgetting cached `no SessionService` decision; \
                 next issue_credentials will re-probe"
            );
        }
    }

    async fn check_not_locked_out(&self, bmc_mac: MacAddress) -> Option<BmcSessionError> {
        let lockouts = self.lockouts.lock().await;
        let state = lockouts.get(&bmc_mac)?;
        if state.tripped_at.is_some() {
            Some(BmcSessionError::AvoidLockout {
                bmc_mac,
                consecutive_unauthorized: state.consecutive_unauthorized,
                last_status: state.last_status,
            })
        } else {
            None
        }
    }

    /// Checks and return any authorization/authentication related error, as well as update lockouts
    async fn record_unauthorized(
        &self,
        bmc_mac: MacAddress,
        status: u16,
    ) -> Option<BmcSessionError> {
        let mut lockouts = self.lockouts.lock().await;
        let entry = lockouts.entry(bmc_mac).or_insert(LockoutState {
            consecutive_unauthorized: 0,
            last_status: status,
            tripped_at: None,
        });
        entry.consecutive_unauthorized = entry.consecutive_unauthorized.saturating_add(1);
        entry.last_status = status;
        if entry.consecutive_unauthorized >= self.lockout_threshold && entry.tripped_at.is_none() {
            entry.tripped_at = Some(Instant::now());
            carbide_instrument::emit(BmcSessionLockoutBreakerTripped::new(
                bmc_mac,
                status,
                entry.consecutive_unauthorized,
                self.lockout_threshold,
            ));
            return Some(BmcSessionError::AvoidLockout {
                bmc_mac,
                consecutive_unauthorized: entry.consecutive_unauthorized,
                last_status: status,
            });
        }
        None
    }

    async fn clear_lockout(&self, bmc_mac: MacAddress) {
        if self.lockouts.lock().await.remove(&bmc_mac).is_some() {
            carbide_instrument::emit(BmcSessionLockoutBreakerCleared::new(bmc_mac));
        }
    }

    #[cfg(test)]
    pub(crate) async fn force_trip_for_test(
        &self,
        bmc_mac: MacAddress,
        consecutive_unauthorized: u32,
        last_status: u16,
    ) {
        self.lockouts.lock().await.insert(
            bmc_mac,
            LockoutState {
                consecutive_unauthorized: consecutive_unauthorized.max(1),
                last_status,
                tripped_at: Some(Instant::now()),
            },
        );
    }

    async fn acquire_mac_lock(&self, bmc_mac: MacAddress) -> Arc<Mutex<()>> {
        let mut mac_locks = self.mac_locks.lock().await;
        mac_locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        mac_locks
            .entry(bmc_mac)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn bmc_root_credentials(
        &self,
        bmc_mac: MacAddress,
    ) -> Result<Credentials, BmcSessionError> {
        self.credential_manager
            .get_credentials(&CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: bmc_mac,
                },
            })
            .await
            .map_err(|err| BmcSessionError::CredentialStore(err.to_string()))?
            .ok_or(BmcSessionError::MissingRootCredentials(bmc_mac))
    }
}

/// The sessions a caller must give up to fit under `cap`: the oldest ones,
/// keeping the newest `cap`. `outstanding` is expected oldest-first, as
/// [`BmcSessionStore::find_by_owner`] returns it, and to contain the row for
/// `just_minted`.
///
/// `just_minted` is excluded *before* the excess is selected: concurrent
/// replicas can mint within the same server-side `now()`, and an `issued_at`
/// tie is broken lexically, which can sort the just-minted row among the
/// "oldest". The session whose token is about to be handed out must survive,
/// and skipping it may not shrink the revocation count -- otherwise a tie
/// would leave the caller one over the cap.
fn sessions_beyond_cap(
    outstanding: Vec<StoredSession>,
    cap: usize,
    just_minted: &ODataId,
) -> Vec<StoredSession> {
    let excess = outstanding.len().saturating_sub(cap);
    outstanding
        .into_iter()
        .filter(|row| ODataId::from(row.session_odata_id.clone()) != *just_minted)
        .take(excess)
        .collect()
}

fn classify_unauthorized(err: &NvError<RedfishBmc>) -> Option<u16> {
    let NvError::Bmc(BmcError::InvalidResponse { status, .. }) = err else {
        return None;
    };
    if *status == reqwest::StatusCode::UNAUTHORIZED || *status == reqwest::StatusCode::FORBIDDEN {
        Some(status.as_u16())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use async_trait::async_trait;
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_secrets::SecretsError;
    use carbide_secrets::credentials::{
        BmcCredentialType, CredentialKey, CredentialManager, CredentialReader, CredentialWriter,
        Credentials,
    };
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::{Check, check_values, value_scenarios};
    use mac_address::MacAddress;
    use sqlx::types::chrono::Utc;
    use tokio::sync::Mutex;

    use super::{
        BmcSessionCleanupFailed, BmcSessionCleanupOperation, BmcSessionError, BmcSessionManager,
        BmcSessionStore, StoredSession,
    };

    fn mac(byte: u8) -> MacAddress {
        MacAddress::from([byte, 0, 0, 0, 0, 1])
    }

    const TEST_LOCKOUT_THRESHOLD: u32 = 3;
    const TEST_MAX_SESSIONS_PER_CALLER: usize = 4;
    const CLEANUP_FAILURE_METRIC: &str = "carbide_bmc_session_cleanup_failures_total";

    /// One row per session, insertion-ordered like the Postgres store's
    /// `issued_at` ordering (rows are only ever appended).
    #[derive(Default)]
    struct InMemoryBmcSessionStore {
        rows: Mutex<Vec<StoredSession>>,
    }

    impl InMemoryBmcSessionStore {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        async fn rows(&self) -> Vec<StoredSession> {
            self.rows.lock().await.clone()
        }
    }

    #[async_trait]
    impl BmcSessionStore for InMemoryBmcSessionStore {
        async fn find_by_owner(
            &self,
            spiffe_service_id: &str,
            bmc_mac: MacAddress,
        ) -> Result<Vec<StoredSession>, BmcSessionError> {
            Ok(self
                .rows
                .lock()
                .await
                .iter()
                .filter(|row| {
                    row.spiffe_service_id == spiffe_service_id && row.bmc_mac_address == bmc_mac
                })
                .cloned()
                .collect())
        }

        async fn insert(
            &self,
            spiffe_service_id: &str,
            bmc_mac: MacAddress,
            session_odata_id: &str,
        ) -> Result<(), BmcSessionError> {
            let mut rows = self.rows.lock().await;
            // Mirror the Postgres ON CONFLICT: a colliding row describes a
            // session the BMC has already replaced, so it is taken over.
            rows.retain(|row| {
                row.bmc_mac_address != bmc_mac || row.session_odata_id != session_odata_id
            });
            rows.push(StoredSession {
                spiffe_service_id: spiffe_service_id.to_owned(),
                bmc_mac_address: bmc_mac,
                session_odata_id: session_odata_id.to_owned(),
                issued_at: Utc::now(),
            });
            Ok(())
        }

        async fn delete_session(
            &self,
            spiffe_service_id: &str,
            bmc_mac: MacAddress,
            session_odata_id: &str,
        ) -> Result<bool, BmcSessionError> {
            let mut rows = self.rows.lock().await;
            let before = rows.len();
            rows.retain(|row| {
                row.spiffe_service_id != spiffe_service_id
                    || row.bmc_mac_address != bmc_mac
                    || row.session_odata_id != session_odata_id
            });
            Ok(rows.len() < before)
        }

        async fn delete_by_mac(&self, bmc_mac: MacAddress) -> Result<(), BmcSessionError> {
            self.rows
                .lock()
                .await
                .retain(|row| row.bmc_mac_address != bmc_mac);
            Ok(())
        }
    }

    struct DeleteFailingBmcSessionStore;

    #[async_trait]
    impl BmcSessionStore for DeleteFailingBmcSessionStore {
        async fn find_by_owner(
            &self,
            _spiffe_service_id: &str,
            _bmc_mac: MacAddress,
        ) -> Result<Vec<StoredSession>, BmcSessionError> {
            Ok(Vec::new())
        }

        async fn insert(
            &self,
            _spiffe_service_id: &str,
            _bmc_mac: MacAddress,
            _session_odata_id: &str,
        ) -> Result<(), BmcSessionError> {
            Ok(())
        }

        async fn delete_session(
            &self,
            _spiffe_service_id: &str,
            _bmc_mac: MacAddress,
            _session_odata_id: &str,
        ) -> Result<bool, BmcSessionError> {
            Ok(true)
        }

        async fn delete_by_mac(&self, _bmc_mac: MacAddress) -> Result<(), BmcSessionError> {
            Err(BmcSessionError::Store(
                "injected session-row deletion failure".to_string(),
            ))
        }
    }

    fn manager_with_creds() -> (Arc<BmcSessionManager>, Arc<InMemoryBmcSessionStore>) {
        manager_with_creds_and_threshold(TEST_LOCKOUT_THRESHOLD)
    }

    fn manager_with_creds_and_threshold(
        threshold: u32,
    ) -> (Arc<BmcSessionManager>, Arc<InMemoryBmcSessionStore>) {
        manager_with_creds_threshold_and_fallback(threshold, false)
    }

    fn manager_with_creds_threshold_and_fallback(
        threshold: u32,
        allow_basic_auth_fallback: bool,
    ) -> (Arc<BmcSessionManager>, Arc<InMemoryBmcSessionStore>) {
        let bmc_proxy = Arc::new(ArcSwap::new(Arc::new(None)));
        let redfish_pool = carbide_redfish::nv_redfish::new_pool(bmc_proxy);
        let credential_manager =
            Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            }));
        let store = InMemoryBmcSessionStore::new();
        let manager = Arc::new(BmcSessionManager::new(
            redfish_pool,
            credential_manager,
            store.clone(),
            threshold,
            allow_basic_auth_fallback,
            TEST_MAX_SESSIONS_PER_CALLER,
        ));
        (manager, store)
    }

    fn cap_row(n: u8) -> StoredSession {
        StoredSession {
            spiffe_service_id: "svc".to_string(),
            bmc_mac_address: mac(0x10),
            session_odata_id: format!("/sessions/{n}"),
            // Explicit, distinct timestamps document the oldest-first input
            // ordering the function's contract assumes.
            issued_at: Utc::now() + chrono::Duration::seconds(i64::from(n)),
        }
    }

    /// Runs the selection over rows `/sessions/0..rows` with the row at
    /// index `minted` playing the just-minted session.
    fn observe_sessions_beyond_cap((rows, cap, minted): (u8, usize, u8)) -> Vec<String> {
        let just_minted = nv_redfish::core::ODataId::from(format!("/sessions/{minted}"));
        super::sessions_beyond_cap((0..rows).map(cap_row).collect(), cap, &just_minted)
            .into_iter()
            .map(|row| row.session_odata_id)
            .collect()
    }

    #[test]
    fn sessions_beyond_cap_keeps_the_newest_cap_sessions() {
        // The just-minted row is the newest (last index) except where the
        // scenario says otherwise.
        check_values(
            [
                Check {
                    scenario: "under cap",
                    input: (3, 4, 2),
                    expect: vec![],
                },
                Check {
                    scenario: "exactly at cap",
                    input: (4, 4, 3),
                    expect: vec![],
                },
                Check {
                    scenario: "one over revokes the oldest",
                    input: (5, 4, 4),
                    expect: vec!["/sessions/0".to_string()],
                },
                // Regression: an issued_at tie can sort the just-minted row
                // among the "oldest". It must survive, and the caller must
                // still land on the cap -- the next-oldest goes instead.
                Check {
                    scenario: "minted row sorted oldest survives, next-oldest goes",
                    input: (5, 4, 0),
                    expect: vec!["/sessions/1".to_string()],
                },
                Check {
                    scenario: "many over revoke oldest first",
                    input: (7, 4, 6),
                    expect: vec![
                        "/sessions/0".to_string(),
                        "/sessions/1".to_string(),
                        "/sessions/2".to_string(),
                    ],
                },
                // The constructor clamps the configured cap to >= 1, so 0 is
                // unreachable in production; the function itself still
                // behaves sanely: everything but the minted row goes.
                Check {
                    scenario: "cap of zero revokes everything else",
                    input: (2, 0, 1),
                    expect: vec!["/sessions/0".to_string()],
                },
            ],
            observe_sessions_beyond_cap,
        );
    }

    #[test]
    fn odata_id_last_segment_returns_session_id() {
        let id = nv_redfish::core::ODataId::from(
            "/redfish/v1/SessionService/Sessions/abc123".to_string(),
        );
        assert_eq!(id.last_segment(), Some("abc123"));
    }

    async fn seed_row(
        store: &InMemoryBmcSessionStore,
        spiffe_service_id: &str,
        bmc_mac: MacAddress,
        session_odata_id: &str,
    ) {
        store
            .insert(spiffe_service_id, bmc_mac, session_odata_id)
            .await
            .expect("in-memory insert never fails");
    }

    #[tokio::test]
    async fn flush_mac_deletes_store_rows_and_clears_lockout() {
        let _metrics = MetricsCapture::start();
        let (manager, store) = manager_with_creds();
        let mac_a = mac(0xAA);
        let mac_b = mac(0xBB);

        seed_row(&store, "svc-1", mac_a, "/sessions/1").await;
        seed_row(&store, "svc-2", mac_a, "/sessions/2").await;
        seed_row(&store, "svc-1", mac_b, "/sessions/3").await;
        manager.force_trip_for_test(mac_a, 3, 401).await;

        manager.flush_mac(mac_a).await;

        // mac_a rows are gone, mac_b survives.
        let rows = store.rows().await;
        assert_eq!(rows.len(), 1);
        assert!(rows.iter().all(|row| row.bmc_mac_address == mac_b));
        // lockout was cleared along with the rows.
        assert!(manager.check_not_locked_out(mac_a).await.is_none());
    }

    #[test]
    fn flush_mac_counts_store_delete_failure_and_still_clears_cached_state() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let bmc_proxy = Arc::new(ArcSwap::new(Arc::new(None)));
        let redfish_pool = carbide_redfish::nv_redfish::new_pool(bmc_proxy);
        let credential_manager =
            Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            }));
        let manager = Arc::new(BmcSessionManager::new(
            redfish_pool,
            credential_manager,
            Arc::new(DeleteFailingBmcSessionStore),
            TEST_LOCKOUT_THRESHOLD,
            false,
            TEST_MAX_SESSIONS_PER_CALLER,
        ));
        let bmc_mac = mac(0xAF);

        runtime.block_on(async {
            manager.force_trip_for_test(bmc_mac, 3, 401).await;
            manager.no_session_service.lock().await.insert(bmc_mac);
        });

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| runtime.block_on(manager.flush_mac(bmc_mac)));
        let cleanup_logs = logs
            .iter()
            .filter(|log| log.field("metric_name") == Some(CLEANUP_FAILURE_METRIC))
            .collect::<Vec<_>>();

        assert_eq!(
            cleanup_logs.len(),
            1,
            "the injected store failure should emit one cleanup Event"
        );
        let log = cleanup_logs[0];
        let bmc_mac_address = bmc_mac.to_string();
        assert_eq!(log.level, tracing::Level::WARN);
        assert_eq!(log.metadata_name, "bmc_session_cleanup_failed");
        assert_eq!(
            log.message,
            "failed to delete BMC session rows during flush_mac; continuing"
        );
        assert_eq!(log.field("operation"), Some("delete_session_rows"));
        assert_eq!(log.field("bmc_mac_address"), Some(bmc_mac_address.as_str()));
        assert_eq!(
            log.field("error"),
            Some("BMC session store error: injected session-row deletion failure")
        );
        assert_eq!(
            metrics.counter_delta(
                CLEANUP_FAILURE_METRIC,
                &[("operation", "delete_session_rows")]
            ),
            1.0
        );

        runtime.block_on(async {
            assert!(
                manager.check_not_locked_out(bmc_mac).await.is_none(),
                "`flush_mac` should clear the breaker after a store failure"
            );
            assert!(
                !manager.no_session_service.lock().await.contains(&bmc_mac),
                "`flush_mac` should clear the SessionService cache after a store failure"
            );
        });
    }

    #[tokio::test]
    async fn note_credentials_updated_retains_store_rows() {
        let _metrics = MetricsCapture::start();
        let (manager, store) = manager_with_creds();
        let bmc_mac = mac(0xCC);
        seed_row(&store, "svc-1", bmc_mac, "/sessions/keep-me").await;
        manager.force_trip_for_test(bmc_mac, 5, 403).await;

        manager.note_credentials_updated(bmc_mac).await;

        // Row is still present so a later mint's cap pass can clean up the
        // stale session with the new creds; the breaker has been cleared.
        let rows = store.rows().await;
        assert!(
            rows.iter()
                .any(|row| row.spiffe_service_id == "svc-1" && row.bmc_mac_address == bmc_mac)
        );
        assert!(manager.check_not_locked_out(bmc_mac).await.is_none());
    }

    // The regression this change exists for: callers sharing one SPIFFE
    // identity each keep their own session row. Under the old
    // one-row-per-identity model the second insert overwrote (and the
    // manager then revoked) the first caller's session.
    #[tokio::test]
    async fn store_keeps_one_row_per_session_for_one_identity() {
        let store = InMemoryBmcSessionStore::new();
        let bmc_mac = mac(0xDD);
        store.insert("svc", bmc_mac, "/sessions/v1").await.unwrap();
        store.insert("svc", bmc_mac, "/sessions/v2").await.unwrap();

        let rows = store
            .find_by_owner("svc", bmc_mac)
            .await
            .expect("in-memory find never fails");
        assert_eq!(
            rows.iter()
                .map(|row| row.session_odata_id.as_str())
                .collect::<Vec<_>>(),
            vec!["/sessions/v1", "/sessions/v2"],
            "both sessions must coexist, oldest first"
        );
    }

    #[tokio::test]
    async fn store_delete_session_removes_only_that_session() {
        let store = InMemoryBmcSessionStore::new();
        let bmc_mac = mac(0xDE);
        store.insert("svc", bmc_mac, "/sessions/v1").await.unwrap();
        store.insert("svc", bmc_mac, "/sessions/v2").await.unwrap();

        store
            .delete_session("svc", bmc_mac, "/sessions/v1")
            .await
            .expect("in-memory delete never fails");

        let rows = store
            .find_by_owner("svc", bmc_mac)
            .await
            .expect("in-memory find never fails");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_odata_id, "/sessions/v2");
    }

    #[tokio::test]
    async fn rotate_returns_missing_credentials_when_unset() {
        let bmc_proxy = Arc::new(ArcSwap::new(Arc::new(None)));
        let redfish_pool = carbide_redfish::nv_redfish::new_pool(bmc_proxy);
        let credential_manager = Arc::new(TestCredentialManager::default());
        let store = InMemoryBmcSessionStore::new();
        let manager = BmcSessionManager::new(
            redfish_pool,
            credential_manager,
            store,
            TEST_LOCKOUT_THRESHOLD,
            false,
            TEST_MAX_SESSIONS_PER_CALLER,
        );

        let bmc_mac = mac(0xCE);
        let bmc_addr = "127.0.0.1:9999".parse().unwrap();
        let err = manager
            .mint_session("svc-x", bmc_mac, bmc_addr)
            .await
            .expect_err("should fail with missing root credentials");
        match err {
            super::BmcSessionError::MissingRootCredentials(got) => assert_eq!(got, bmc_mac),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mac_lock_collapses_all_spiffe_callers_for_one_mac() {
        let (manager, _store) = manager_with_creds();
        let mac_a = mac(0x01);
        let mac_b = mac(0x02);

        let lock_a1 = manager.acquire_mac_lock(mac_a).await;
        let lock_a2 = manager.acquire_mac_lock(mac_a).await;
        let lock_b = manager.acquire_mac_lock(mac_b).await;

        assert!(
            Arc::ptr_eq(&lock_a1, &lock_a2),
            "every caller for the same MAC must share a single mutex — \
             otherwise the lockout breaker can be raced past"
        );
        assert!(
            !Arc::ptr_eq(&lock_a1, &lock_b),
            "distinct MACs must use distinct mutexes — otherwise one slow \
             BMC blocks unrelated traffic"
        );
        let _g = lock_b.lock().await;
    }

    #[tokio::test]
    async fn acquire_mac_lock_evicts_unused_entries() {
        let (manager, _store) = manager_with_creds();
        let mac_a = mac(0x10);
        let mac_b = mac(0x11);

        // Acquire and immediately drop a lock for mac_a — no rotate is
        // holding it once this expression statement ends.
        drop(manager.acquire_mac_lock(mac_a).await);
        assert!(
            manager.mac_locks.lock().await.contains_key(&mac_a),
            "entry should be present immediately after acquire-then-drop \
             (GC only runs on the next acquire)"
        );

        // Touching any MAC fires the opportunistic GC pass, which should
        // evict the now-stale mac_a entry because nobody references its
        // Arc except the map.
        let _b = manager.acquire_mac_lock(mac_b).await;

        let locks = manager.mac_locks.lock().await;
        assert!(
            !locks.contains_key(&mac_a),
            "stale mac_lock entry must have been evicted to keep the map \
             bounded; current keys = {:?}",
            locks.keys().collect::<Vec<_>>()
        );
        assert!(
            locks.contains_key(&mac_b),
            "the freshly-acquired entry must be retained"
        );
    }

    #[tokio::test]
    async fn acquire_mac_lock_retains_in_use_entries() {
        let (manager, _store) = manager_with_creds();
        let mac_busy = mac(0x20);
        let mac_other = mac(0x21);

        // Hold a clone of the Arc for mac_busy to simulate an in-flight
        // rotate that's currently inside its critical section.
        let in_flight = manager.acquire_mac_lock(mac_busy).await;

        // Touching a different MAC triggers GC. mac_busy must survive
        // because `in_flight` keeps the Arc strong_count above 1.
        let _other = manager.acquire_mac_lock(mac_other).await;

        let locks = manager.mac_locks.lock().await;
        assert!(
            locks.contains_key(&mac_busy),
            "entry held by an in-flight caller must NOT be evicted — \
             otherwise concurrent rotates would race past the per-MAC lock"
        );
        drop(in_flight);
    }

    struct CountingCredentialManager {
        creds: Credentials,
        in_flight: Mutex<HashMap<MacAddress, u32>>,
        peak: Mutex<HashMap<MacAddress, u32>>,
        hold: std::time::Duration,
    }

    impl CountingCredentialManager {
        fn new(creds: Credentials, hold: std::time::Duration) -> Arc<Self> {
            Arc::new(Self {
                creds,
                in_flight: Mutex::new(HashMap::new()),
                peak: Mutex::new(HashMap::new()),
                hold,
            })
        }

        async fn peak_for(&self, bmc_mac: MacAddress) -> u32 {
            self.peak.lock().await.get(&bmc_mac).copied().unwrap_or(0)
        }
    }

    #[async_trait]
    impl CredentialReader for CountingCredentialManager {
        async fn get_credentials(
            &self,
            key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            let bmc_mac = match key {
                CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                } => *bmc_mac_address,
                other => panic!("unexpected credential key in rotate path: {other:?}"),
            };

            let current = {
                let mut in_flight = self.in_flight.lock().await;
                let entry = in_flight.entry(bmc_mac).or_insert(0);
                *entry = entry.saturating_add(1);
                *entry
            };
            {
                let mut peak = self.peak.lock().await;
                let entry = peak.entry(bmc_mac).or_insert(0);
                if current > *entry {
                    *entry = current;
                }
            }

            tokio::time::sleep(self.hold).await;

            {
                let mut in_flight = self.in_flight.lock().await;
                if let Some(value) = in_flight.get_mut(&bmc_mac) {
                    *value = value.saturating_sub(1);
                }
            }

            Ok(Some(self.creds.clone()))
        }
    }

    #[async_trait]
    impl CredentialWriter for CountingCredentialManager {
        async fn get_credentials_from_writer(
            &self,
            key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            CredentialReader::get_credentials(self, key).await
        }

        async fn set_credentials(
            &self,
            _key: &CredentialKey,
            _credentials: &Credentials,
        ) -> Result<(), SecretsError> {
            unreachable!("rotate path never writes credentials")
        }

        async fn create_credentials(
            &self,
            _key: &CredentialKey,
            _credentials: &Credentials,
        ) -> Result<(), SecretsError> {
            unreachable!("rotate path never creates credentials")
        }

        async fn delete_credentials(&self, _key: &CredentialKey) -> Result<(), SecretsError> {
            unreachable!("rotate path never deletes credentials")
        }
    }

    impl CredentialManager for CountingCredentialManager {}

    #[tokio::test]
    async fn rotate_serializes_per_mac_even_across_distinct_spiffe_callers() {
        let bmc_proxy = Arc::new(ArcSwap::new(Arc::new(None)));
        let redfish_pool = carbide_redfish::nv_redfish::new_pool(bmc_proxy);
        let credential_manager = CountingCredentialManager::new(
            Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
            std::time::Duration::from_millis(50),
        );
        let store = InMemoryBmcSessionStore::new();
        let manager = Arc::new(BmcSessionManager::new(
            redfish_pool,
            credential_manager.clone(),
            store,
            TEST_LOCKOUT_THRESHOLD,
            false,
            TEST_MAX_SESSIONS_PER_CALLER,
        ));

        let bmc_mac = mac(0xAB);
        let bmc_addr = "127.0.0.1:1".parse().unwrap();

        let mut handles = Vec::new();
        for i in 0..16 {
            let manager = manager.clone();
            let spiffe = format!("svc-{i}");
            handles.push(tokio::spawn(async move {
                let _ = manager.mint_session(&spiffe, bmc_mac, bmc_addr).await;
            }));
        }
        for h in handles {
            h.await.expect("rotate task should not panic");
        }

        let peak = credential_manager.peak_for(bmc_mac).await;
        assert_eq!(
            peak, 1,
            "rotate must serialize per-MAC across distinct SPIFFE callers; \
             observed peak in-flight credential lookups = {peak}, want 1"
        );
    }

    #[tokio::test]
    async fn record_unauthorized_returns_none_below_threshold() {
        let (manager, _store) = manager_with_creds_and_threshold(3);
        let bmc_mac = mac(0xDE);
        for _ in 0..2 {
            assert!(manager.record_unauthorized(bmc_mac, 401).await.is_none());
            assert!(
                manager.check_not_locked_out(bmc_mac).await.is_none(),
                "breaker should not trip below threshold"
            );
        }
        let state = manager
            .lockouts
            .lock()
            .await
            .get(&bmc_mac)
            .cloned()
            .expect("state should exist after recording");
        assert_eq!(state.consecutive_unauthorized, 2);
        assert!(state.tripped_at.is_none());
    }

    #[tokio::test]
    async fn record_unauthorized_trips_at_threshold() {
        let _metrics = MetricsCapture::start();
        let (manager, _store) = manager_with_creds_and_threshold(3);
        let bmc_mac = mac(0xDE);
        assert!(manager.record_unauthorized(bmc_mac, 401).await.is_none());
        assert!(manager.record_unauthorized(bmc_mac, 401).await.is_none());
        let trip = manager
            .record_unauthorized(bmc_mac, 403)
            .await
            .expect("third unauthorized should trip the breaker");
        match trip {
            super::BmcSessionError::AvoidLockout {
                bmc_mac: got_mac,
                consecutive_unauthorized,
                last_status,
            } => {
                assert_eq!(got_mac, bmc_mac);
                assert_eq!(consecutive_unauthorized, 3);
                assert_eq!(last_status, 403);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        let still = manager
            .check_not_locked_out(bmc_mac)
            .await
            .expect("breaker should remain tripped");
        assert!(matches!(still, super::BmcSessionError::AvoidLockout { .. }));
    }

    #[tokio::test]
    async fn record_unauthorized_only_emits_avoid_lockout_on_the_tripping_request() {
        let _metrics = MetricsCapture::start();
        let (manager, _store) = manager_with_creds_and_threshold(2);
        let bmc_mac = mac(0xDE);
        assert!(manager.record_unauthorized(bmc_mac, 401).await.is_none());
        let trip = manager.record_unauthorized(bmc_mac, 401).await;
        assert!(matches!(
            trip,
            Some(super::BmcSessionError::AvoidLockout { .. })
        ));
        let follow_up = manager.record_unauthorized(bmc_mac, 401).await;
        assert!(
            follow_up.is_none(),
            "second AvoidLockout should not be emitted from record_unauthorized"
        );
    }

    const TEST_SPIFFE_SERVICE_ID: &str = "spiffe://example.test/service";
    const TEST_SESSION_ID: &str = "/redfish/v1/SessionService/Sessions/42";

    #[derive(Debug)]
    enum CleanupFailureCase {
        RevokePriorSession,
        ListSessionsForRevoke,
        RevokeUnpersistedSession,
        DeleteSessionRows,
    }

    #[derive(Debug, PartialEq)]
    struct CleanupFailureObservation {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        bmc_mac_address: Option<String>,
        spiffe_service_id: Option<String>,
        session: Option<String>,
        error: Option<String>,
        spiffe_service_id_kind: Option<CapturedFieldKind>,
        error_kind: Option<CapturedFieldKind>,
        counter_delta: f64,
    }

    fn observe_cleanup_failure(case: CleanupFailureCase) -> CleanupFailureObservation {
        let bmc_mac = mac(0xBC);
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| match case {
            CleanupFailureCase::RevokePriorSession => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::RevokePriorSession,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(TEST_SPIFFE_SERVICE_ID.to_string()),
                    session: Some(nv_redfish::core::ODataId::from(TEST_SESSION_ID.to_string())),
                    error: "DeleteError { status: 500 }".to_string(),
                });
            }
            CleanupFailureCase::ListSessionsForRevoke => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::ListSessionsForRevoke,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(TEST_SPIFFE_SERVICE_ID.to_string()),
                    session: None,
                    error: "ListError { status: 503 }".to_string(),
                });
            }
            CleanupFailureCase::RevokeUnpersistedSession => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::RevokeUnpersistedSession,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: Some(TEST_SPIFFE_SERVICE_ID.to_string()),
                    session: Some(nv_redfish::core::ODataId::from(TEST_SESSION_ID.to_string())),
                    error: "DeleteError { status: 500 }".to_string(),
                });
            }
            CleanupFailureCase::DeleteSessionRows => {
                carbide_instrument::emit(BmcSessionCleanupFailed {
                    operation: BmcSessionCleanupOperation::DeleteSessionRows,
                    bmc_mac_address: bmc_mac,
                    spiffe_service_id: None,
                    session: None,
                    error: "BMC session store error: database unavailable".to_string(),
                });
            }
        });
        assert_eq!(
            logs.len(),
            1,
            "each cleanup failure should write one record"
        );
        let log = logs.first().expect("cleanup failure Event did not log");
        let operation = log.field("operation").map(str::to_string);

        CleanupFailureObservation {
            level: log.level,
            metadata_name: log.metadata_name.clone(),
            message: log.message.clone(),
            event_name: log.field("event_name").map(str::to_string),
            metric_name: log.field("metric_name").map(str::to_string),
            operation: operation.clone(),
            bmc_mac_address: log.field("bmc_mac_address").map(str::to_string),
            spiffe_service_id: log.field("spiffe_service_id").map(str::to_string),
            session: log.field("session").map(str::to_string),
            error: log.field("error").map(str::to_string),
            spiffe_service_id_kind: log.field_kind("spiffe_service_id"),
            error_kind: log.field_kind("error"),
            counter_delta: metrics.counter_delta(
                CLEANUP_FAILURE_METRIC,
                &[(
                    "operation",
                    operation
                        .as_deref()
                        .expect("cleanup failure Event should label its operation"),
                )],
            ),
        }
    }

    fn expected_cleanup_failure(
        event_name: &str,
        message: &str,
        operation: &str,
        spiffe_service_id: Option<&str>,
        session: Option<&str>,
        error: &str,
    ) -> CleanupFailureObservation {
        CleanupFailureObservation {
            level: tracing::Level::WARN,
            metadata_name: event_name.to_string(),
            message: message.to_string(),
            event_name: Some(event_name.to_string()),
            metric_name: Some(CLEANUP_FAILURE_METRIC.to_string()),
            operation: Some(operation.to_string()),
            bmc_mac_address: Some(mac(0xBC).to_string()),
            spiffe_service_id: spiffe_service_id.map(str::to_string),
            session: session.map(str::to_string),
            error: Some(error.to_string()),
            spiffe_service_id_kind: spiffe_service_id.map(|_| CapturedFieldKind::Debug),
            error_kind: Some(CapturedFieldKind::Debug),
            counter_delta: 1.0,
        }
    }

    #[test]
    fn cleanup_failures_log_and_count_by_operation() {
        value_scenarios!(
            run = observe_cleanup_failure;
            "prior session revoke fails" {
                CleanupFailureCase::RevokePriorSession => expected_cleanup_failure(
                    "bmc_session_cleanup_failed",
                    "failed to revoke an excess BMC session; it will leak until BMC idle timeout",
                    "revoke_prior_session",
                    Some(TEST_SPIFFE_SERVICE_ID),
                    Some(TEST_SESSION_ID),
                    "DeleteError { status: 500 }",
                ),
            }
            "session listing for prior revoke fails" {
                CleanupFailureCase::ListSessionsForRevoke => expected_cleanup_failure(
                    "bmc_session_cleanup_failed",
                    "failed to list BMC sessions for excess-session revoke; continuing",
                    "list_sessions_for_revoke",
                    Some(TEST_SPIFFE_SERVICE_ID),
                    None,
                    "ListError { status: 503 }",
                ),
            }
            "unpersisted session rollback revoke fails" {
                CleanupFailureCase::RevokeUnpersistedSession => expected_cleanup_failure(
                    "bmc_session_cleanup_failed",
                    "failed to revoke just-created session after store upsert failed; it will leak until BMC idle timeout",
                    "revoke_unpersisted_session",
                    Some(TEST_SPIFFE_SERVICE_ID),
                    Some(TEST_SESSION_ID),
                    "DeleteError { status: 500 }",
                ),
            }
            "flush store deletion fails" {
                CleanupFailureCase::DeleteSessionRows => expected_cleanup_failure(
                    "bmc_session_cleanup_failed",
                    "failed to delete BMC session rows during flush_mac; continuing",
                    "delete_session_rows",
                    None,
                    None,
                    "BMC session store error: database unavailable",
                ),
            }
        );
    }

    const BREAKER_TRANSITION_METRIC: &str = "carbide_bmc_session_lockout_breaker_transitions_total";

    #[derive(Clone, Copy)]
    enum BreakerTransitionCase {
        Trip,
        ClearExisting,
        ClearMissing,
    }

    #[derive(Debug, PartialEq)]
    struct BreakerTransitionObservation {
        tripped_delta: f64,
        cleared_delta: f64,
        unauthorized_results: Vec<bool>,
        remains_locked_out: bool,
        logs: Vec<BreakerTransitionLog>,
    }

    #[derive(Debug, PartialEq)]
    struct BreakerTransitionLog {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        transition: Option<String>,
        bmc_mac_address: Option<String>,
        http_status: Option<String>,
        consecutive_unauthorized_count: Option<String>,
        lockout_threshold_count: Option<String>,
        transition_kind: Option<CapturedFieldKind>,
        bmc_mac_address_kind: Option<CapturedFieldKind>,
        http_status_kind: Option<CapturedFieldKind>,
        consecutive_unauthorized_count_kind: Option<CapturedFieldKind>,
        lockout_threshold_count_kind: Option<CapturedFieldKind>,
    }

    fn observe_breaker_transition(case: BreakerTransitionCase) -> BreakerTransitionObservation {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let (manager, _store) = manager_with_creds_and_threshold(2);
        let bmc_mac = mac(0xDE);
        let metrics = MetricsCapture::start();
        let mut unauthorized_results = Vec::new();

        let logs = capture_logs(|| {
            runtime.block_on(async {
                match case {
                    BreakerTransitionCase::Trip => {
                        unauthorized_results
                            .push(manager.record_unauthorized(bmc_mac, 401).await.is_some());
                        unauthorized_results
                            .push(manager.record_unauthorized(bmc_mac, 403).await.is_some());
                        unauthorized_results
                            .push(manager.record_unauthorized(bmc_mac, 401).await.is_some());
                    }
                    BreakerTransitionCase::ClearExisting => {
                        manager.force_trip_for_test(bmc_mac, 4, 403).await;
                        manager.clear_lockout(bmc_mac).await;
                    }
                    BreakerTransitionCase::ClearMissing => {
                        manager.clear_lockout(bmc_mac).await;
                    }
                }
            });
        })
        .into_iter()
        .filter(|log| log.field("metric_name") == Some(BREAKER_TRANSITION_METRIC))
        .map(|log| {
            let event_name = log.field("event_name").map(str::to_string);
            let metric_name = log.field("metric_name").map(str::to_string);
            let transition = log.field("transition").map(str::to_string);
            let bmc_mac_address = log.field("bmc_mac_address").map(str::to_string);
            let http_status = log.field("http_status").map(str::to_string);
            let consecutive_unauthorized_count = log
                .field("consecutive_unauthorized_count")
                .map(str::to_string);
            let lockout_threshold_count = log.field("lockout_threshold_count").map(str::to_string);
            let transition_kind = log.field_kind("transition");
            let bmc_mac_address_kind = log.field_kind("bmc_mac_address");
            let http_status_kind = log.field_kind("http_status");
            let consecutive_unauthorized_count_kind =
                log.field_kind("consecutive_unauthorized_count");
            let lockout_threshold_count_kind = log.field_kind("lockout_threshold_count");

            BreakerTransitionLog {
                metadata_name: log.metadata_name,
                level: log.level,
                message: log.message,
                event_name,
                metric_name,
                transition,
                bmc_mac_address,
                http_status,
                consecutive_unauthorized_count,
                lockout_threshold_count,
                transition_kind,
                bmc_mac_address_kind,
                http_status_kind,
                consecutive_unauthorized_count_kind,
                lockout_threshold_count_kind,
            }
        })
        .collect();

        let remains_locked_out = runtime
            .block_on(manager.check_not_locked_out(bmc_mac))
            .is_some();
        BreakerTransitionObservation {
            tripped_delta: metrics
                .counter_delta(BREAKER_TRANSITION_METRIC, &[("transition", "tripped")]),
            cleared_delta: metrics
                .counter_delta(BREAKER_TRANSITION_METRIC, &[("transition", "cleared")]),
            unauthorized_results,
            remains_locked_out,
            logs,
        }
    }

    fn expected_breaker_transition_log(
        event_name: &str,
        level: tracing::Level,
        message: &str,
        transition: &str,
        bmc_mac: MacAddress,
        trip_context: Option<(u16, u32, u32)>,
    ) -> BreakerTransitionLog {
        let (http_status, consecutive_unauthorized_count, lockout_threshold_count) = trip_context
            .map(|(status, consecutive, threshold)| {
                (
                    Some(status.to_string()),
                    Some(consecutive.to_string()),
                    Some(threshold.to_string()),
                )
            })
            .unwrap_or_default();
        let native_number_kind = trip_context.map(|_| CapturedFieldKind::I64);

        BreakerTransitionLog {
            metadata_name: event_name.to_string(),
            level,
            message: message.to_string(),
            event_name: Some(event_name.to_string()),
            metric_name: Some(BREAKER_TRANSITION_METRIC.to_string()),
            transition: Some(transition.to_string()),
            bmc_mac_address: Some(bmc_mac.to_string()),
            http_status,
            consecutive_unauthorized_count,
            lockout_threshold_count,
            transition_kind: Some(CapturedFieldKind::String),
            bmc_mac_address_kind: Some(CapturedFieldKind::Debug),
            http_status_kind: native_number_kind,
            consecutive_unauthorized_count_kind: native_number_kind,
            lockout_threshold_count_kind: native_number_kind,
        }
    }

    #[test]
    fn breaker_transitions_log_and_count_once() {
        let bmc_mac = mac(0xDE);
        check_values(
            [
                Check {
                    scenario: "the first threshold crossing trips once",
                    input: BreakerTransitionCase::Trip,
                    expect: BreakerTransitionObservation {
                        tripped_delta: 1.0,
                        cleared_delta: 0.0,
                        unauthorized_results: vec![false, true, false],
                        remains_locked_out: true,
                        logs: vec![expected_breaker_transition_log(
                            "bmc_session_lockout_breaker_tripped",
                            tracing::Level::WARN,
                            "BmcSessionManager: lockout-avoidance breaker tripped",
                            "tripped",
                            bmc_mac,
                            Some((403, 2, 2)),
                        )],
                    },
                },
                Check {
                    scenario: "removing existing breaker state clears once",
                    input: BreakerTransitionCase::ClearExisting,
                    expect: BreakerTransitionObservation {
                        tripped_delta: 0.0,
                        cleared_delta: 1.0,
                        unauthorized_results: Vec::new(),
                        remains_locked_out: false,
                        logs: vec![expected_breaker_transition_log(
                            "bmc_session_lockout_breaker_cleared",
                            tracing::Level::INFO,
                            "BmcSessionManager: lockout-avoidance breaker cleared",
                            "cleared",
                            bmc_mac,
                            None,
                        )],
                    },
                },
                Check {
                    scenario: "clearing a missing breaker is a no-op",
                    input: BreakerTransitionCase::ClearMissing,
                    expect: BreakerTransitionObservation {
                        tripped_delta: 0.0,
                        cleared_delta: 0.0,
                        unauthorized_results: Vec::new(),
                        remains_locked_out: false,
                        logs: Vec::new(),
                    },
                },
            ],
            observe_breaker_transition,
        );
    }

    #[tokio::test]
    async fn clear_lockout_removes_tripped_state() {
        let _metrics = MetricsCapture::start();
        let (manager, _store) = manager_with_creds_and_threshold(1);
        let bmc_mac = mac(0xEE);
        manager.force_trip_for_test(bmc_mac, 1, 401).await;
        assert!(manager.check_not_locked_out(bmc_mac).await.is_some());
        manager.clear_lockout(bmc_mac).await;
        assert!(manager.check_not_locked_out(bmc_mac).await.is_none());
        assert!(!manager.lockouts.lock().await.contains_key(&bmc_mac));
    }

    #[tokio::test]
    async fn rotate_short_circuits_when_breaker_tripped() {
        let (manager, _store) = manager_with_creds_and_threshold(1);
        let bmc_mac = mac(0xF1);
        manager.force_trip_for_test(bmc_mac, 7, 401).await;

        let bmc_addr = "127.0.0.1:9999".parse().unwrap();
        let err = manager
            .mint_session("svc-locked", bmc_mac, bmc_addr)
            .await
            .expect_err("rotate must refuse to contact a locked-out BMC");
        match err {
            super::BmcSessionError::AvoidLockout {
                bmc_mac: got,
                consecutive_unauthorized,
                last_status,
            } => {
                assert_eq!(got, bmc_mac);
                assert_eq!(consecutive_unauthorized, 7);
                assert_eq!(last_status, 401);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_unauthorized_records_trip_exactly_once() {
        let _metrics = MetricsCapture::start();
        let (manager, _store) = manager_with_creds_and_threshold(3);
        let bmc_mac = mac(0xF2);

        let mut handles = Vec::new();
        for _ in 0..16 {
            let manager = manager.clone();
            handles.push(tokio::spawn(async move {
                manager.record_unauthorized(bmc_mac, 401).await
            }));
        }

        let mut trips = 0;
        for h in handles {
            if matches!(
                h.await.expect("task panicked"),
                Some(super::BmcSessionError::AvoidLockout { .. })
            ) {
                trips += 1;
            }
        }
        assert_eq!(
            trips, 1,
            "exactly one record_unauthorized should report a trip"
        );

        let state = manager
            .lockouts
            .lock()
            .await
            .get(&bmc_mac)
            .cloned()
            .expect("state present after concurrent records");
        assert!(state.tripped_at.is_some());
        assert!(state.consecutive_unauthorized >= 3);
    }

    #[tokio::test]
    async fn issue_credentials_with_flag_off_surfaces_no_session_service_error() {
        let bmc_proxy = Arc::new(ArcSwap::new(Arc::new(None)));
        let redfish_pool = carbide_redfish::nv_redfish::new_pool(bmc_proxy);
        let credential_manager = Arc::new(TestCredentialManager::default());
        let store = InMemoryBmcSessionStore::new();
        let manager = BmcSessionManager::new(
            redfish_pool,
            credential_manager,
            store,
            TEST_LOCKOUT_THRESHOLD,
            false,
            TEST_MAX_SESSIONS_PER_CALLER,
        );

        let bmc_mac = mac(0xA1);
        let bmc_addr = "127.0.0.1:9999".parse().unwrap();
        let err = manager
            .issue_credentials("svc-x", bmc_mac, bmc_addr)
            .await
            .map(|_| ())
            .expect_err("flag off must propagate the underlying rotate() error");
        assert!(
            matches!(err, BmcSessionError::MissingRootCredentials(_)),
            "expected MissingRootCredentials passthrough, got {err:?}"
        );
    }
}
