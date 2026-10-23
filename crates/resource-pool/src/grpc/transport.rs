// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use carbide_resource_pool_api::v1::resource_pool_client::ResourcePoolClient;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Instant, timeout, timeout_at};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Request, Response, Status};
use trace_propagation::TraceInjectService;
use url::Url;

use super::{
    CredentialFile, GrpcResourcePoolConfigError, GrpcResourcePoolError, ResourcePoolTlsConfig,
    validate_endpoint,
};

// One provider attempt must leave time inside the REST activity's 40-second
// budget (`rest-api/common/pkg/util/workflow.go`) for Core to save its result
// and return. These bounds do not expire a reservation; a slower provider can
// still finish and be queried with Recover.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

// Match tonic's normal receive limit in both directions. Definitions stay
// compact; neither credential files nor a provider reply can grow without bound.
pub(crate) const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PEM_BYTES: u64 = 256 * 1024;

// Share 32 slots across clones to bound memory and provider concurrency. Excess
// calls return Busy immediately; each admitted call keeps its slot through the
// complete response. Credential refresh is serialized, and waiting for its lock
// consumes the same call budget as connecting and receiving the response.
const MAX_IN_FLIGHT: usize = 32;

type Client = ResourcePoolClient<TraceInjectService<Channel>>;

/// `Transport` shares one connection and its credential snapshot across clones.
#[derive(Clone)]
pub(super) struct Transport {
    inner: Arc<Connection>,
    connect_timeout: Duration,
    call_timeout: Duration,
}

struct Connection {
    endpoint: Url,
    tls: Option<ResourcePoolTlsConfig>,
    cached: Arc<Mutex<Option<CachedClient>>>,
    in_flight: Semaphore,
}

struct CachedClient {
    client: Client,
    credentials: Credentials,
}

enum PreparedConnection {
    Reuse(Client),
    Connect {
        endpoint: Box<Endpoint>,
        credentials: Credentials,
    },
}

// Do not derive Debug: this snapshot includes the private key.
#[derive(Default, PartialEq, Eq)]
struct Credentials {
    ca: Option<Vec<u8>>,
    certificate: Option<Vec<u8>>,
    key: Option<Vec<u8>>,
}

impl Transport {
    pub(super) fn new(
        endpoint: Url,
        tls: Option<ResourcePoolTlsConfig>,
    ) -> Result<Self, GrpcResourcePoolError> {
        validate_endpoint(&endpoint, tls.as_ref())?;
        // An empty TLS table has no files to refresh before reusing a connection.
        let tls = tls.filter(|tls| *tls != ResourcePoolTlsConfig::default());
        Ok(Self {
            inner: Arc::new(Connection {
                endpoint,
                tls,
                cached: Arc::new(Mutex::new(None)),
                in_flight: Semaphore::new(MAX_IN_FLIGHT),
            }),
            connect_timeout: CONNECT_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
        })
    }

    #[tracing::instrument(skip_all, fields(resource_pool_operation = operation))]
    pub(super) async fn call<T, R, F, Fut>(
        &self,
        operation: &'static str,
        request: T,
        call: F,
    ) -> Result<R, GrpcResourcePoolError>
    where
        F: FnOnce(Client, Request<T>) -> Fut,
        Fut: Future<Output = Result<Response<R>, Status>>,
    {
        let deadline = Instant::now() + self.call_timeout;
        let _slot = self
            .inner
            .in_flight
            .try_acquire()
            .map_err(|_| GrpcResourcePoolError::Busy)?;
        // The outer deadline includes the response body and trailers; tonic
        // stops its own request timer once response headers arrive.
        timeout_at(deadline, async {
            let client = self.client().await?;
            let mut request = Request::new(request);
            request.set_timeout(deadline.saturating_duration_since(Instant::now()));
            call(client, request)
                .await
                .map(Response::into_inner)
                .map_err(|status| {
                    if is_timeout(&status) {
                        GrpcResourcePoolError::DeadlineExceeded
                    } else {
                        GrpcResourcePoolError::Rpc(status.into())
                    }
                })
        })
        .await
        .map_err(|_| GrpcResourcePoolError::DeadlineExceeded)?
    }

    async fn client(&self) -> Result<Client, GrpcResourcePoolError> {
        // The preparation job keeps the refresh lock even if its caller stops
        // waiting. A stuck filesystem operation therefore leaves at most one
        // blocking job per shared client; later calls wait within their budgets.
        let cached = self.inner.cached.clone().lock_owned().await;
        if self.inner.tls.is_none()
            && let Some(cached) = cached.as_ref()
        {
            return Ok(cached.client.clone());
        }
        let transport = self.clone();
        let (mut cached, prepared) = tokio::task::spawn_blocking(move || {
            let prepared = transport.prepare(cached.as_ref())?;
            Ok::<_, GrpcResourcePoolError>((cached, prepared))
        })
        .await
        .map_err(|error| GrpcResourcePoolError::Preparation(error.into()))??;
        let (endpoint, credentials) = match prepared {
            PreparedConnection::Reuse(client) => return Ok(client),
            PreparedConnection::Connect {
                endpoint,
                credentials,
            } => (endpoint, credentials),
        };

        // Keep the refresh lock through publication so an older snapshot cannot
        // replace a newer connection. Only the waiting caller connects or caches;
        // a preparation result abandoned by cancellation is dropped.

        // This entry point lets tonic wrap DNS, TCP, and TLS in one timeout
        // that also applies when the cached channel reconnects. The regular
        // connect() path only configures the TCP connector's timeout.
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_nodelay(true);
        connector.set_connect_timeout(Some(self.connect_timeout));
        // The outer connection timer also covers the initial HTTP/2 handshake.
        let channel = timeout(
            self.connect_timeout,
            endpoint.connect_with_connector(connector),
        )
        .await
        .map_err(|_| GrpcResourcePoolError::DeadlineExceeded)?
        .map_err(|error| {
            if is_timeout(&error) {
                GrpcResourcePoolError::DeadlineExceeded
            } else {
                GrpcResourcePoolError::Connect(error)
            }
        })?;
        let client = ResourcePoolClient::new(TraceInjectService::new(channel))
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
            .max_decoding_message_size(MAX_MESSAGE_BYTES);
        *cached = Some(CachedClient {
            client: client.clone(),
            credentials,
        });
        Ok(client)
    }

    fn prepare(
        &self,
        cached: Option<&CachedClient>,
    ) -> Result<PreparedConnection, GrpcResourcePoolError> {
        let credentials = match &self.inner.tls {
            Some(tls) => Credentials {
                ca: read_pem(tls.ca_cert_file.as_deref(), CredentialFile::CaCertificate)?,
                certificate: read_pem(
                    tls.client_cert_file.as_deref(),
                    CredentialFile::ClientCertificate,
                )?,
                key: read_pem(tls.client_key_file.as_deref(), CredentialFile::ClientKey)?,
            },
            None => Credentials::default(),
        };
        if let Some(cached) = cached
            && cached.credentials == credentials
        {
            return Ok(PreparedConnection::Reuse(cached.client.clone()));
        }

        // With request/response streams open, HTTP/2 keepalive uses the call
        // budget (10 seconds) for its interval and the connect budget (5 seconds)
        // for its acknowledgement timeout.
        let mut endpoint = Endpoint::from_shared(self.inner.endpoint.to_string())
            .map_err(|_| GrpcResourcePoolConfigError::InvalidEndpoint)?
            .connect_timeout(self.connect_timeout)
            .http2_keep_alive_interval(self.call_timeout)
            .keep_alive_timeout(self.connect_timeout);
        if self.inner.endpoint.scheme() == "https" {
            let mut tls = ClientTlsConfig::new();
            if let Some(host) = self.inner.endpoint.host_str() {
                // URL authorities bracket IPv6 addresses; rustls verifies the
                // address itself, without those brackets.
                tls = tls.domain_name(host.trim_matches(['[', ']']));
            }
            tls = match &credentials.ca {
                Some(ca) => {
                    validate_ca(ca)?;
                    tls.ca_certificate(Certificate::from_pem(ca))
                }
                None => tls.with_native_roots(),
            };
            if let (Some(certificate), Some(key)) = (&credentials.certificate, &credentials.key) {
                tls = tls.identity(Identity::from_pem(certificate, key));
            }
            // Native roots are loaded synchronously inside `tls_config`, so
            // endpoint preparation must stay on the blocking worker too.
            endpoint = endpoint
                .tls_config(tls)
                .map_err(GrpcResourcePoolError::Tls)?;
        }
        Ok(PreparedConnection::Connect {
            endpoint: Box::new(endpoint),
            credentials,
        })
    }

    #[cfg(test)]
    pub(super) fn with_timeouts(mut self, connect: Duration, call: Duration) -> Self {
        self.connect_timeout = connect;
        self.call_timeout = call;
        self
    }
}

fn read_pem(
    path: Option<&Path>,
    file: CredentialFile,
) -> Result<Option<Vec<u8>>, GrpcResourcePoolError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let read = || {
        let mut options = OpenOptions::new();
        options.read(true);
        // Inspect the opened file, so a symlink swap cannot bypass the check.
        // Nonblocking open also prevents a substituted FIFO from waiting for a writer.
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK);
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "credential source must be a regular file",
            ));
        }
        let mut pem = Vec::new();
        file.take(MAX_PEM_BYTES + 1).read_to_end(&mut pem)?;
        if pem.is_empty() || pem.len() as u64 > MAX_PEM_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "credential file must contain between 1 byte and 256 KiB",
            ));
        }
        Ok(pem)
    };
    read()
        .map(Some)
        .map_err(|source| GrpcResourcePoolError::Credentials { file, source })
}

/// `validate_ca` rejects empty bundles and invalid certificates, including those
/// beside a valid certificate that tonic's permissive root loader could ignore.
fn validate_ca(pem: &[u8]) -> Result<(), GrpcResourcePoolError> {
    let invalid = || GrpcResourcePoolError::Credentials {
        file: CredentialFile::CaCertificate,
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CA bundle must contain valid PEM certificates",
        ),
    };
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(pem) {
        roots
            .add(certificate.map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    }
    if roots.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

/// Recognize local timers without interpreting a peer's status code or message.
fn is_timeout(error: &(dyn Error + 'static)) -> bool {
    std::iter::successors(Some(error), |&error| error.source()).any(|error| {
        error.is::<tonic::TimeoutExpired>()
            || error.is::<tokio::time::error::Elapsed>()
            || error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
    })
}

#[cfg(test)]
mod tests;
