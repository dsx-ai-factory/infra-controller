/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Authenticated `GET /firmware/nic/{digest}/{role}?operation_id=...` serves only
//! the current card's frozen image or optional configuration. `digest` is exactly
//! 64 hexadecimal SHA-256 characters; roles are `image` and `configuration`.
//! The sole query field is a URL-encoded operation ID containing 1..=256 printable
//! nonspace ASCII bytes, with at most 1024 encoded query bytes.
//!
//! The shared listener must supply its verified certificate/JWT `AuthContext`.
//! Its general authorizers pass this route through; this handler requires the
//! exact authenticated stable or predicted host's validated current operation.
//! `UpdatingFirmware` permits a missing receipt or correlated Accepted, Running,
//! or CancelRequested receipt. `ObservingSupervisor` permits only those live
//! correlated receipts, so recovery can finish staging before reconciliation.
//! Other phases, settled receipts, mismatched operation IDs, non-current targets,
//! unknown roles, and unrelated digests return 404.
//!
//! Success returns 200 with exact `Content-Length`, `application/octet-stream`,
//! and verified bytes; no redirect or source URL is returned. Missing machine
//! identity returns 401, malformed/non-host identity 403, and invalid digest/query
//! 400. HEAD returns 405 with `Allow: GET` and performs no artifact work.
//! Pending work returns 202; exhausted admission/storage, unavailable state,
//! snapshot expiry or shutdown return 503; transfer/I/O failure returns 502.
//! Every 202/502/503 includes `Retry-After: 5`. Failures are reported once; a
//! later authorized request may explicitly retry. Transfers belong to the cache's
//! supervised lifecycle, while each response body owns its reader/admission.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use carbide_firmware::{
    ArtifactDigest, CacheError, CacheLookup, CachedArtifact, FirmwareArtifactCache,
};
use carbide_libmlx_model::firmware::operation::FirmwareOperationState;
use carbide_uuid::machine::HostMachineId;
use db::DbSessionGuard;
use model::machine::ManagedHostState;
use model::machine::nic_firmware::NicFirmwarePhase;
use model::nic_firmware::NicFirmwareArtifact;
use sqlx::PgPool;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthContext;

// Bound DB work and open response streams together, with no admission queue.
// The cache independently bounds physical readers and background downloads.
const MAX_REQUESTS: usize = 32;
const RETRY_AFTER_SECONDS: &str = "5";
// Match power-details' finite read-only snapshot budget. It covers acquisition,
// BEGIN, snapshot setup, both reads and COMMIT together, with no retry loop.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct ArtifactState {
    pool: PgPool,
    cache: FirmwareArtifactCache,
    requests: Arc<Semaphore>,
    cancel: CancellationToken,
}

/// Mount inside the listener's shared authentication middleware. Every read
/// still requires its authenticated host's current frozen workflow membership.
/// `cancel` ends in-flight database snapshots during server shutdown; each
/// snapshot also has one five-second deadline covering acquisition through commit.
pub(crate) fn router(
    pool: PgPool,
    cache: FirmwareArtifactCache,
    cancel: CancellationToken,
) -> Router {
    Router::new()
        .route(
            "/firmware/nic/{digest}/{role}",
            get(download)
                .head(|| async { (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")]) }),
        )
        .with_state(ArtifactState {
            pool,
            cache,
            requests: Arc::new(Semaphore::new(MAX_REQUESTS)),
            cancel,
        })
}

#[tracing::instrument(skip_all, fields(host_machine_id, operation_id, artifact_digest))]
async fn download(
    State(state): State<ArtifactState>,
    Path((digest, role)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    auth: Option<Extension<AuthContext>>,
) -> Response {
    let Some(identity) = auth.as_ref().and_then(|auth| auth.get_spiffe_machine_id()) else {
        return (StatusCode::UNAUTHORIZED, "host identity required").into_response();
    };
    let Ok(host) = identity.parse::<HostMachineId>() else {
        return (StatusCode::FORBIDDEN, "host identity required").into_response();
    };
    let Ok(digest) = digest.parse::<ArtifactDigest>() else {
        return (StatusCode::BAD_REQUEST, "invalid artifact digest").into_response();
    };
    let Some(operation_id) = operation_id(query.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "invalid operation identifier").into_response();
    };
    if !matches!(role.as_str(), "image" | "configuration") {
        return unavailable();
    }
    let span = tracing::Span::current();
    span.record("host_machine_id", tracing::field::display(host));
    span.record("operation_id", &operation_id);
    span.record("artifact_digest", tracing::field::display(digest));
    let Ok(permit) = state.requests.clone().try_acquire_owned() else {
        return retry(StatusCode::SERVICE_UNAVAILABLE, "firmware service is busy");
    };
    let authorized = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => {
            return retry(StatusCode::SERVICE_UNAVAILABLE, "firmware service unavailable");
        }
        result = tokio::time::timeout(
            SNAPSHOT_TIMEOUT,
            load_snapshot(&state.pool, host, &operation_id, digest, &role),
        ) => {
            match result {
                Ok(Ok(Some(authorized))) => authorized,
                Ok(Ok(None)) => return unavailable(),
                Ok(Err(response)) => return response,
                Err(_) => {
                    tracing::warn!("NIC firmware artifact database snapshot deadline exceeded");
                    return retry(StatusCode::SERVICE_UNAVAILABLE, "firmware state unavailable");
                }
            }
        }
    };
    let AuthorizedArtifact { artifact, retained } = authorized;
    match state.cache.open(&artifact.url, digest, &retained).await {
        Ok(CacheLookup::Ready(artifact)) => {
            let length = artifact.content_length();
            let reader = ArtifactReader {
                artifact: Some(artifact),
                permit: Some(permit),
            };
            (
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (header::CONTENT_LENGTH, length.to_string()),
                ],
                Body::from_stream(ReaderStream::new(reader)),
            )
                .into_response()
        }
        Ok(CacheLookup::Pending) => retry(StatusCode::ACCEPTED, "firmware artifact pending"),
        Err(CacheError::Capacity) => {
            retry(StatusCode::SERVICE_UNAVAILABLE, "firmware cache is full")
        }
        Err(CacheError::Cancelled) => retry(
            StatusCode::SERVICE_UNAVAILABLE,
            "firmware service unavailable",
        ),
        Err(CacheError::Io(error)) => {
            tracing::warn!(%error, "NIC firmware artifact cache I/O failed");
            retry(StatusCode::BAD_GATEWAY, "firmware artifact unavailable")
        }
        Err(CacheError::Download(_)) => {
            tracing::warn!("NIC firmware artifact download failed");
            retry(StatusCode::BAD_GATEWAY, "firmware artifact unavailable")
        }
        Err(
            CacheError::InvalidDigest | CacheError::InvalidSource | CacheError::InvalidCapacity,
        ) => {
            tracing::warn!("Invalid persisted NIC firmware artifact cache inputs");
            retry(
                StatusCode::SERVICE_UNAVAILABLE,
                "firmware state unavailable",
            )
        }
    }
}

struct AuthorizedArtifact {
    artifact: NicFirmwareArtifact,
    retained: HashSet<ArtifactDigest>,
}

// Authorization and eviction membership must come from the same SQL snapshot.
// The session guard discards a blocked connection when this future is dropped,
// rather than leaving its pool slot occupied by SQLx's asynchronous rollback.
async fn load_snapshot(
    pool: &PgPool,
    host_machine_id: HostMachineId,
    operation_id: &str,
    digest: ArtifactDigest,
    role: &str,
) -> Result<Option<AuthorizedArtifact>, Response> {
    let mut session = DbSessionGuard::acquire(pool)
        .await
        .map_err(snapshot_error)?;
    let mut txn = session
        .begin_read_only_snapshot(SNAPSHOT_TIMEOUT)
        .await
        .map_err(snapshot_error)?;
    let authorized: Result<Option<AuthorizedArtifact>, Response> = async {
        let operation =
            match db::machine::lookup_managed_host_state(txn.as_pgconn(), host_machine_id.into())
                .await
                .map_err(snapshot_error)?
            {
                Some(ManagedHostState::NicFirmwareUpdating { operation }) => operation,
                _ => return Ok(None),
            };
        if operation.validate().is_err() {
            tracing::warn!("Invalid persisted NIC firmware artifact authorization");
            return Err(retry(
                StatusCode::SERVICE_UNAVAILABLE,
                "firmware state unavailable",
            ));
        }
        if operation.operation_id != operation_id {
            return Ok(None);
        }
        let Some(target) = operation.current_target() else {
            return Ok(None);
        };
        let live_receipt = target.receipt.as_ref().is_some_and(|receipt| {
            receipt.operation_id == operation.operation_id
                && matches!(
                    receipt.state,
                    FirmwareOperationState::Accepted
                        | FirmwareOperationState::Running
                        | FirmwareOperationState::CancelRequested
                )
        });
        if target.receipt.is_some() && !live_receipt {
            return Ok(None);
        }
        match operation.phase {
            NicFirmwarePhase::UpdatingFirmware { .. } => {}
            NicFirmwarePhase::ObservingSupervisor { .. } if live_receipt => {}
            _ => return Ok(None),
        }
        let entry = &target.resolution.entry;
        let artifact = match role {
            "image" => Some(&entry.image),
            "configuration" => entry.device_config.as_ref(),
            _ => None,
        };
        let Some(artifact) = artifact
            .filter(|artifact| artifact.sha256.parse::<ArtifactDigest>().ok() == Some(digest))
        else {
            return Ok(None);
        };
        let artifact = artifact.clone();

        // Frozen membership cannot grow through existing workflow transitions,
        // so this snapshot can only retain extra files. Admission must coordinate
        // new membership with eviction before enabling a producer:
        // https://github.com/dsx-ai-factory/infra-controller/issues/6680
        let operations = db::machine::find_host_nic_firmware_operations(txn.as_pgconn())
            .await
            .map_err(snapshot_error)?;
        let mut retained = HashSet::new();
        for (retained_host_machine_id, operation) in operations {
            if operation.validate().is_err() {
                tracing::warn!(
                    %retained_host_machine_id,
                    error_category = "invalid_retained_operation",
                    "Invalid persisted NIC firmware artifact retention"
                );
                return Err(retry(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "firmware state unavailable",
                ));
            }
            if matches!(operation.phase, NicFirmwarePhase::Failed { .. }) {
                continue;
            }
            for target in operation.targets {
                let entry = target.resolution.entry;
                for artifact in std::iter::once(entry.image).chain(entry.device_config) {
                    let digest = artifact.sha256.parse::<ArtifactDigest>().map_err(|_| {
                        retry(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "firmware state unavailable",
                        )
                    })?;
                    retained.insert(digest);
                }
            }
        }
        Ok(Some(AuthorizedArtifact { artifact, retained }))
    }
    .await;
    // Validation denials leave a healthy read-only transaction. Complete it
    // before returning the response so a bad operation does not churn the pool.
    // SQL failures still discard the session if COMMIT cannot finish normally.
    txn.commit().await.map_err(snapshot_error)?;
    session.return_to_pool();
    authorized
}

fn snapshot_error(error: db::DatabaseError) -> Response {
    // SQLx decode errors can include persisted JSON. Classify them without
    // dumping frozen source URLs or arbitrary database contents into logs.
    let error_category = match &error {
        db::DatabaseError::Sqlx(error) => match &error.source {
            sqlx::Error::PoolTimedOut => "pool_timeout",
            sqlx::Error::PoolClosed => "pool_closed",
            sqlx::Error::Database(_) => "database",
            sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode",
            _ => "database_io",
        },
        _ => "snapshot",
    };
    tracing::warn!(
        error_category,
        "NIC firmware artifact database snapshot failed"
    );
    retry(
        StatusCode::SERVICE_UNAVAILABLE,
        "firmware state unavailable",
    )
}

fn operation_id(query: Option<&str>) -> Option<String> {
    let query = query.filter(|query| query.len() <= 1024)?;
    let mut fields = url::form_urlencoded::parse(query.as_bytes());
    let (name, value) = fields.next()?;
    if name != "operation_id"
        || fields.next().is_some()
        || !(1..=256).contains(&value.len())
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(value.into_owned())
}

fn unavailable() -> Response {
    (StatusCode::NOT_FOUND, "firmware artifact unavailable").into_response()
}

fn retry(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        [(header::RETRY_AFTER, RETRY_AFTER_SECONDS)],
        message,
    )
        .into_response()
}

// The body owns request admission and the cached reader. Disconnect releases
// request admission; the cache keeps any physical I/O lease until its worker
// finishes. EOF releases both even if the caller keeps the body.
struct ArtifactReader {
    artifact: Option<CachedArtifact>,
    permit: Option<OwnedSemaphorePermit>,
}

impl AsyncRead for ArtifactReader {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let reader = self.get_mut();
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let Some(artifact) = reader.artifact.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let previous_length = buffer.filled().len();
        let result = Pin::new(artifact).poll_read(context, buffer);
        if matches!(&result, Poll::Ready(Err(_)))
            || (matches!(&result, Poll::Ready(Ok(()))) && previous_length == buffer.filled().len())
        {
            reader.artifact = None;
            reader.permit = None;
        }
        result
    }
}
