// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bounded calls to the resource pool provider API with validated responses.

mod config;
mod response;
mod transport;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tls_tests;

use carbide_resource_pool_api::v1;
pub use config::{GrpcResourcePoolConfigError, ResourcePoolTlsConfig, validate_endpoint};
use thiserror::Error;
use transport::Transport;
use url::Url;

/// `GrpcResourcePoolBackend` executes one provider call per method invocation.
///
/// Requests must use the caller's saved identities and immutable inputs. This
/// client neither generates identities nor retries requests. Dropping a call
/// stops local waiting; it cannot undo work already received by the provider.
/// The caller must reconcile uncertain requests using its saved operation
/// identity. If recovery keeps returning malformed responses, use Cancel with
/// that original identity and require a valid terminal reply before cleanup.
///
/// Successful responses are checked for required fields, valid wire values,
/// and agreement with the request. The caller must separately check the saved
/// value domain, eligibility revision's declarations, consumer type, and routing
/// admission policy; those inputs are absent from the wire allocation request.
/// No provider assignments or eligibility definitions are cached here.
#[derive(Clone)]
pub struct GrpcResourcePoolBackend {
    transport: Transport,
}

/// `GrpcResourcePoolError` distinguishes failures before dispatch from responses
/// that leave the provider's work uncertain. `InvalidConfiguration`,
/// `Preparation`, `Credentials`, `Tls`, `Connect`, and `Busy` occur before this
/// invocation sends its RPC. `Rpc`, `DeadlineExceeded`, and `InvalidResponse`
/// may follow dispatch; they do not prove that a mutating request failed.
///
/// Recovery decisions belong to the caller, using its saved operation identity
/// and the method's contract. An RPC status code alone, including `NotFound`,
/// does not establish whether an allocation committed or cleanup completed.
#[derive(Debug, Error)]
pub enum GrpcResourcePoolError {
    /// The endpoint or TLS settings violate a local configuration requirement.
    #[error("invalid resource pool provider configuration: {0}")]
    InvalidConfiguration(#[from] GrpcResourcePoolConfigError),
    /// The credential and TLS preparation task failed before dispatch.
    #[error(transparent)]
    Preparation(PreparationError),
    /// A configured credential file could not be read or contains malformed credentials.
    #[error("failed to read or validate resource pool provider {file}")]
    Credentials {
        /// Credential role, excluding its path or contents.
        file: CredentialFile,
        /// The I/O or content-validation error for this credential.
        source: std::io::Error,
    },
    /// TLS configuration could not be applied to the provider endpoint.
    #[error("failed to configure resource pool provider TLS")]
    Tls(#[source] tonic::transport::Error),
    /// A connection to the provider could not be established.
    #[error("failed to connect to resource pool provider")]
    Connect(#[source] tonic::transport::Error),
    /// The connection or complete call exceeded its local deadline.
    #[error("resource pool provider call deadline exceeded")]
    DeadlineExceeded,
    /// This client already has the maximum number of admitted calls.
    #[error("resource pool provider client is busy")]
    Busy,
    /// The peer or local gRPC stack returned a status with its code preserved.
    #[error(transparent)]
    Rpc(RpcError),
    /// A successful response violated the protocol or did not match the request.
    #[error("invalid resource pool provider response: {0}")]
    InvalidResponse(&'static str),
}

/// `CredentialFile` identifies a configured credential without its path or contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialFile {
    /// CA certificate bundle used to verify the provider's certificate.
    CaCertificate,
    /// Client certificate chain presented to the provider.
    ClientCertificate,
    /// Private key paired with the client certificate chain.
    ClientKey,
}

impl std::fmt::Display for CredentialFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CaCertificate => "ca_cert_file",
            Self::ClientCertificate => "client_cert_file",
            Self::ClientKey => "client_key_file",
        })
    }
}

/// `PreparationError` retains a failed credential and TLS preparation task.
/// Formatting and error sources omit its panic payload, which may contain
/// credential paths or contents. Inspect [`Self::join_error`] explicitly when
/// those unredacted diagnostics are needed.
pub struct PreparationError {
    error: tokio::task::JoinError,
}

impl PreparationError {
    /// `join_error` returns the original task failure. Its panic payload is
    /// unredacted; callers must redact it before recording or exposing it.
    pub fn join_error(&self) -> &tokio::task::JoinError {
        &self.error
    }
}

impl From<tokio::task::JoinError> for PreparationError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self { error }
    }
}

impl std::fmt::Display for PreparationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.error.is_cancelled() {
            "resource pool provider preparation task was cancelled"
        } else {
            "resource pool provider preparation task panicked"
        })
    }
}

impl std::fmt::Debug for PreparationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparationError")
            .field("cancelled", &self.error.is_cancelled())
            .field("panicked", &self.error.is_panic())
            .finish()
    }
}

impl std::error::Error for PreparationError {}

/// `RpcError` retains a status from the peer or local gRPC stack.
///
/// Codes are preserved without classifying whether an allocation committed.
/// Display, Debug, and error sources omit status messages, details, and metadata:
/// a remote peer can put credentials or request contents into those fields.
pub struct RpcError {
    status: Box<tonic::Status>,
}

impl RpcError {
    /// `code` returns the unmodified gRPC status code. Interpret it using the
    /// called method's contract; local gRPC failures can use the same codes.
    pub fn code(&self) -> tonic::Code {
        self.status.code()
    }

    /// `status` permits explicit inspection of the original diagnostics.
    /// Messages, details, metadata, and source errors are untrusted and unredacted;
    /// callers must redact them before recording or exposing them.
    pub fn status(&self) -> &tonic::Status {
        &self.status
    }

    /// `into_status` returns the original status, including unredacted diagnostics.
    /// Callers must apply the same redaction required by [`Self::status`].
    pub fn into_status(self) -> tonic::Status {
        *self.status
    }
}

impl From<tonic::Status> for RpcError {
    fn from(status: tonic::Status) -> Self {
        Self {
            status: Box::new(status),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "resource pool RPC returned {:?}", self.code())
    }
}

impl std::fmt::Debug for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RpcError")
            .field("code", &self.code())
            .finish()
    }
}

impl std::error::Error for RpcError {}

impl GrpcResourcePoolBackend {
    /// `new` checks local endpoint and TLS settings without connecting or reading
    /// credentials. Calls read configured credential files before channel reuse.
    /// Each call has a ten-second total deadline, including waiting for the
    /// credential refresh lock, reading credentials, connecting, and receiving
    /// the response body and trailers. Each connection attempt also has a
    /// five-second deadline covering DNS, TCP, and TLS. At most 32 calls run
    /// concurrently per shared client; additional calls return
    /// [`GrpcResourcePoolError::Busy`] without queuing.
    ///
    /// At most one credential/TLS preparation job is outstanding per shared client.
    /// Already-started filesystem calls can outlive cancellation or the deadline;
    /// the job retains the refresh lock until it finishes, so later calls wait
    /// within their own budgets without starting more preparation jobs. Runtime
    /// shutdown can still wait for a blocked filesystem call.
    /// With request/response streams open, HTTP/2 keepalive uses a ten-second
    /// interval and a five-second acknowledgement timeout. Idle connections do
    /// not send keepalive pings.
    ///
    /// Encoded requests and decoded responses are limited to 4 MiB each. HTTPS
    /// verifies TLS; HTTP is allowed without TLS settings.
    pub fn new(
        endpoint: Url,
        tls: Option<ResourcePoolTlsConfig>,
    ) -> Result<Self, GrpcResourcePoolError> {
        Ok(Self {
            transport: Transport::new(endpoint, tls)?,
        })
    }

    /// `describe` resolves an alias to nonempty provider and pool identities and a
    /// recognized value domain. Callers compare these with any saved binding.
    pub async fn describe(
        &self,
        request: v1::DescribeRequest,
    ) -> Result<v1::DescribeResponse, GrpcResourcePoolError> {
        let response = self
            .transport
            .call("describe", request, |mut client, request| async move {
                client.describe(request).await
            })
            .await?;
        response::describe(&response)?;
        Ok(response)
    }

    /// `define` acknowledges an immutable eligibility revision. It does not
    /// create inventory or establish that eligible values are available.
    pub async fn define(
        &self,
        request: v1::DefineRequest,
    ) -> Result<v1::DefineResponse, GrpcResourcePoolError> {
        self.transport
            .call("define", request, |mut client, request| async move {
                client.define(request).await
            })
            .await
    }

    /// `allocate` returns a ready assignment matching the complete operation.
    /// Omitted `requested_value` selects automatic mode; a present value requires
    /// that exact value in manual mode. Requests are sent unchanged so the
    /// provider can check saved inputs before validating a replayed body.
    pub async fn allocate(
        &self,
        request: v1::AllocateRequest,
    ) -> Result<v1::AllocateResponse, GrpcResourcePoolError> {
        let expected = request.clone();
        let response = self
            .transport
            .call("allocate", request, |mut client, request| async move {
                client.allocate(request).await
            })
            .await?;
        response::allocate(&expected, &response)?;
        Ok(response)
    }

    /// `allocate_exact` requires the returned value and operation to match the
    /// request. Either valid assignment mode is accepted; callers check the mode
    /// against the saved eligibility definition.
    pub async fn allocate_exact(
        &self,
        request: v1::AllocateExactRequest,
    ) -> Result<v1::AllocateExactResponse, GrpcResourcePoolError> {
        let expected = request.clone();
        let response = self
            .transport
            .call(
                "allocate_exact",
                request,
                |mut client, request| async move { client.allocate_exact(request).await },
            )
            .await?;
        response::allocate_exact(&expected, &response)?;
        Ok(response)
    }

    /// `recover` returns the state of the original operation without allocating.
    /// `Unknown` permits a delayed allocation to arrive. Only `Cancelled` and
    /// `Released` establish terminal cleanup; an allocated response is also checked
    /// against the queried pool and request identity.
    pub async fn recover(
        &self,
        request: v1::RecoverRequest,
    ) -> Result<v1::RecoverResponse, GrpcResourcePoolError> {
        let expected = request.clone();
        let response = self
            .transport
            .call("recover", request, |mut client, request| async move {
                client.recover(request).await
            })
            .await?;
        response::recover(&expected, &response)?;
        Ok(response)
    }

    /// `cancel` stops the original operation, including one not yet observed by
    /// the provider. A successful response must be `Pending`, `Cancelled`, or
    /// `Released`. `Pending` requires polling `recover` with the same identity.
    pub async fn cancel(
        &self,
        request: v1::CancelRequest,
    ) -> Result<v1::CancelResponse, GrpcResourcePoolError> {
        let expected = request.clone();
        let response = self
            .transport
            .call("cancel", request, |mut client, request| async move {
                client.cancel(request).await
            })
            .await?;
        response::cancel(&expected, &response)?;
        Ok(response)
    }

    /// `lookup` returns a valid active assignment for the requested pool and
    /// allocation handle. Callers compare its other fields with saved assignment
    /// data before attaching it to an entity.
    pub async fn lookup(
        &self,
        request: v1::LookupRequest,
    ) -> Result<v1::LookupResponse, GrpcResourcePoolError> {
        let expected = request.clone();
        let response = self
            .transport
            .call("lookup", request, |mut client, request| async move {
                client.lookup(request).await
            })
            .await?;
        response::lookup(&expected, &response)?;
        Ok(response)
    }

    /// `release` frees the identified allocation when its complete owner matches.
    /// `applied = false` is preserved and does not establish cleanup; use `recover`
    /// on the original operation when terminal confirmation is required.
    pub async fn release(
        &self,
        request: v1::ReleaseRequest,
    ) -> Result<v1::ReleaseResponse, GrpcResourcePoolError> {
        self.transport
            .call("release", request, |mut client, request| async move {
                client.release(request).await
            })
            .await
    }

    /// `stats` returns a present observation with checked automatic/manual totals.
    /// Unsupported or unavailable statistics remain RPC errors, never zeros.
    pub async fn stats(
        &self,
        request: v1::StatsRequest,
    ) -> Result<v1::StatsResponse, GrpcResourcePoolError> {
        let response = self
            .transport
            .call("stats", request, |mut client, request| async move {
                client.stats(request).await
            })
            .await?;
        response::stats(&response)?;
        Ok(response)
    }

    #[cfg(test)]
    fn with_timeouts(mut self, connect: std::time::Duration, call: std::time::Duration) -> Self {
        self.transport = self.transport.with_timeouts(connect, call);
        self
    }
}
