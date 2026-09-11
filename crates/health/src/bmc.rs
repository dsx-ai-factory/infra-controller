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

use std::any::{Any, type_name};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use carbide_utils::redfish::format_forwarded_host_parameter;
use carbide_uuid::rack::RackId;
use futures::TryStreamExt;
use http::HeaderMap;
use http::header::{self, InvalidHeaderValue};
use nv_redfish::bmc_http::reqwest::{BmcError, Client as ReqwestClient};
use nv_redfish::bmc_http::{CacheSettings, ConcurrencyLimitedBmc, HttpBmc, HttpClient};
use nv_redfish::core::query::{ExpandQuery, FilterQuery};
use nv_redfish::core::upload::{MultipartUpdateRequest, UploadReader};
use nv_redfish::core::{
    Action, Bmc, BoxTryStream, EntityTypeRef, Expandable, ModificationResponse, ODataETag, ODataId,
    SessionCreateResponse,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::HealthError;
use crate::endpoint::{BmcAddr, BmcCredentials, EndpointMetadata};
use crate::metrics::{BmcLatencyMetrics, BmcLatencyObservation};

pub(crate) const CREDENTIAL_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the per-endpoint circuit stays open after the first connect-level
/// failure. Subsequent failed probes double this up to [`CIRCUIT_MAX_BACKOFF`].
const CIRCUIT_INITIAL_BACKOFF: Duration = Duration::from_secs(5);
/// Upper bound on the circuit backoff window.
const CIRCUIT_MAX_BACKOFF: Duration = Duration::from_secs(300);
/// How long a single half-open probe is allowed to run before the circuit lets
/// another caller probe. This only matters if a probe is lost (e.g. its future
/// is cancelled) — it stops the circuit from latching half-open forever. It is
/// deliberately longer than a BMC connect timeout.
const CIRCUIT_PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a credential generation stays marked as known-bad after a
/// post-refresh retry was refused with it.
///
/// Long enough that a full collection sweep against a misconfigured endpoint
/// costs one refresh rather than one per resource, short enough that
/// credentials repaired out of band are picked up within a couple of intervals.
const KNOWN_BAD_CREDENTIAL_COOLDOWN: Duration = Duration::from_secs(60);

/// How long a credential fetch the provider failed is remembered before another
/// caller may drive the provider again for this endpoint.
///
/// Nothing else damps a provider failure: no request reaches the BMC, so the
/// connection circuit never trips, and the known-bad record only covers
/// credentials that were fetched and then refused. The value matches
/// [`KNOWN_BAD_CREDENTIAL_COOLDOWN`] for the same reason it has: a sweep against
/// an endpoint whose credentials cannot be issued costs one provider call, and a
/// provider that recovers is noticed within a couple of collection intervals.
const CREDENTIAL_FETCH_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

/// Per-endpoint connection circuit breaker state.
///
/// When a BMC stops answering at the network level, every collector sharing the
/// endpoint's [`BmcClient`] would otherwise keep firing requests that each block
/// for a full TCP connect timeout — hundreds of them per sensor sweep — and log
/// a warning apiece. The breaker short-circuits those requests after the first
/// connect-level failure so a dead endpoint costs one failed probe per backoff
/// window instead of a flood. See NVBug 6036327.
#[derive(Debug)]
enum CircuitState {
    /// Requests flow normally.
    Closed,
    /// Requests fast-fail until `until`; `backoff` is the window that was applied.
    Open { until: Instant, backoff: Duration },
    /// A single probe has been let through and is in flight until `deadline`;
    /// other callers fast-fail. `backoff` is the window to escalate from if the
    /// probe fails.
    Probing {
        deadline: Instant,
        backoff: Duration,
    },
}

/// What a batch-oriented collector should do this iteration, derived from the
/// endpoint's circuit state via [`BmcClient::collector_sweep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorSweep {
    /// Circuit closed — run the full batch as normal.
    Full,
    /// Backoff window elapsed — send a single probe to test reachability instead
    /// of the full fan-out, so a still-dead BMC costs one request, not hundreds.
    Probe,
    /// Circuit open within the backoff window — skip entirely.
    Skip,
}

/// A credential generation that a post-refresh retry already proved wrong.
///
/// Auth failures deliberately do not open the connection circuit — the BMC is
/// answering, it is just refusing us — so nothing else damps them. Without this
/// record a misconfigured endpoint pays a refresh and a replay on *every* read:
/// each caller observes a fresh generation, so each one does a real credential
/// fetch. Remembering the generation that was just refused collapses a sweep to
/// one refresh, and the cooldown still lets the client notice credentials that
/// were repaired out of band.
#[derive(Debug, Clone, Copy)]
struct KnownBadCredentials {
    generation: u64,
    proven_at: Instant,
}

/// The most recent credential fetch the provider failed for this endpoint.
///
/// Both paths that drive the provider retry on every read unless something
/// remembers the failure: a failed initial fetch leaves the init cell empty, and
/// a failed refresh leaves the generation unchanged. Every collector sharing the
/// client then pays a provider call per resource, so one endpoint whose
/// credentials Core refuses to issue turns each sweep into hundreds of failed
/// `GetBmcCredentials` RPCs, each a database lookup and an error span on the
/// Core side. Remembering the failure fast-fails those callers locally and
/// admits one per cooldown to try the provider again.
#[derive(Debug)]
struct CredentialFetchFailure {
    failed_at: Instant,
    /// The provider's error, rendered, so the fast-fail error still says why
    /// credentials are unavailable and not only that a fetch was skipped.
    error: String,
}

/// Whether `cooldown` has elapsed since `since`, admitting the caller that
/// observes it and restarting the window for everyone else.
///
/// Restarting rather than clearing matters under the collectors' concurrent
/// fan-out: clearing would let every caller queued behind the owning lock
/// through at once, turning the single revalidation back into the flood the
/// cooldown exists to prevent. The admitted caller either moves past the record
/// or records a fresh verdict.
fn admit_one_after_cooldown(since: &mut Instant, cooldown: Duration) -> bool {
    if since.elapsed() < cooldown {
        return false;
    }
    *since = Instant::now();
    true
}

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait CredentialProvider: Send + Sync {
    fn fetch_credentials<'a>(
        &'a self,
        endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>>;
}

#[derive(Clone)]
pub struct FixedCredentialProvider {
    credentials: BmcCredentials,
}

impl FixedCredentialProvider {
    pub fn new(credentials: BmcCredentials) -> Self {
        Self { credentials }
    }
}

impl CredentialProvider for FixedCredentialProvider {
    fn fetch_credentials<'a>(
        &'a self,
        _endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        let credentials = self.credentials.clone();
        Box::pin(async move { Ok(credentials) })
    }
}

#[derive(Clone, Debug, Default)]
struct BmcIdentity {
    vendor: Option<String>,
    model: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BmcLatencyEndpointLabels {
    pub(crate) machine_id: Option<String>,
    pub(crate) rack_id: Option<String>,
}

impl BmcLatencyEndpointLabels {
    pub(crate) fn new(machine_id: Option<String>, rack_id: Option<String>) -> Self {
        Self {
            machine_id,
            rack_id,
        }
    }
}

#[derive(Clone)]
pub(crate) struct BmcLatencyInstrumentation {
    metrics: Arc<BmcLatencyMetrics>,
    endpoint_labels: BmcLatencyEndpointLabels,
}

impl BmcLatencyInstrumentation {
    pub(crate) fn new(
        metrics: Arc<BmcLatencyMetrics>,
        endpoint_labels: BmcLatencyEndpointLabels,
    ) -> Self {
        Self {
            metrics,
            endpoint_labels,
        }
    }
}

pub(crate) fn bmc_latency_endpoint_labels(
    metadata: Option<&EndpointMetadata>,
    rack_id: Option<&RackId>,
) -> BmcLatencyEndpointLabels {
    let machine_id = match metadata {
        Some(EndpointMetadata::Machine(machine)) => machine.machine_id.map(|id| id.to_string()),
        _ => None,
    };
    BmcLatencyEndpointLabels::new(machine_id, rack_id.map(ToString::to_string))
}

pub struct BmcClient {
    /// Enforces this endpoint's configured Redfish operation limit.
    inner: ConcurrencyLimitedBmc<HttpBmc<InstrumentedHttpClient>>,
    addr: BmcAddr,
    provider: Arc<dyn CredentialProvider>,
    credential_generation: AtomicU64,
    init: OnceCell<()>,
    refresh_lock: Mutex<()>,

    circuit: StdMutex<CircuitState>,
    /// Lock-free fast-path hint mirroring `circuit`: `false` iff the circuit is
    /// `Closed`. Lets the healthy request path (the overwhelmingly common case)
    /// skip the mutex entirely. It is only ever set to `true` while holding the
    /// `circuit` lock as the state leaves `Closed`, and cleared while holding it
    /// as the state returns to `Closed`, so the invariant "circuit is blocking
    /// ⟹ `circuit_tripped` is `true`" always holds. A stale `false` read just
    /// means a request that was already racing an open-transition proceeds —
    /// harmless, identical to a request already in flight when the circuit trips.
    circuit_tripped: AtomicBool,
    /// Set once a refresh-and-replay has been refused; see
    /// [`KnownBadCredentials`]. Only touched on the auth-failure path, so the
    /// healthy request path never takes this lock.
    known_bad_credentials: StdMutex<Option<KnownBadCredentials>>,
    /// Set while the provider's last fetch for this endpoint failed; see
    /// [`CredentialFetchFailure`]. Only touched around a fetch attempt, so the
    /// healthy request path never takes this lock.
    credential_fetch_failure: StdMutex<Option<CredentialFetchFailure>>,
    bmc_identity: Arc<StdMutex<BmcIdentity>>,
}

impl BmcClient {
    pub(crate) fn new(
        reqwest: ReqwestClient,
        addr: BmcAddr,
        provider: Arc<dyn CredentialProvider>,
        proxy_url: Option<Url>,
        cache_size: usize,
        request_concurrency: NonZeroUsize,
        bmc_latency_instrumentation: Option<BmcLatencyInstrumentation>,
    ) -> Result<Self, HealthError> {
        let server_address = bmc_server_address(&addr);
        let url_scheme = bmc_url_scheme(&addr).to_string();
        let bmc_url = bmc_url(&addr, proxy_url.as_ref())?;
        let headers = bmc_headers(&addr, proxy_url.as_ref())?;
        let bmc_identity = Arc::new(StdMutex::new(BmcIdentity::default()));
        let http_client = InstrumentedHttpClient::new(
            reqwest,
            bmc_latency_instrumentation,
            server_address,
            url_scheme,
            Arc::clone(&bmc_identity),
        );

        // Currently nv-redfish BMC requires credentials, so this placeholder is used;
        // they will be replaced as soon as we call ensure_credentials.
        let placeholder =
            nv_redfish::bmc_http::BmcCredentials::username_password(String::new(), None::<String>);
        let inner = HttpBmc::with_custom_headers(
            http_client,
            bmc_url,
            placeholder,
            CacheSettings::with_capacity(cache_size),
            headers,
        )
        .with_request_concurrency_limit(request_concurrency);

        Ok(Self {
            inner,
            addr,
            provider,
            credential_generation: AtomicU64::new(0),
            init: OnceCell::new(),
            refresh_lock: Mutex::new(()),
            circuit: StdMutex::new(CircuitState::Closed),
            circuit_tripped: AtomicBool::new(false),
            known_bad_credentials: StdMutex::new(None),
            credential_fetch_failure: StdMutex::new(None),
            bmc_identity,
        })
    }

    fn note_bmc_identity_from<T: 'static>(&self, value: &T) {
        if let Some(root) =
            (value as &dyn Any).downcast_ref::<nv_redfish::schema::service_root::ServiceRoot>()
        {
            self.note_bmc_identity(
                root.vendor.as_ref().and_then(Option::as_deref),
                root.product.as_ref().and_then(Option::as_deref),
            );
        }
    }

    fn note_bmc_identity(&self, vendor: Option<&str>, model: Option<&str>) {
        let vendor = vendor.and_then(normalize_identity_value);
        let model = model.and_then(normalize_identity_value);
        if vendor.is_none() && model.is_none() {
            return;
        }

        let mut identity = self
            .bmc_identity
            .lock()
            .expect("BMC identity mutex poisoned");
        if let Some(vendor) = vendor {
            identity.vendor = Some(vendor);
        }
        if let Some(model) = model {
            identity.model = Some(model);
        }
    }

    pub async fn ensure_credentials(&self) -> Result<(), HealthError> {
        self.init
            .get_or_try_init(|| async {
                // The cell runs one initializer at a time, so a fan-out that
                // arrives while the provider is failing queues here, and each
                // queued caller then observes the recorded failure instead of
                // driving the provider again.
                let credentials = self.fetch_credentials().await.map_err(|error| {
                    HealthError::GenericError(format!(
                        "failed to fetch initial BMC credentials: {error}"
                    ))
                })?;
                self.inner.set_credentials(credentials.into());
                self.credential_generation.fetch_add(1, Ordering::AcqRel);
                Ok::<_, HealthError>(())
            })
            .await?;
        Ok(())
    }

    pub fn credential_provider(&self) -> Arc<dyn CredentialProvider> {
        self.provider.clone()
    }

    async fn refresh_credentials(
        &self,
        error: &HealthError,
        observed_generation: Option<u64>,
    ) -> Result<(), HealthError> {
        let _guard = self.refresh_lock.lock().await;
        if observed_generation.is_some_and(|generation| {
            generation != self.credential_generation.load(Ordering::Acquire)
        }) {
            return Ok(());
        }

        // Debug, not warn: the caller replays the request after this, so a first
        // auth failure is an expected consequence of credential rotation and is
        // recovered from silently. Only a retry that fails *after* the refresh
        // warrants operator attention.
        tracing::debug!(
            error = ?error,
            endpoint = ?self.addr,
            "Authentication failed, refreshing BMC credentials"
        );

        let credentials = self.fetch_credentials().await.map_err(|refresh_error| {
            HealthError::GenericError(format!(
                "failed to refresh BMC credentials after auth error {error}: {refresh_error}"
            ))
        })?;
        self.inner.set_credentials(credentials.into());
        self.credential_generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Drive the provider for this endpoint, bounded by
    /// [`CREDENTIAL_REFRESH_TIMEOUT`] and damped by the fetch-failure cooldown.
    ///
    /// Callers hold whichever lock serialises their path, the init cell or
    /// `refresh_lock`, so the cooldown check and the fetch it admits cannot
    /// interleave with another caller's fetch through this client.
    async fn fetch_credentials(&self) -> Result<BmcCredentials, HealthError> {
        self.check_credential_fetch_cooldown()?;
        let result = match tokio::time::timeout(
            CREDENTIAL_REFRESH_TIMEOUT,
            self.provider.fetch_credentials(&self.addr),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(HealthError::GenericError(format!(
                "timed out after {}s fetching BMC credentials",
                CREDENTIAL_REFRESH_TIMEOUT.as_secs(),
            ))),
        };
        // A success clears the record rather than leaving the restarted window
        // behind: the admitted retry can be followed at once by a 401 on the
        // credentials it just fetched, and that refresh has to be allowed to run.
        *self
            .credential_fetch_failure
            .lock()
            .expect("credential fetch failure mutex poisoned") =
            result.as_ref().err().map(|error| CredentialFetchFailure {
                failed_at: Instant::now(),
                error: error.to_string(),
            });
        result
    }

    /// Fail fast while the provider's last failure for this endpoint is inside
    /// the cooldown. Once it elapses, exactly one caller is admitted to retry.
    fn check_credential_fetch_cooldown(&self) -> Result<(), HealthError> {
        let mut failure = self
            .credential_fetch_failure
            .lock()
            .expect("credential fetch failure mutex poisoned");
        let Some(failure) = failure.as_mut() else {
            return Ok(());
        };
        let elapsed = failure.failed_at.elapsed();
        if admit_one_after_cooldown(&mut failure.failed_at, CREDENTIAL_FETCH_FAILURE_COOLDOWN) {
            return Ok(());
        }
        Err(HealthError::GenericError(format!(
            "BMC credential fetch skipped: the provider failed {}s ago and is retried after {}s: {}",
            elapsed.as_secs(),
            CREDENTIAL_FETCH_FAILURE_COOLDOWN.as_secs(),
            failure.error,
        )))
    }

    /// Run an idempotent read through the circuit breaker, retrying it once if it
    /// failed with an authentication error and credentials were refreshed.
    ///
    /// BMC credentials rotate underneath us, so a long-lived client will
    /// intermittently see a 401 on an otherwise healthy endpoint. Refreshing
    /// without replaying the request leaves the caller holding a failure for a
    /// resource that is perfectly readable with the new credentials — which for
    /// the metric collectors means the series behind that resource vanish for the
    /// interval and reappear on the next one. See NVBug 6506008.
    ///
    /// `op` is only ever re-run for reads (`get`/`expand`/`filter`/`stream`), so
    /// replaying it is safe. The retry is deliberately single-shot: it runs with
    /// credentials newer than the ones that were rejected, so a second 401 means
    /// the credentials themselves are wrong, not stale, and retrying again would
    /// just multiply load against a BMC that is refusing us.
    async fn read_with_auth_retry<T, Op, Fut>(&self, op: Op) -> Result<T, HealthError>
    where
        Op: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, HealthError>>,
    {
        self.ensure_credentials().await?;
        let observed_generation = self.credential_generation.load(Ordering::Acquire);
        let error = match self.guarded(op()).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if !is_auth_error(&error) {
            return Err(error);
        }

        // Suppress on the generation installed *now*, not the one this attempt
        // observed. Those differ whenever a concurrent caller refreshed while
        // this request was in flight, and each direction matters: if the
        // credentials moved on to something untested, this attempt's 401 is
        // stale and the replay must happen — suppressing it would drop the
        // resource for the interval, the very failure this branch fixes. If
        // they moved on to a generation already refused, replaying is pointless
        // however old this attempt's view is.
        if self.credentials_known_bad(self.credential_generation.load(Ordering::Acquire)) {
            return Err(error);
        }

        // A no-op refresh (another caller already rotated past
        // `observed_generation`) still counts as success: the credentials in
        // place are newer than the ones the failed attempt used.
        if let Err(refresh_error) = self
            .refresh_credentials(&error, Some(observed_generation))
            .await
        {
            tracing::error!(
                error = ?refresh_error,
                original_error = ?error,
                endpoint = ?self.addr,
                "Failed to refresh BMC credentials after authentication error"
            );
            return Err(error);
        }

        // Capture the generation the replay is about to run against, so the
        // record names the credentials that were actually refused. A concurrent
        // refresh can still swap them mid-flight; per-request credential capture
        // is the only way to close that, and the cost of being wrong is one
        // extra refresh a cooldown later.
        let retry_generation = self.credential_generation.load(Ordering::Acquire);
        self.guarded(op())
            .await
            .inspect(|_| {
                tracing::debug!(
                    original_error = ?error,
                    endpoint = ?self.addr,
                    "Retry after BMC credential refresh succeeded"
                );
            })
            .inspect_err(|retry_error| {
                // Only a credential rejection condemns the credentials, and
                // deliberately not every error `is_auth_error` accepts. A 500, a
                // decode error or a dropped connection says nothing about them,
                // and neither does a 403: that means this identity may not read
                // this *resource*, which is per-resource by definition. Treating
                // one as an endpoint-wide verdict would let a single forbidden
                // resource suppress the refresh for every other resource — and
                // since it is polled every interval, it would re-condemn the
                // credentials as fast as the cooldown expired.
                if is_credential_rejection(retry_error) {
                    self.note_credentials_refused(retry_generation);
                }
                // Freshly fetched credentials were refused too, so this is a
                // real misconfiguration rather than a rotation we raced. The
                // caller's own warning does not say that we already refreshed
                // and replayed, and that distinction is the whole diagnostic —
                // it has to be in the logs by default, not behind a debug level
                // an operator can only enable after the fact.
                tracing::warn!(
                    error = ?retry_error,
                    original_error = ?error,
                    endpoint = ?self.addr,
                    "Retry after BMC credential refresh also failed"
                );
            })
    }

    /// Whether `generation` was already proven wrong by a replay that ran with
    /// freshly fetched credentials and was refused anyway.
    ///
    /// Consumes the record once the cooldown elapses, so exactly one caller gets
    /// through to try a refresh again — credentials repaired out of band are
    /// picked up without every caller in the meantime paying for the discovery.
    fn credentials_known_bad(&self, generation: u64) -> bool {
        let mut known_bad = self
            .known_bad_credentials
            .lock()
            .expect("known-bad credential mutex poisoned");
        // A different generation means these credentials have not been tested
        // yet, so a stale record simply stops applying — there is nothing to
        // clear on the success path.
        let Some(bad) = known_bad.as_mut() else {
            return false;
        };
        if bad.generation != generation {
            return false;
        }
        // The admitted caller either refreshes past this generation or records
        // a fresh verdict.
        !admit_one_after_cooldown(&mut bad.proven_at, KNOWN_BAD_CREDENTIAL_COOLDOWN)
    }

    /// Record that a replay running at `generation` was refused, so subsequent
    /// callers observing the same generation skip the refresh-and-replay.
    fn note_credentials_refused(&self, generation: u64) {
        let mut known_bad = self
            .known_bad_credentials
            .lock()
            .expect("known-bad credential mutex poisoned");
        // Under the collectors' concurrent fan-out a slow replay can land after
        // another caller has already refreshed and condemned a later
        // generation. Its verdict is stale by then, and letting it overwrite
        // would un-suppress everyone using the current credentials.
        if known_bad.is_some_and(|bad| bad.generation > generation) {
            return;
        }
        *known_bad = Some(KnownBadCredentials {
            generation,
            proven_at: Instant::now(),
        });
    }

    /// Run a BMC operation through the connection circuit breaker.
    ///
    /// Fast-fails (without touching the network) while the circuit is open, and
    /// updates the breaker based on the outcome. A connect-level failure trips
    /// it. Any other outcome — a success, or a non-connection error such as a
    /// 404/auth/decode — means the BMC actually answered, so it closes the
    /// circuit. Closing on a non-connection error matters for the half-open
    /// probe: without it a reachable-but-erroring BMC would stay fast-failed
    /// until the probe deadline.
    async fn guarded<T>(
        &self,
        op: impl std::future::Future<Output = Result<T, HealthError>>,
    ) -> Result<T, HealthError> {
        self.check_circuit()?;
        match op.await {
            Ok(value) => {
                self.note_reachable();
                Ok(value)
            }
            Err(error) => {
                if is_connection_error(&error) {
                    self.trip_circuit(&error);
                } else {
                    // The BMC responded (just not happily); it is reachable.
                    self.note_reachable();
                }
                Err(error)
            }
        }
    }

    /// Gate an attempt against the circuit. Returns the fast-fail error while the
    /// circuit is open, and otherwise lets the caller proceed — promoting an
    /// expired `Open` (or a lost `Probing`) circuit to a fresh half-open probe.
    fn check_circuit(&self) -> Result<(), HealthError> {
        // Fast path: a healthy (closed) circuit never touches the mutex.
        if !self.circuit_tripped.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut state = self.circuit.lock().expect("circuit mutex poisoned");
        match *state {
            CircuitState::Closed => Ok(()),
            CircuitState::Open { until, backoff } => {
                if Instant::now() >= until {
                    *state = CircuitState::Probing {
                        deadline: Instant::now() + CIRCUIT_PROBE_TIMEOUT,
                        backoff,
                    };
                    Ok(())
                } else {
                    Err(self.circuit_open_error())
                }
            }
            CircuitState::Probing { deadline, backoff } => {
                // A probe is already in flight; everyone else waits. If the probe
                // was lost (deadline passed without a result), let a new one run.
                if Instant::now() >= deadline {
                    *state = CircuitState::Probing {
                        deadline: Instant::now() + CIRCUIT_PROBE_TIMEOUT,
                        backoff,
                    };
                    Ok(())
                } else {
                    Err(self.circuit_open_error())
                }
            }
        }
    }

    /// Record that the BMC answered, closing the circuit if it was open.
    fn note_reachable(&self) {
        // Fast path: already closed — nothing to do, no lock.
        if !self.circuit_tripped.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.circuit.lock().expect("circuit mutex poisoned");
        if !matches!(*state, CircuitState::Closed) {
            tracing::info!(endpoint = ?self.addr, "BMC is reachable again; closing connection circuit");
            *state = CircuitState::Closed;
        }
        // Clear the hint while still holding the lock so it can never lag the
        // state into a `Closed`-but-`tripped` window that the fast path would
        // wrongly treat as blocking.
        self.circuit_tripped.store(false, Ordering::Release);
    }

    /// Open (or escalate) the circuit after a connect-level failure.
    fn trip_circuit(&self, error: &HealthError) {
        let mut state = self.circuit.lock().expect("circuit mutex poisoned");
        match *state {
            CircuitState::Closed => {
                *state = CircuitState::Open {
                    until: Instant::now() + CIRCUIT_INITIAL_BACKOFF,
                    backoff: CIRCUIT_INITIAL_BACKOFF,
                };
                tracing::warn!(
                    endpoint = ?self.addr,
                    backoff_seconds = CIRCUIT_INITIAL_BACKOFF.as_secs(),
                    error = ?error,
                    "BMC connect failure; opening connection circuit to stop request flood"
                );
            }
            CircuitState::Probing { backoff, .. } => {
                // The half-open probe failed: keep the circuit open and back off
                // further before the next probe.
                let next = (backoff * 2).min(CIRCUIT_MAX_BACKOFF);
                *state = CircuitState::Open {
                    until: Instant::now() + next,
                    backoff: next,
                };
                tracing::debug!(
                    endpoint = ?self.addr,
                    backoff_seconds = next.as_secs(),
                    "BMC still unreachable; extending connection circuit backoff"
                );
            }
            // Already open: this is a request that was in flight before the
            // circuit opened. Leave the existing window untouched.
            CircuitState::Open { .. } => {}
        }
        // Every branch above leaves the circuit non-`Closed`; publish the hint
        // while still holding the lock so the fast path observes it.
        self.circuit_tripped.store(true, Ordering::Release);
    }

    /// What a batch-oriented caller (e.g. the sensor sweep) should do this
    /// iteration, so it can avoid both the request flood and its log spam:
    /// run the full batch, send a single probe, or skip entirely. Reading this
    /// once up front — rather than letting each request fast-fail individually —
    /// keeps a dead endpoint from re-emitting a per-request burst every time the
    /// backoff window elapses.
    pub fn collector_sweep(&self) -> CollectorSweep {
        // Fast path: a closed circuit runs normally and never takes the lock.
        if !self.circuit_tripped.load(Ordering::Acquire) {
            return CollectorSweep::Full;
        }
        let state = self.circuit.lock().expect("circuit mutex poisoned");
        match *state {
            CircuitState::Closed => CollectorSweep::Full,
            CircuitState::Open { until, .. } => {
                if Instant::now() < until {
                    CollectorSweep::Skip
                } else {
                    CollectorSweep::Probe
                }
            }
            CircuitState::Probing { deadline, .. } => {
                if Instant::now() < deadline {
                    CollectorSweep::Skip
                } else {
                    CollectorSweep::Probe
                }
            }
        }
    }

    fn circuit_open_error(&self) -> HealthError {
        HealthError::GenericError(format!(
            "BMC {} is unreachable; request skipped while the connection circuit breaker is open",
            self.addr.ip
        ))
    }

    /// Seed the circuit state and its fast-path hint coherently. Tests use this
    /// instead of writing the mutex directly so the `circuit_tripped` invariant
    /// is never violated.
    #[cfg(test)]
    fn set_circuit_for_test(&self, state: CircuitState) {
        let tripped = !matches!(state, CircuitState::Closed);
        *self.circuit.lock().expect("circuit mutex poisoned") = state;
        self.circuit_tripped.store(tripped, Ordering::Release);
    }

    /// Backdate (or clear) the known-bad record, so a test can reach the
    /// cooldown boundary without sleeping. The cooldown is measured against a
    /// `std::time::Instant`, which tokio's paused clock does not move.
    #[cfg(test)]
    fn set_known_bad_credentials_for_test(&self, record: Option<KnownBadCredentials>) {
        *self
            .known_bad_credentials
            .lock()
            .expect("known-bad credential mutex poisoned") = record;
    }

    /// Backdate (or clear) the fetch-failure record, for the same reason as
    /// [`Self::set_known_bad_credentials_for_test`].
    #[cfg(test)]
    fn set_credential_fetch_failure_for_test(&self, record: Option<CredentialFetchFailure>) {
        *self
            .credential_fetch_failure
            .lock()
            .expect("credential fetch failure mutex poisoned") = record;
    }
}

#[derive(Clone)]
struct InstrumentedHttpClient {
    inner: ReqwestClient,
    bmc_latency_instrumentation: Option<BmcLatencyInstrumentation>,
    server_address: String,
    url_scheme: String,
    bmc_identity: Arc<StdMutex<BmcIdentity>>,
}

impl InstrumentedHttpClient {
    fn new(
        inner: ReqwestClient,
        bmc_latency_instrumentation: Option<BmcLatencyInstrumentation>,
        server_address: String,
        url_scheme: String,
        bmc_identity: Arc<StdMutex<BmcIdentity>>,
    ) -> Self {
        Self {
            inner,
            bmc_latency_instrumentation,
            server_address,
            url_scheme,
            bmc_identity,
        }
    }

    fn observe(
        &self,
        method: &str,
        url: &Url,
        status_code: &str,
        entity_type: &str,
        duration: Duration,
    ) {
        let Some(instrumentation) = &self.bmc_latency_instrumentation else {
            return;
        };

        let identity = self
            .bmc_identity
            .lock()
            .expect("BMC identity mutex poisoned")
            .clone();
        let endpoint_labels = &instrumentation.endpoint_labels;
        instrumentation.metrics.observe(BmcLatencyObservation {
            status_code,
            method,
            path: url.path(),
            server_address: &self.server_address,
            url_scheme: &self.url_scheme,
            bmc_vendor: identity.vendor.as_deref(),
            bmc_model: identity.model.as_deref(),
            entity_type,
            machine_id: endpoint_labels.machine_id.as_deref(),
            rack_id: endpoint_labels.rack_id.as_deref(),
            duration,
        });
    }

    fn observe_result<T>(
        &self,
        method: &str,
        url: &Url,
        result: &Result<T, BmcError>,
        success_status_code: &str,
        duration: Duration,
    ) {
        let status_code = result_status_code(result, success_status_code);
        self.observe(method, url, &status_code, entity_type_name::<T>(), duration);
    }

    fn observe_modification_result<T>(
        &self,
        method: &str,
        url: &Url,
        result: &Result<ModificationResponse<T>, BmcError>,
        entity_status_code: &str,
        duration: Duration,
    ) {
        let status_code = modification_result_status_code(result, entity_status_code);
        self.observe(method, url, &status_code, entity_type_name::<T>(), duration);
    }
}

impl HttpClient for InstrumentedHttpClient {
    type Error = BmcError;

    async fn get<T>(
        &self,
        url: Url,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        etag: Option<ODataETag>,
        custom_headers: &HeaderMap,
    ) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self
            .inner
            .get::<T>(url, credentials, etag, custom_headers)
            .await;
        self.observe_result("GET", &request_url, &result, "200", started.elapsed());
        result
    }

    async fn post<B, T>(
        &self,
        url: Url,
        body: &B,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let entity_status_code = post_entity_status_code(&request_url);
        let result = self
            .inner
            .post::<B, T>(url, body, credentials, custom_headers)
            .await;
        self.observe_modification_result(
            "POST",
            &request_url,
            &result,
            entity_status_code,
            started.elapsed(),
        );
        result
    }

    async fn post_session<B, T>(
        &self,
        url: Url,
        body: &B,
        custom_headers: &HeaderMap,
    ) -> Result<SessionCreateResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self
            .inner
            .post_session::<B, T>(url, body, custom_headers)
            .await;
        self.observe_result("POST", &request_url, &result, "201", started.elapsed());
        result
    }

    async fn post_multipart_update<U, V, T>(
        &self,
        url: Url,
        request: MultipartUpdateRequest<'_, U, V>,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        U: UploadReader,
        T: DeserializeOwned + Send + Sync,
        V: Serialize + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self
            .inner
            .post_multipart_update::<U, V, T>(url, request, credentials, custom_headers)
            .await;
        self.observe_modification_result("POST", &request_url, &result, "200", started.elapsed());
        result
    }

    async fn patch<B, T>(
        &self,
        url: Url,
        etag: ODataETag,
        body: &B,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self
            .inner
            .patch::<B, T>(url, etag, body, credentials, custom_headers)
            .await;
        self.observe_modification_result("PATCH", &request_url, &result, "200", started.elapsed());
        result
    }

    async fn delete<T>(
        &self,
        url: Url,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self
            .inner
            .delete::<T>(url, credentials, custom_headers)
            .await;
        self.observe_modification_result("DELETE", &request_url, &result, "200", started.elapsed());
        result
    }

    async fn sse<T: Sized + for<'de> Deserialize<'de> + Send>(
        &self,
        url: Url,
        credentials: &nv_redfish::bmc_http::BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<BoxTryStream<T, Self::Error>, Self::Error> {
        let started = Instant::now();
        let request_url = url.clone();
        let result = self.inner.sse::<T>(url, credentials, custom_headers).await;
        self.observe_result("GET", &request_url, &result, "200", started.elapsed());
        result
    }
}

fn bmc_server_address(addr: &BmcAddr) -> String {
    addr.ip.to_string()
}

fn bmc_url_scheme(addr: &BmcAddr) -> &'static str {
    if addr.port.is_some_and(|port| port == 80) {
        "http"
    } else {
        "https"
    }
}

fn bmc_url(addr: &BmcAddr, proxy_url: Option<&Url>) -> Result<Url, HealthError> {
    match proxy_url {
        Some(url) => Ok(url.clone()),
        None => addr
            .to_url()
            .map_err(|e| HealthError::GenericError(e.to_string())),
    }
}

fn bmc_headers(addr: &BmcAddr, proxy_url: Option<&Url>) -> Result<HeaderMap, HealthError> {
    let mut headers = HeaderMap::new();
    if proxy_url.is_some() {
        headers.insert(
            header::FORWARDED,
            format_forwarded_host_parameter(&addr.ip.to_string())
                .parse()
                .map_err(|e: InvalidHeaderValue| HealthError::GenericError(e.to_string()))?,
        );
    }
    Ok(headers)
}

const UNKNOWN_HTTP_STATUS_CODE: &str = "unknown";

fn normalize_identity_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn result_status_code<T>(result: &Result<T, BmcError>, success_status_code: &str) -> String {
    match result {
        Ok(_) => success_status_code.to_string(),
        Err(error) => {
            bmc_error_status_code(error).unwrap_or_else(|| UNKNOWN_HTTP_STATUS_CODE.to_string())
        }
    }
}

fn modification_result_status_code<T>(
    result: &Result<ModificationResponse<T>, BmcError>,
    entity_status_code: &str,
) -> String {
    match result {
        Ok(ModificationResponse::Entity(_)) => entity_status_code.to_string(),
        Ok(ModificationResponse::Task(_)) => http::StatusCode::ACCEPTED.as_u16().to_string(),
        Ok(ModificationResponse::Empty) => http::StatusCode::NO_CONTENT.as_u16().to_string(),
        Err(error) => {
            bmc_error_status_code(error).unwrap_or_else(|| UNKNOWN_HTTP_STATUS_CODE.to_string())
        }
    }
}

fn bmc_error_status_code(error: &BmcError) -> Option<String> {
    if let BmcError::InvalidResponse { status, .. } = error {
        return Some(status.as_u16().to_string());
    }

    None
}

fn post_entity_status_code(url: &Url) -> &'static str {
    if url.path().contains("/Actions/") {
        "200"
    } else {
        "201"
    }
}

fn entity_type_name<T>() -> &'static str {
    let type_name = type_name::<T>();
    let without_generics = type_name.split('<').next().unwrap_or(type_name);
    without_generics
        .rsplit("::")
        .next()
        .unwrap_or(without_generics)
}

impl Bmc for BmcClient {
    type Error = HealthError;

    async fn expand<T: Expandable>(
        &self,
        id: &ODataId,
        query: ExpandQuery,
    ) -> Result<Arc<T>, Self::Error> {
        self.read_with_auth_retry(|| async {
            self.inner
                .expand(id, query.clone())
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn get<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
    ) -> Result<Arc<T>, Self::Error> {
        self.read_with_auth_retry(|| async move {
            let result = self.inner.get::<T>(id).await.map_err(HealthError::from);
            if let Ok(value) = &result {
                self.note_bmc_identity_from(value.as_ref());
            }
            result
        })
        .await
    }

    async fn filter<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
        query: FilterQuery,
    ) -> Result<Arc<T>, Self::Error> {
        self.read_with_auth_retry(|| async {
            self.inner
                .filter(id, query.clone())
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn create<V: Send + Sync + Serialize, R: Send + Sync + for<'de> Deserialize<'de>>(
        &self,
        id: &ODataId,
        query: &V,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        self.ensure_credentials().await?;
        self.guarded(async {
            self.inner
                .create(id, query)
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn update<
        V: Sync + Send + Serialize,
        R: Send + Sync + Sized + for<'de> Deserialize<'de>,
    >(
        &self,
        id: &ODataId,
        etag: Option<&ODataETag>,
        update: &V,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        self.ensure_credentials().await?;
        self.guarded(async {
            self.inner
                .update(id, etag, update)
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn multipart_update<U, V, R>(
        &self,
        uri: &str,
        request: MultipartUpdateRequest<'_, U, V>,
    ) -> Result<ModificationResponse<R>, Self::Error>
    where
        U: UploadReader,
        R: Send + Sync + for<'de> Deserialize<'de>,
        V: Send + Sync + Serialize,
    {
        self.ensure_credentials().await?;
        self.guarded(async {
            self.inner
                .multipart_update(uri, request)
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn delete<R: EntityTypeRef + for<'de> Deserialize<'de>>(
        &self,
        id: &ODataId,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        self.ensure_credentials().await?;
        self.guarded(async { self.inner.delete(id).await.map_err(HealthError::from) })
            .await
    }

    async fn action<
        T: Send + Sync + Serialize,
        R: Send + Sync + Sized + for<'de> Deserialize<'de>,
    >(
        &self,
        action: &Action<T, R>,
        params: &T,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        self.ensure_credentials().await?;
        self.guarded(async {
            self.inner
                .action(action, params)
                .await
                .map_err(HealthError::from)
        })
        .await
    }

    async fn stream<T: Sized + for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        uri: &str,
    ) -> Result<BoxTryStream<T, Self::Error>, Self::Error> {
        // Only stream *establishment* runs through the breaker and the auth
        // retry. Per-item errors on the returned long-lived stream (e.g. a
        // mid-stream SSE disconnect) are intentionally not fed back into them:
        // streaming collectors own a reconnect loop with their own exponential
        // backoff, and the breaker is scoped to the periodic-collector request
        // flood — many short requests against a dead endpoint — not a single
        // long-lived connection. Routing item errors here would also couple
        // log-stream health to sensor/discovery collection.
        let stream = self
            .read_with_auth_retry(|| async {
                self.inner.stream(uri).await.map_err(HealthError::from)
            })
            .await?;
        Ok(Box::pin(stream.map_err(HealthError::from)))
    }

    async fn create_session<
        V: Send + Sync + Serialize,
        R: Send + Sync + for<'de> Deserialize<'de>,
    >(
        &self,
        id: &ODataId,
        query: &V,
    ) -> Result<SessionCreateResponse<R>, Self::Error> {
        self.ensure_credentials().await?;
        self.guarded(async {
            self.inner
                .create_session(id, query)
                .await
                .map_err(HealthError::from)
        })
        .await
    }
}

pub(crate) fn is_auth_error(error: &HealthError) -> bool {
    match error {
        HealthError::HttpError(message) => {
            message.contains("HTTP 401") || message.contains("HTTP 403")
        }
        HealthError::BmcError(inner) => is_auth_bmc_source_error(inner.as_ref()),
        _ => false,
    }
}

/// Whether the BMC rejected the *credentials themselves*, as opposed to
/// refusing this identity access to one resource.
///
/// Strictly narrower than [`is_auth_error`]: 401 says the credentials are not
/// valid, which is true of the endpoint as a whole, while 403 says they are
/// valid but not privileged for the resource that was asked for. Only the
/// former generalises, so only the former may be recorded as an endpoint-wide
/// verdict. Retrying still uses the wider predicate — a rotation can change
/// which account is in play, so replaying a 403 once is harmless.
pub(crate) fn is_credential_rejection(error: &HealthError) -> bool {
    match error {
        HealthError::HttpError(message) => message.contains("HTTP 401"),
        HealthError::BmcError(inner) => is_credential_rejection_bmc_source_error(inner.as_ref()),
        _ => false,
    }
}

fn is_credential_rejection_bmc_source_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<BmcError>().is_some_and(|inner| {
        matches!(
            inner,
            BmcError::InvalidResponse { status, .. } if *status == http::StatusCode::UNAUTHORIZED
        )
    }) || error
        .downcast_ref::<HealthError>()
        .is_some_and(is_credential_rejection)
}

pub(crate) fn is_auth_bmc_source_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<BmcError>()
        .is_some_and(is_auth_bmc_error)
        || error
            .downcast_ref::<HealthError>()
            .is_some_and(is_auth_error)
}

fn is_auth_bmc_error(error: &BmcError) -> bool {
    matches!(
        error,
        BmcError::InvalidResponse { status, .. }
            if *status == http::StatusCode::UNAUTHORIZED || *status == http::StatusCode::FORBIDDEN
    )
}

/// Whether an error represents the BMC being unreachable at the transport layer
/// (TCP connect refused/timed out, or a request that timed out) — as opposed to
/// the BMC answering with an error. Only these trip the connection circuit
/// breaker; an HTTP 404 or a decode error means the BMC is alive and talking.
pub(crate) fn is_connection_error(error: &HealthError) -> bool {
    match error {
        HealthError::BmcError(inner) => is_connection_bmc_source_error(inner.as_ref()),
        _ => false,
    }
}

fn is_connection_bmc_source_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<BmcError>()
        .is_some_and(is_connection_bmc_error)
        || error
            .downcast_ref::<HealthError>()
            .is_some_and(is_connection_error)
}

fn is_connection_bmc_error(error: &BmcError) -> bool {
    matches!(error, BmcError::ReqwestError(e) if e.is_connect() || e.is_timeout())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use carbide_test_support::value_scenarios;
    use mac_address::MacAddress;
    use nv_redfish::bmc_http::reqwest::ClientParams as ReqwestClientParams;
    use prometheus::{Encoder, Registry, TextEncoder};

    use super::*;
    use crate::endpoint::BmcAddr;
    use crate::metrics::BmcLatencyAttribute;

    const TEST_REQUEST_CONCURRENCY: NonZeroUsize = NonZeroUsize::MIN;

    struct CountingProvider {
        calls: Arc<AtomicUsize>,
        delay: Option<Duration>,
        credentials: BmcCredentials,
    }

    impl CountingProvider {
        fn new(
            credentials: BmcCredentials,
            delay: Option<Duration>,
        ) -> (Arc<Self>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = Arc::new(Self {
                calls: calls.clone(),
                delay,
                credentials,
            });
            (provider, calls)
        }
    }

    impl CredentialProvider for CountingProvider {
        fn fetch_credentials<'a>(
            &'a self,
            _endpoint: &'a BmcAddr,
        ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
            let delay = self.delay;
            let credentials = self.credentials.clone();
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async move {
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                Ok(credentials)
            })
        }
    }

    /// Fails the first fetch and succeeds every fetch after it.
    struct FlakyProvider {
        attempts: AtomicUsize,
    }

    impl CredentialProvider for FlakyProvider {
        fn fetch_credentials<'a>(
            &'a self,
            _endpoint: &'a BmcAddr,
        ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
            let attempt = self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async move {
                if attempt == 0 {
                    Err(HealthError::GenericError("transient".to_string()))
                } else {
                    Ok(BmcCredentials::SessionToken {
                        token: "t".to_string(),
                    })
                }
            })
        }
    }

    /// Succeeds the first fetch (the initial load) and fails every fetch after
    /// it, standing in for a provider that can no longer issue credentials.
    struct InitOnceThenFailingProvider {
        calls: AtomicUsize,
    }

    impl CredentialProvider for InitOnceThenFailingProvider {
        fn fetch_credentials<'a>(
            &'a self,
            _endpoint: &'a BmcAddr,
        ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
            let call = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async move {
                if call == 0 {
                    Ok(BmcCredentials::SessionToken {
                        token: "t".to_string(),
                    })
                } else {
                    Err(HealthError::GenericError("vault down".to_string()))
                }
            })
        }
    }

    /// A fetch-failure record old enough that the next check admits a retry.
    fn expired_fetch_failure() -> Option<CredentialFetchFailure> {
        Some(CredentialFetchFailure {
            failed_at: Instant::now() - CREDENTIAL_FETCH_FAILURE_COOLDOWN - Duration::from_secs(1),
            error: "expired".to_string(),
        })
    }

    fn test_addr() -> BmcAddr {
        BmcAddr {
            ip: "10.0.0.1".parse().unwrap(),
            port: Some(443),
            mac: MacAddress::from_str("00:11:22:33:44:55").unwrap(),
        }
    }

    #[test]
    fn proxy_forwarded_header_formats_ip_literals() {
        value_scenarios!(run = |ip: &str| {
            let addr = BmcAddr {
                ip: ip.parse().unwrap(),
                port: Some(443),
                mac: MacAddress::from_str("00:11:22:33:44:55").unwrap(),
            };
            let proxy_url = Url::parse("https://proxy.example.com").unwrap();

            bmc_headers(&addr, Some(&proxy_url))
                .unwrap()
                .get(header::FORWARDED)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        };
            "Forwarded host parameter" {
                "192.0.2.10" => "host=192.0.2.10".to_string(),
                "2001:db8::10" => "host=\"[2001:db8::10]\"".to_string(),
            }
        );
    }

    fn reqwest() -> ReqwestClient {
        ReqwestClient::with_params(ReqwestClientParams::new().accept_invalid_certs(true))
            .expect("reqwest client builds")
    }

    fn reqwest_with_timeout(timeout: Duration) -> ReqwestClient {
        ReqwestClient::with_params(
            ReqwestClientParams::new()
                .accept_invalid_certs(true)
                .timeout(timeout),
        )
        .expect("reqwest client builds")
    }

    fn test_bmc(
        reqwest: ReqwestClient,
        addr: BmcAddr,
        provider: Arc<dyn CredentialProvider>,
        proxy_url: Option<Url>,
        cache_size: usize,
        bmc_latency_instrumentation: Option<BmcLatencyInstrumentation>,
    ) -> Result<BmcClient, HealthError> {
        BmcClient::new(
            reqwest,
            addr,
            provider,
            proxy_url,
            cache_size,
            TEST_REQUEST_CONCURRENCY,
            bmc_latency_instrumentation,
        )
    }

    fn bmc_status_error(status: http::StatusCode) -> BmcError {
        BmcError::InvalidResponse {
            url: Url::parse("https://127.0.0.1/redfish/v1").expect("valid url"),
            status,
            text: String::new(),
        }
    }

    fn test_client() -> BmcClient {
        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("constructor ok")
    }

    fn dummy_error() -> HealthError {
        HealthError::GenericError("boom".to_string())
    }

    #[test]
    fn non_connection_errors_do_not_trip_the_circuit() {
        // 401/403, 404, and generic errors mean the BMC answered (or the failure
        // is unrelated to reachability) — they must not open the breaker.
        assert!(!is_connection_error(&HealthError::BmcError(Box::new(
            bmc_status_error(http::StatusCode::UNAUTHORIZED)
        ))));
        assert!(!is_connection_error(&HealthError::BmcError(Box::new(
            bmc_status_error(http::StatusCode::NOT_FOUND)
        ))));
        assert!(!is_connection_error(&HealthError::HttpError(
            "HTTP 404".to_string()
        )));
        assert!(!is_connection_error(&dummy_error()));
    }

    #[tokio::test]
    async fn real_connect_failure_is_classified_and_trips_the_circuit() {
        // Reserve an ephemeral port, then release it, so connecting to it is
        // refused — a real, deterministic transport-level failure with no
        // fixed-port collision and no waiting on a timeout. This exercises the
        // whole chain end to end (reqwest error -> BmcError -> HealthError ->
        // is_connection_error -> trip_circuit), guarding the assumption that a
        // genuine connect failure — the production flood was `Connect, TimedOut`
        // — is actually classified as a connection error. See NVBug 6036327.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let addr = BmcAddr {
            ip: "127.0.0.1".parse().expect("loopback ip"),
            port: Some(port),
            mac: MacAddress::from_str("00:11:22:33:44:55").expect("mac"),
        };
        let client =
            Arc::new(test_bmc(reqwest(), addr, provider, None, 10, None).expect("constructor ok"));

        assert_eq!(
            client.collector_sweep(),
            CollectorSweep::Full,
            "circuit starts closed"
        );

        // Any real Redfish read against the closed port fails to connect.
        let result = nv_redfish::ServiceRoot::new(client.clone()).await;
        assert!(result.is_err(), "connecting to a closed port must fail");

        assert_eq!(
            client.collector_sweep(),
            CollectorSweep::Skip,
            "a genuine connect failure must be classified as a connection error and open the breaker"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_concurrency_limits_parallel_transport_attempts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");

        let proxy_url = Url::parse(&format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        ))
        .expect("test proxy URL");

        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );

        let client = Arc::new(
            BmcClient::new(
                reqwest_with_timeout(Duration::from_millis(200)),
                test_addr(),
                provider,
                Some(proxy_url),
                10,
                NonZeroUsize::MIN,
                None,
            )
            .expect("constructor succeeds"),
        );

        client.ensure_credentials().await.expect("credentials load");

        let first_client = Arc::clone(&client);

        let first = tokio::spawn(async move {
            first_client
                .inner
                .get::<nv_redfish::schema::service_root::ServiceRoot>(&ODataId::from(
                    "/redfish/v1/".to_string(),
                ))
                .await
        });

        let (_first_connection, _) =
            tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .expect("first request reaches transport")
                .expect("accept first request");

        let second_client = Arc::clone(&client);

        let second = tokio::spawn(async move {
            second_client
                .inner
                .get::<nv_redfish::schema::service_root::ServiceRoot>(&ODataId::from(
                    "/redfish/v1/".to_string(),
                ))
                .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "second request must wait while the first holds the only permit"
        );

        assert!(first.await.expect("first task joins").is_err());

        let (_second_connection, _) =
            tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .expect("second request reaches transport after permit release")
                .expect("accept second request");

        assert!(second.await.expect("second task joins").is_err());
    }

    #[test]
    fn circuit_opens_on_failure_then_closes_on_success() {
        let client = test_client();

        // Starts closed: requests flow.
        assert_eq!(client.collector_sweep(), CollectorSweep::Full);
        assert!(client.check_circuit().is_ok());

        // A connect-level failure opens the circuit.
        client.trip_circuit(&dummy_error());
        assert_eq!(client.collector_sweep(), CollectorSweep::Skip);
        assert!(
            client.check_circuit().is_err(),
            "open circuit must fast-fail"
        );

        // A success closes it again.
        client.note_reachable();
        assert_eq!(client.collector_sweep(), CollectorSweep::Full);
        assert!(client.check_circuit().is_ok());
    }

    #[tokio::test]
    async fn non_connection_error_during_probe_closes_circuit() {
        let client = test_client();

        // An open window that has elapsed: the next caller through `guarded`
        // becomes the half-open probe.
        client.set_circuit_for_test(CircuitState::Open {
            until: Instant::now() - Duration::from_secs(1),
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });

        // The probe reaches the BMC and gets a real (non-connection) error.
        let result: Result<(), HealthError> = client
            .guarded(async {
                Err(HealthError::BmcError(Box::new(bmc_status_error(
                    http::StatusCode::NOT_FOUND,
                ))))
            })
            .await;
        assert!(result.is_err());

        assert_eq!(
            client.collector_sweep(),
            CollectorSweep::Full,
            "a non-connection response proves reachability and must close the circuit, \
             not leave it half-open until the probe deadline"
        );
    }

    #[test]
    fn collector_sweep_probes_once_window_elapses() {
        let client = test_client();
        client.set_circuit_for_test(CircuitState::Open {
            until: Instant::now() - Duration::from_secs(1),
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });
        assert_eq!(
            client.collector_sweep(),
            CollectorSweep::Probe,
            "an elapsed backoff window should admit a single probe, not a full sweep"
        );
    }

    #[test]
    fn fast_path_hint_tracks_circuit_state() {
        let client = test_client();

        // Closed: hint clear.
        assert!(!client.circuit_tripped.load(Ordering::Acquire));

        // Tripped: hint set so the lock-free fast path consults the lock.
        client.trip_circuit(&dummy_error());
        assert!(client.circuit_tripped.load(Ordering::Acquire));

        // Reachable again: hint cleared so the fast path stays lock-free.
        client.note_reachable();
        assert!(!client.circuit_tripped.load(Ordering::Acquire));

        // Promoting an expired Open to a half-open probe keeps the hint set
        // (still non-closed).
        client.set_circuit_for_test(CircuitState::Open {
            until: Instant::now() - Duration::from_secs(1),
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });
        assert!(client.check_circuit().is_ok());
        assert!(client.circuit_tripped.load(Ordering::Acquire));
    }

    #[test]
    fn expired_open_circuit_admits_exactly_one_probe() {
        let client = test_client();

        // Simulate an open window that has already elapsed.
        client.set_circuit_for_test(CircuitState::Open {
            until: Instant::now() - Duration::from_secs(1),
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });

        // The first caller is let through as the probe...
        assert!(client.check_circuit().is_ok(), "probe should be admitted");
        assert!(
            matches!(
                *client.circuit.lock().unwrap(),
                CircuitState::Probing { .. }
            ),
            "circuit should be half-open after admitting a probe"
        );
        // ...and everyone else keeps fast-failing while the probe is in flight.
        assert!(client.check_circuit().is_err());
        assert_eq!(client.collector_sweep(), CollectorSweep::Skip);
    }

    #[test]
    fn failed_probe_escalates_backoff() {
        let client = test_client();

        client.set_circuit_for_test(CircuitState::Probing {
            deadline: Instant::now() + CIRCUIT_PROBE_TIMEOUT,
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });

        client.trip_circuit(&dummy_error());

        match *client.circuit.lock().unwrap() {
            CircuitState::Open { backoff, .. } => assert_eq!(
                backoff,
                (CIRCUIT_INITIAL_BACKOFF * 2).min(CIRCUIT_MAX_BACKOFF),
                "a failed probe must double the backoff window"
            ),
            ref other => panic!("expected Open after failed probe, got {other:?}"),
        }
    }

    #[test]
    fn stale_failure_while_open_does_not_extend_backoff() {
        let client = test_client();

        client.set_circuit_for_test(CircuitState::Open {
            until: Instant::now() + CIRCUIT_INITIAL_BACKOFF,
            backoff: CIRCUIT_INITIAL_BACKOFF,
        });

        // A request that was already in flight when the circuit opened fails. It
        // must not push the backoff window out further.
        client.trip_circuit(&dummy_error());

        match *client.circuit.lock().unwrap() {
            CircuitState::Open { backoff, .. } => {
                assert_eq!(
                    backoff, CIRCUIT_INITIAL_BACKOFF,
                    "backoff must be unchanged"
                )
            }
            ref other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn detects_auth_bmc_errors() {
        assert!(is_auth_bmc_error(&bmc_status_error(
            http::StatusCode::UNAUTHORIZED
        )));
        assert!(is_auth_bmc_error(&bmc_status_error(
            http::StatusCode::FORBIDDEN
        )));
        assert!(!is_auth_bmc_error(&bmc_status_error(
            http::StatusCode::NOT_FOUND
        )));
    }

    #[test]
    fn detects_auth_health_errors() {
        assert!(is_auth_error(&HealthError::BmcError(Box::new(
            bmc_status_error(http::StatusCode::UNAUTHORIZED),
        ))));
        assert!(is_auth_error(&HealthError::HttpError(
            "request failed with HTTP 403".to_string(),
        )));
        assert!(!is_auth_error(&HealthError::HttpError(
            "request failed with HTTP 404".to_string(),
        )));
    }

    #[tokio::test]
    async fn new_does_not_fetch_credentials_eagerly() {
        let (provider, calls) = CountingProvider::new(
            BmcCredentials::UsernamePassword {
                username: "u".to_string(),
                password: Some("p".to_string()),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None)
            .expect("constructor succeeds");

        assert_eq!(
            calls.load(AtomicOrdering::SeqCst),
            0,
            "construction must not call the credential provider"
        );
        assert_eq!(
            client.credential_generation.load(Ordering::Acquire),
            0,
            "generation stays 0 until first successful fetch"
        );
    }

    #[tokio::test]
    async fn ensure_credentials_calls_provider_exactly_once_under_concurrency() {
        let (provider, calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            Some(Duration::from_millis(50)),
        );
        let client =
            Arc::new(test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok"));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let client = client.clone();
            handles.push(tokio::spawn(
                async move { client.ensure_credentials().await },
            ));
        }
        for h in handles {
            h.await.expect("task").expect("ensure ok");
        }

        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn failed_initial_fetch_is_not_retried_until_the_cooldown_elapses() {
        // A failed `get_or_try_init` leaves the cell empty, so without the
        // cooldown every collector read re-drives the provider: one endpoint
        // whose credentials Core refused to issue cost hundreds of failed
        // GetBmcCredentials RPCs per minute.
        let provider = Arc::new(FlakyProvider {
            attempts: AtomicUsize::new(0),
        });
        let client = Arc::new(
            test_bmc(reqwest(), test_addr(), provider.clone(), None, 10, None)
                .expect("constructor succeeds"),
        );

        // A concurrent fan-out arriving while the provider fails costs one call.
        let mut handles = Vec::new();
        for _ in 0..16 {
            let client = client.clone();
            handles.push(tokio::spawn(
                async move { client.ensure_credentials().await },
            ));
        }
        for handle in handles {
            handle
                .await
                .expect("task")
                .expect_err("the provider failure surfaces to every caller");
        }
        assert_eq!(provider.attempts.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 0);

        // A later read inside the cooldown fast-fails and still names the cause.
        let error = client
            .ensure_credentials()
            .await
            .expect_err("still inside the cooldown");
        assert!(
            error.to_string().contains("transient"),
            "the fast-fail error must carry the provider's error: {error}"
        );
        assert_eq!(provider.attempts.load(AtomicOrdering::SeqCst), 1);

        // Once the cooldown elapses one caller gets to try the provider again.
        client.set_credential_fetch_failure_for_test(expired_fetch_failure());
        client
            .ensure_credentials()
            .await
            .expect("the retry after the cooldown succeeds");
        assert_eq!(provider.attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn successful_fetch_clears_the_failure_record() {
        // The admitted retry restarts the window before it runs. If its success
        // left the record in place, a 401 on the freshly fetched credentials
        // would find the refresh suppressed and go unreplayed.
        let provider = Arc::new(FlakyProvider {
            attempts: AtomicUsize::new(0),
        });
        let client = test_bmc(reqwest(), test_addr(), provider.clone(), None, 10, None)
            .expect("constructor succeeds");
        client
            .ensure_credentials()
            .await
            .expect_err("the first fetch fails");
        client.set_credential_fetch_failure_for_test(expired_fetch_failure());

        let (op, attempts) = scripted_op(vec![Err(auth_error()), Ok("body")]);
        let value = client
            .read_with_auth_retry(op)
            .await
            .expect("a refresh right after a recovered fetch must run");

        assert_eq!(value, "body");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider.attempts.load(AtomicOrdering::SeqCst),
            3,
            "failed fetch, recovered fetch, refresh"
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_collapses_to_a_single_provider_call() {
        let (provider, calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            Some(Duration::from_millis(50)),
        );
        let client =
            Arc::new(test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok"));
        client.ensure_credentials().await.expect("init ok");
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        let observed = client.credential_generation.load(Ordering::Acquire);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                client
                    .refresh_credentials(
                        &HealthError::HttpError("HTTP 401".to_string()),
                        Some(observed),
                    )
                    .await
            }));
        }
        for h in handles {
            h.await.expect("task").expect("refresh ok");
        }

        // One init fetch + exactly one refresh fetch.
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn refresh_consumes_provider_and_bumps_generation() {
        struct SequenceProvider {
            tokens: StdMutex<Vec<&'static str>>,
            handed_out: StdMutex<Vec<&'static str>>,
            calls: Arc<AtomicUsize>,
        }

        impl CredentialProvider for SequenceProvider {
            fn fetch_credentials<'a>(
                &'a self,
                _endpoint: &'a BmcAddr,
            ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
                self.calls.fetch_add(1, AtomicOrdering::SeqCst);
                let token = self
                    .tokens
                    .lock()
                    .unwrap()
                    .pop()
                    .expect("token sequence exhausted");
                self.handed_out.lock().unwrap().push(token);
                Box::pin(async move {
                    Ok(BmcCredentials::SessionToken {
                        token: token.to_string(),
                    })
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(SequenceProvider {
            tokens: StdMutex::new(vec!["second", "first"]),
            handed_out: StdMutex::new(Vec::new()),
            calls: calls.clone(),
        });
        let client = test_bmc(reqwest(), test_addr(), provider.clone(), None, 10, None)
            .expect("constructor ok");

        client.ensure_credentials().await.expect("init ok");
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 1);

        client
            .refresh_credentials(&HealthError::HttpError("HTTP 401".to_string()), None)
            .await
            .expect("refresh ok");

        assert_eq!(client.credential_generation.load(Ordering::Acquire), 2);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider.handed_out.lock().unwrap().as_slice(),
            &["first", "second"],
            "init must consume the first token, refresh the second"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_credentials_respects_timeout() {
        struct HangingProvider;

        impl CredentialProvider for HangingProvider {
            fn fetch_credentials<'a>(
                &'a self,
                _endpoint: &'a BmcAddr,
            ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
                Box::pin(std::future::pending())
            }
        }

        let client = Arc::new(
            test_bmc(
                reqwest(),
                test_addr(),
                Arc::new(HangingProvider),
                None,
                10,
                None,
            )
            .expect("constructor ok"),
        );
        let refresh_client = client.clone();
        let refresh = tokio::spawn(async move {
            refresh_client
                .refresh_credentials(&HealthError::HttpError("HTTP 401".to_string()), None)
                .await
        });

        // Sleep just past the timeout so the timer fires; tokio's paused
        // clock auto-advances via tokio::time::advance.
        tokio::time::advance(CREDENTIAL_REFRESH_TIMEOUT + Duration::from_secs(1)).await;
        let result = refresh.await.expect("task joined");
        assert!(result.is_err(), "hanging provider must surface as timeout");
    }

    #[tokio::test(start_paused = true)]
    async fn ensure_credentials_respects_timeout() {
        struct HangingProvider;

        impl CredentialProvider for HangingProvider {
            fn fetch_credentials<'a>(
                &'a self,
                _endpoint: &'a BmcAddr,
            ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
                Box::pin(std::future::pending())
            }
        }

        let client = Arc::new(
            test_bmc(
                reqwest(),
                test_addr(),
                Arc::new(HangingProvider),
                None,
                10,
                None,
            )
            .expect("constructor ok"),
        );
        let ensure_client = client.clone();
        let ensure = tokio::spawn(async move { ensure_client.ensure_credentials().await });

        tokio::time::advance(CREDENTIAL_REFRESH_TIMEOUT + Duration::from_secs(1)).await;
        let result = ensure.await.expect("task joined");
        let error = result.expect_err("hanging provider must surface as timeout");
        match error {
            HealthError::GenericError(msg) => assert!(
                msg.contains("timed out") && msg.contains("initial BMC credentials"),
                "expected timeout message, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }

        // OnceCell must not have latched the failure — a subsequent call
        // with a working provider has to be able to succeed.
        let (recovery_provider, recovery_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let recovered = test_bmc(reqwest(), test_addr(), recovery_provider, None, 10, None)
            .expect("constructor ok");
        recovered.ensure_credentials().await.expect("recovery ok");
        assert_eq!(recovery_calls.load(AtomicOrdering::SeqCst), 1);
    }

    /// Records how many times the operation ran and replays a scripted outcome
    /// per attempt, so a test can assert both the result and the attempt count.
    fn scripted_op(
        outcomes: Vec<Result<&'static str, HealthError>>,
    ) -> (
        impl Fn() -> std::future::Ready<Result<&'static str, HealthError>>,
        Arc<AtomicUsize>,
    ) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let outcomes = Arc::new(StdMutex::new(outcomes.into_iter()));
        let op = move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            let outcome = outcomes
                .lock()
                .unwrap()
                .next()
                .expect("operation ran more times than the test scripted");
            std::future::ready(outcome)
        };
        (op, attempts)
    }

    fn auth_error() -> HealthError {
        HealthError::BmcError(Box::new(bmc_status_error(http::StatusCode::UNAUTHORIZED)))
    }

    fn forbidden_error() -> HealthError {
        HealthError::BmcError(Box::new(bmc_status_error(http::StatusCode::FORBIDDEN)))
    }

    #[tokio::test]
    async fn read_retries_once_after_refreshing_on_auth_error() {
        // NVBug 6506008: a 401 that only reflects rotated credentials must not
        // surface to the collector — otherwise the resource's series drop out of
        // the interval and reappear on the next one.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, attempts) = scripted_op(vec![Err(auth_error()), Ok("body")]);

        let value = client
            .read_with_auth_retry(op)
            .await
            .expect("retry with refreshed credentials succeeds");

        assert_eq!(value, "body");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2, "one retry");
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "initial fetch plus one refresh"
        );
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn read_does_not_retry_non_auth_errors() {
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, attempts) = scripted_op(vec![Err(HealthError::HttpError(
            "request failed with HTTP 404".to_string(),
        ))]);

        client
            .read_with_auth_retry(op)
            .await
            .expect_err("404 must surface");

        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 1, "no retry");
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            1,
            "no credential refresh for a non-auth failure"
        );
    }

    #[tokio::test]
    async fn read_retries_at_most_once() {
        // Genuinely wrong credentials must not turn every read into a request
        // storm against a BMC that is already refusing us.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, attempts) = scripted_op(vec![Err(auth_error()), Err(auth_error())]);

        client
            .read_with_auth_retry(op)
            .await
            .expect_err("second auth failure surfaces");

        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "exactly one refresh, no refresh after the retry"
        );
    }

    #[tokio::test]
    async fn read_surfaces_original_error_when_refresh_fails() {
        let provider = Arc::new(InitOnceThenFailingProvider {
            calls: AtomicUsize::new(0),
        });
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, attempts) = scripted_op(vec![Err(auth_error())]);

        let error = client
            .read_with_auth_retry(op)
            .await
            .expect_err("unrefreshable auth failure surfaces");

        assert!(
            is_auth_error(&error),
            "the original 401 must surface, not the refresh failure: {error:?}"
        );
        assert_eq!(
            attempts.load(AtomicOrdering::SeqCst),
            1,
            "no retry when the refresh could not produce new credentials"
        );
    }

    #[tokio::test]
    async fn failed_refresh_is_not_retried_until_the_cooldown_elapses() {
        // A failed refresh leaves the generation unchanged, so without the
        // cooldown every 401 pays a provider call: the same fan-out as the
        // initial fetch once an endpoint's credentials stop being issuable.
        let provider = Arc::new(InitOnceThenFailingProvider {
            calls: AtomicUsize::new(0),
        });
        let client =
            test_bmc(reqwest(), test_addr(), provider.clone(), None, 10, None).expect("ok");

        for read in 0..3 {
            let (op, attempts) = scripted_op(vec![Err(auth_error())]);
            let error = client
                .read_with_auth_retry(op)
                .await
                .expect_err("the refresh cannot produce credentials");
            assert!(
                is_auth_error(&error),
                "read {read} surfaces the original 401: {error:?}"
            );
            assert_eq!(
                attempts.load(AtomicOrdering::SeqCst),
                1,
                "read {read} must not be replayed"
            );
        }
        assert_eq!(
            provider.calls.load(AtomicOrdering::SeqCst),
            2,
            "the initial fetch plus the one refresh that failed"
        );

        client.set_credential_fetch_failure_for_test(expired_fetch_failure());
        let (op, _) = scripted_op(vec![Err(auth_error())]);
        client
            .read_with_auth_retry(op)
            .await
            .expect_err("the provider is still failing");
        assert_eq!(
            provider.calls.load(AtomicOrdering::SeqCst),
            3,
            "the elapsed cooldown admits one refresh"
        );
    }

    #[tokio::test]
    async fn read_retries_when_a_concurrent_caller_already_refreshed() {
        // `refresh_credentials` is a no-op when another caller has already
        // rotated past the observed generation. That still means the credentials
        // now in place are newer than the rejected ones, so the retry must run.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        client.ensure_credentials().await.expect("init ok");

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let value = client
            .read_with_auth_retry(|| {
                let attempt = counter.fetch_add(1, AtomicOrdering::SeqCst);
                if attempt == 0 {
                    // Stand in for a concurrent caller finishing its own refresh
                    // between our generation read and our refresh attempt.
                    client.credential_generation.fetch_add(1, Ordering::AcqRel);
                }
                std::future::ready(if attempt == 0 {
                    Err(auth_error())
                } else {
                    Ok("body")
                })
            })
            .await
            .expect("retry runs against the concurrently refreshed credentials");

        assert_eq!(value, "body");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            1,
            "the no-op refresh must not re-fetch"
        );
        assert_eq!(client.credential_generation.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn refused_credentials_suppress_further_refresh_and_replay() {
        // Auth failures never open the connection circuit, so without this a
        // misconfigured endpoint pays a refresh and a replay on every read of
        // every sweep. Reviewer's arithmetic on the PR: 300 reads cost ~600
        // requests and ~300 provider fetches; suppression takes that to ~301
        // requests and one fetch.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");

        let (first, first_attempts) = scripted_op(vec![Err(auth_error()), Err(auth_error())]);
        client
            .read_with_auth_retry(first)
            .await
            .expect_err("credentials are genuinely wrong");
        assert_eq!(first_attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "initial fetch plus the one refresh that proved them wrong"
        );

        // Every subsequent read observes the generation just proven bad.
        for read in 0..5 {
            let (op, attempts) = scripted_op(vec![Err(auth_error())]);
            client
                .read_with_auth_retry(op)
                .await
                .expect_err("still refused");
            assert_eq!(
                attempts.load(AtomicOrdering::SeqCst),
                1,
                "read {read} must not be replayed"
            );
        }
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "no further credential fetches while the generation is known bad"
        );
    }

    #[tokio::test]
    async fn known_bad_credentials_are_retried_once_the_cooldown_elapses() {
        // Credentials repaired out of band have to be picked up, so the record
        // must expire rather than latch.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        client.ensure_credentials().await.expect("init ok");

        let generation = client.credential_generation.load(Ordering::Acquire);
        client.set_known_bad_credentials_for_test(Some(KnownBadCredentials {
            generation,
            proven_at: Instant::now() - KNOWN_BAD_CREDENTIAL_COOLDOWN - Duration::from_secs(1),
        }));

        let (op, attempts) = scripted_op(vec![Err(auth_error()), Ok("body")]);
        let value = client
            .read_with_auth_retry(op)
            .await
            .expect("the elapsed cooldown must admit a refresh and replay");

        assert_eq!(value, "body");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "init fetch plus the refresh the elapsed cooldown allowed"
        );
    }

    #[tokio::test]
    async fn a_known_bad_record_does_not_suppress_a_newer_generation() {
        // The record names one generation. Credentials rotated since then are
        // untested, so they must not inherit its verdict.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        client.ensure_credentials().await.expect("init ok");

        let stale_generation = client.credential_generation.load(Ordering::Acquire) - 1;
        client.set_known_bad_credentials_for_test(Some(KnownBadCredentials {
            generation: stale_generation,
            proven_at: Instant::now(),
        }));

        let (op, attempts) = scripted_op(vec![Err(auth_error()), Ok("body")]);
        client
            .read_with_auth_retry(op)
            .await
            .expect("a generation with no verdict must still be retried");

        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(provider_calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_condemned_view_still_replays_against_untested_credentials() {
        // The mirror of the case below: this attempt observed a generation that
        // was condemned, but a concurrent caller has since installed untested
        // credentials. Suppressing here would drop the resource for the
        // interval — exactly the failure this branch exists to fix.
        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        client.ensure_credentials().await.expect("init ok");

        let observed = client.credential_generation.load(Ordering::Acquire);
        client.note_credentials_refused(observed);

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let value = client
            .read_with_auth_retry(|| {
                let attempt = counter.fetch_add(1, AtomicOrdering::SeqCst);
                if attempt == 0 {
                    // A concurrent caller installs credentials nobody has tested.
                    client.credential_generation.fetch_add(1, Ordering::AcqRel);
                }
                std::future::ready(if attempt == 0 {
                    Err(auth_error())
                } else {
                    Ok("body")
                })
            })
            .await
            .expect("untested credentials must still get a replay");

        assert_eq!(value, "body");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_caller_holding_a_stale_view_is_suppressed_by_the_current_verdict() {
        // A concurrent sweep has every read observing generation N. The first
        // caller refreshes to N+1 and has that refused. The rest still hold N,
        // but the credentials they would replay against are the condemned N+1,
        // so replaying is pointless however old their view is.
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        client.ensure_credentials().await.expect("init ok");

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        client
            .read_with_auth_retry(|| {
                let attempt = counter.fetch_add(1, AtomicOrdering::SeqCst);
                if attempt == 0 {
                    // Stand in for the concurrent caller that refreshed while
                    // this request was in flight and had the result refused.
                    let refreshed = client.credential_generation.fetch_add(1, Ordering::AcqRel) + 1;
                    client.note_credentials_refused(refreshed);
                }
                std::future::ready(Err::<&str, HealthError>(auth_error()))
            })
            .await
            .expect_err("still refused");

        assert_eq!(
            attempts.load(AtomicOrdering::SeqCst),
            1,
            "a stale view must not buy a replay against condemned credentials"
        );
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            1,
            "and must not spend a credential fetch"
        );
    }

    #[tokio::test]
    async fn a_non_auth_replay_failure_does_not_condemn_the_credentials() {
        // A 500 or a dropped connection on the replay says nothing about the
        // credentials. Condemning them would suppress the refresh for a genuine
        // 401 arriving later in the cooldown.
        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, _) = scripted_op(vec![
            Err(auth_error()),
            Err(HealthError::HttpError("HTTP 500".to_string())),
        ]);

        client
            .read_with_auth_retry(op)
            .await
            .expect_err("the 500 surfaces");

        let generation = client.credential_generation.load(Ordering::Acquire);
        assert!(
            !client.credentials_known_bad(generation),
            "a non-auth replay failure must not mark the generation bad"
        );
    }

    #[tokio::test]
    async fn a_forbidden_resource_does_not_condemn_the_endpoints_credentials() {
        // 403 means this identity may not read this *resource*; the credentials
        // themselves are fine. Condemning them would let one forbidden resource
        // suppress the refresh-and-replay for every other resource on the
        // endpoint — and since it is polled every interval, it would re-condemn
        // them as fast as the cooldown expired, disabling the retry for good.
        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, attempts) = scripted_op(vec![Err(forbidden_error()), Err(forbidden_error())]);

        client
            .read_with_auth_retry(op)
            .await
            .expect_err("the 403 surfaces");

        assert_eq!(
            attempts.load(AtomicOrdering::SeqCst),
            2,
            "a 403 is still replayed once, since a rotation can change the account"
        );
        let generation = client.credential_generation.load(Ordering::Acquire);
        assert!(
            !client.credentials_known_bad(generation),
            "a forbidden resource must not condemn the endpoint's credentials"
        );
    }

    #[tokio::test]
    async fn a_rejected_credential_still_condemns_the_generation() {
        // The counterpart to the 403 case: 401 does generalise, so it must still
        // produce the endpoint-wide verdict the suppression relies on.
        let (provider, _) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        let client = test_bmc(reqwest(), test_addr(), provider, None, 10, None).expect("ok");
        let (op, _) = scripted_op(vec![Err(auth_error()), Err(auth_error())]);

        client
            .read_with_auth_retry(op)
            .await
            .expect_err("the 401 surfaces");

        let generation = client.credential_generation.load(Ordering::Acquire);
        assert!(
            client.credentials_known_bad(generation),
            "a rejected credential must still be recorded"
        );
    }

    #[test]
    fn credential_rejection_is_narrower_than_auth_failure() {
        for error in [
            HealthError::BmcError(Box::new(bmc_status_error(http::StatusCode::UNAUTHORIZED))),
            HealthError::HttpError("request failed with HTTP 401".to_string()),
        ] {
            assert!(is_auth_error(&error), "401 is an auth failure: {error:?}");
            assert!(
                is_credential_rejection(&error),
                "401 rejects the credentials: {error:?}"
            );
        }

        for error in [
            HealthError::BmcError(Box::new(bmc_status_error(http::StatusCode::FORBIDDEN))),
            HealthError::HttpError("request failed with HTTP 403".to_string()),
        ] {
            assert!(is_auth_error(&error), "403 is an auth failure: {error:?}");
            assert!(
                !is_credential_rejection(&error),
                "403 does not reject the credentials: {error:?}"
            );
        }

        let other = HealthError::HttpError("request failed with HTTP 404".to_string());
        assert!(!is_auth_error(&other));
        assert!(!is_credential_rejection(&other));
    }

    #[test]
    fn a_stale_verdict_does_not_displace_a_newer_one() {
        // A slow replay can land after a concurrent caller already refreshed and
        // condemned a later generation; the late verdict is stale.
        let client = test_client();
        client.note_credentials_refused(7);
        client.note_credentials_refused(5);

        assert!(
            client.credentials_known_bad(7),
            "the newer verdict must survive a late arrival from generation 5"
        );
        assert!(
            !client.credentials_known_bad(5),
            "the stale generation must not have been recorded"
        );
    }

    #[test]
    fn an_elapsed_cooldown_admits_exactly_one_revalidation() {
        // Clearing the record on expiry would let every caller queued behind the
        // mutex during a sweep through at once.
        let client = test_client();
        client.set_known_bad_credentials_for_test(Some(KnownBadCredentials {
            generation: 3,
            proven_at: Instant::now() - KNOWN_BAD_CREDENTIAL_COOLDOWN - Duration::from_secs(1),
        }));

        assert!(
            !client.credentials_known_bad(3),
            "the first caller after the cooldown revalidates"
        );
        assert!(
            client.credentials_known_bad(3),
            "the next caller must stay suppressed, not join a fan-out"
        );
    }

    const ENTITY_BODY: &str = r#"{"@odata.id":"/redfish/v1"}"#;
    const SERVICE_ROOT_BODY: &str = r##"{
        "@odata.id": "/redfish/v1",
        "@odata.type": "#ServiceRoot.v1_15_0.ServiceRoot",
        "Id": "RootService",
        "Name": "Root Service",
        "Links": {
            "Sessions": {
                "@odata.id": "/redfish/v1/SessionService/Sessions"
            }
        },
        "Product": "GB200 BMC",
        "RedfishVersion": "1.15.0",
        "Vendor": "NVIDIA"
    }"##;

    /// Minimal Redfish entity, so the wire-level tests below can drive the real
    /// `get`/`delete` implementations without depending on the shape of any
    /// particular generated schema type.
    #[derive(Debug, Deserialize)]
    struct TestEntity {
        #[serde(rename = "@odata.id")]
        odata_id: ODataId,
    }

    impl EntityTypeRef for TestEntity {
        fn odata_id(&self) -> &ODataId {
            &self.odata_id
        }

        fn etag(&self) -> Option<&ODataETag> {
            None
        }
    }

    /// Serve a scripted sequence of raw HTTP responses on an ephemeral port,
    /// one per connection, counting the requests actually received.
    ///
    /// Each response closes its connection so the client opens a fresh one per
    /// request, making the count an exact measure of how many times the caller
    /// hit the wire. Follows the local-server pattern used by the NVUE REST
    /// collector tests.
    fn spawn_scripted_http_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (Url, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("test server binds local port");
        let addr = listener.local_addr().expect("test server local addr");
        let base_url = Url::parse(&format!("http://{addr}")).expect("test server url parses");

        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0_u8; 4096];
                if stream.read(&mut buffer).is_err() {
                    return;
                }
                counter.fetch_add(1, AtomicOrdering::SeqCst);

                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    503 => "Service Unavailable",
                    _ => panic!("unsupported test response status {status}"),
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (base_url, requests, handle)
    }

    fn client_against(base_url: Url) -> (BmcClient, Arc<AtomicUsize>) {
        client_against_with_metrics(base_url, None)
    }

    fn client_against_with_metrics(
        base_url: Url,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> (BmcClient, Arc<AtomicUsize>) {
        let (provider, provider_calls) = CountingProvider::new(
            BmcCredentials::SessionToken {
                token: "t".to_string(),
            },
            None,
        );
        // `bmc_url` returns a proxy URL verbatim, so this points the client at
        // the local server over plain HTTP without needing TLS.
        let bmc_latency_instrumentation = bmc_latency_metrics.map(|metrics| {
            BmcLatencyInstrumentation::new(
                metrics,
                BmcLatencyEndpointLabels::new(
                    Some("fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg".to_string()),
                    Some("rack-1".to_string()),
                ),
            )
        });
        let client = test_bmc(
            reqwest(),
            test_addr(),
            provider,
            Some(base_url),
            10,
            bmc_latency_instrumentation,
        )
        .expect("constructor ok");
        (client, provider_calls)
    }

    fn bmc_latency_metrics(prefix: &str) -> (Registry, Arc<BmcLatencyMetrics>) {
        bmc_latency_metrics_with_attributes(prefix, &BmcLatencyAttribute::ATTRIBUTES)
    }

    fn bmc_latency_metrics_with_attributes(
        prefix: &str,
        attributes: &[BmcLatencyAttribute],
    ) -> (Registry, Arc<BmcLatencyMetrics>) {
        let registry = Registry::new();
        let metrics = Arc::new(
            BmcLatencyMetrics::new_with_attributes(&registry, prefix, attributes)
                .expect("BMC latency metrics register"),
        );
        (registry, metrics)
    }

    fn render_metrics(registry: &Registry) -> String {
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder
            .encode(&registry.gather(), &mut buffer)
            .expect("metrics encode");
        String::from_utf8(buffer).expect("prometheus text is UTF-8")
    }

    fn assert_bmc_latency_series(metrics: &str, labels: &[&str]) {
        assert!(
            metrics.lines().any(|line| {
                line.starts_with("test_health_bmc_latency_ms_bucket")
                    && labels.iter().all(|label| line.contains(label))
            }),
            "missing BMC latency series with labels {labels:?}; metrics:
{metrics}",
        );
    }

    #[tokio::test]
    async fn bmc_latency_metric_records_status_method_path_and_identity() {
        let (registry, metrics) = bmc_latency_metrics("test_health");
        let (base_url, requests, server) =
            spawn_scripted_http_server(vec![(200, SERVICE_ROOT_BODY), (200, SERVICE_ROOT_BODY)]);
        let (client, _provider_calls) = client_against_with_metrics(base_url, Some(metrics));

        let root = client
            .get::<nv_redfish::schema::service_root::ServiceRoot>(&ODataId::service_root())
            .await
            .expect("service root response decodes");
        assert_eq!(
            root.vendor.as_ref().and_then(Option::as_deref),
            Some("NVIDIA")
        );

        // The HTTP wrapper observes before the BmcClient sees the decoded
        // ServiceRoot, so the first ServiceRoot request seeds identity and the
        // next upstream request carries it as metric labels.
        client
            .get::<nv_redfish::schema::service_root::ServiceRoot>(&ODataId::service_root())
            .await
            .expect("second service root response decodes");
        assert_eq!(requests.load(AtomicOrdering::SeqCst), 2);

        let metrics = render_metrics(&registry);
        assert!(metrics.contains("# HELP test_health_bmc_latency_ms"));
        assert_bmc_latency_series(
            &metrics,
            &[
                "http_response_status_code=\"200\"",
                "http_request_method=\"GET\"",
                "http_path=\"/redfish/v1\"",
                "server_address=\"10.0.0.1\"",
                "url_scheme=\"https\"",
                "bmc_vendor=\"NVIDIA\"",
                "bmc_model=\"GB200 BMC\"",
                "entity_type=\"ServiceRoot\"",
                "machine_id=\"fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg\"",
                "rack_id=\"rack-1\"",
            ],
        );
        server.join().expect("test server thread");
    }

    #[tokio::test]
    async fn bmc_latency_metric_records_http_error_status() {
        let (registry, metrics) = bmc_latency_metrics("test_health");
        let (base_url, _requests, server) = spawn_scripted_http_server(vec![(503, "{}")]);
        let (client, _provider_calls) = client_against_with_metrics(base_url, Some(metrics));

        client
            .get::<TestEntity>(&ODataId::service_root())
            .await
            .expect_err("503 must surface to the caller");

        let metrics = render_metrics(&registry);
        assert_bmc_latency_series(
            &metrics,
            &[
                "http_response_status_code=\"503\"",
                "http_request_method=\"GET\"",
                "http_path=\"/redfish/v1\"",
                "server_address=\"10.0.0.1\"",
                "url_scheme=\"https\"",
                "bmc_vendor=\"unknown\"",
                "bmc_model=\"unknown\"",
                "entity_type=\"TestEntity\"",
                "machine_id=\"fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg\"",
                "rack_id=\"rack-1\"",
            ],
        );
        server.join().expect("test server thread");
    }

    #[tokio::test]
    async fn bmc_latency_metric_uses_configured_attributes_only() {
        let (registry, metrics) = bmc_latency_metrics_with_attributes(
            "test_health",
            &[
                BmcLatencyAttribute::HttpResponseStatusCode,
                BmcLatencyAttribute::ServerAddress,
                BmcLatencyAttribute::UrlScheme,
            ],
        );
        let (base_url, _requests, server) =
            spawn_scripted_http_server(vec![(200, SERVICE_ROOT_BODY)]);
        let (client, _provider_calls) = client_against_with_metrics(base_url, Some(metrics));

        client
            .get::<nv_redfish::schema::service_root::ServiceRoot>(&ODataId::service_root())
            .await
            .expect("service root response decodes");

        let metrics = render_metrics(&registry);
        assert_bmc_latency_series(
            &metrics,
            &[
                "http_response_status_code=\"200\"",
                "server_address=\"10.0.0.1\"",
                "url_scheme=\"https\"",
            ],
        );
        assert!(!metrics.contains("http_request_method="));
        assert!(!metrics.contains("http_path="));
        assert!(!metrics.contains("bmc_vendor="));
        assert!(!metrics.contains("bmc_model="));
        assert!(!metrics.contains("entity_type="));
        assert!(!metrics.contains("machine_id="));
        assert!(!metrics.contains("rack_id="));
        server.join().expect("test server thread");
    }

    #[tokio::test]
    async fn public_read_replays_over_the_wire_after_a_401() {
        // The helper-level tests above pin the retry logic; this one pins the
        // wiring, end to end over real HTTP. Without it, reverting `get` to the
        // old non-retrying body would leave every other test in this file
        // passing. See NVBug 6506008.
        let (base_url, requests, server) =
            spawn_scripted_http_server(vec![(401, "{}"), (200, ENTITY_BODY)]);
        let (client, provider_calls) = client_against(base_url);

        let entity = client
            .get::<TestEntity>(&ODataId::service_root())
            .await
            .map(|entity| entity.odata_id().to_string());

        assert_eq!(
            entity.as_deref().ok(),
            Some("/redfish/v1"),
            "the read must succeed on the replay after the refresh: {entity:?}"
        );
        assert_eq!(
            requests.load(AtomicOrdering::SeqCst),
            2,
            "the 401 must be replayed on the wire, not just refreshed"
        );
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            2,
            "initial fetch plus one refresh"
        );
        server.join().expect("test server thread");
    }

    #[tokio::test]
    async fn public_mutating_call_does_not_replay_after_a_401() {
        // The read-only boundary is a safety property, not just a comment: a
        // replayed write could apply twice.
        let (base_url, requests, server) = spawn_scripted_http_server(vec![(401, "{}")]);
        let (client, provider_calls) = client_against(base_url);

        let result = client.delete::<TestEntity>(&ODataId::service_root()).await;

        assert!(result.is_err(), "the 401 must surface to the caller");
        assert_eq!(
            requests.load(AtomicOrdering::SeqCst),
            1,
            "a mutating call must never be replayed"
        );
        assert_eq!(
            provider_calls.load(AtomicOrdering::SeqCst),
            1,
            "mutating calls do not run the refresh-and-retry path at all"
        );
        server.join().expect("test server thread");
    }
}
