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

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use carbide_secrets::credentials::Credentials;
use carbide_utils::HostPortPair;
use carbide_utils::redfish::{
    format_forwarded_host_parameter, log_redfish_http_error, redact_redfish_response_body,
    redfish_basic_authorization_context,
};
pub use nv_redfish::bmc_http::reqwest::BmcError;
use nv_redfish::bmc_http::reqwest::{
    Client as RedfishReqwestClient, ClientParams as RedfishReqwestClientParams,
};
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use nv_redfish::core::query::{ExpandQuery, FilterQuery};
use nv_redfish::core::upload::{HttpPushUriUpdateRequest, MultipartUpdateRequest, UploadReader};
use nv_redfish::core::{
    Action, Bmc, BoxTryStream, EntityTypeRef, Expandable, ModificationResponse, ODataETag, ODataId,
    Repair, RepairError, SessionCreateResponse,
};
use nv_redfish::oem::hpe::ilo_service_ext::ManagerType as HpeManagerType;
use nv_redfish::{Error as NvError, ServiceRoot as NvServiceRoot};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use url::Url;

mod span_isolated_http_client;

use span_isolated_http_client::SpanIsolatedHttpClient;

const NV_REDFISH_BACKEND: &str = "nv-redfish";

type InnerRedfishBmc = HttpBmc<SpanIsolatedHttpClient>;

/// An `nv-redfish` BMC that emits one structured warning for each failed HTTP
/// operation and removes request credentials from response-bearing errors
/// before either logging or returning them to callers.
///
/// The decorator retains the credential representations used to authenticate
/// the client, plus session-creation values for that operation, solely so it
/// can sanitize untrusted BMC response text. Unsafe error bodies fail closed
/// through the shared Redfish sanitizer. Successful responses, cache behavior,
/// and transport errors that contain no BMC response body remain unchanged.
///
/// This decorator sits outside `HttpBmc`, after its ETag cache handling, so an
/// internal `304 Not Modified` cache signal is not misreported as a failure.
pub struct RedfishBmc {
    inner: InnerRedfishBmc,
    sensitive_values: Arc<[String]>,
}

impl RedfishBmc {
    fn new(inner: InnerRedfishBmc, sensitive_values: Arc<[String]>) -> Self {
        Self {
            inner,
            sensitive_values,
        }
    }

    fn finish<T>(
        &self,
        operation: &'static str,
        result: Result<T, BmcError>,
    ) -> Result<T, BmcError> {
        self.finish_with_sensitive_values(operation, result, &self.sensitive_values)
    }

    fn finish_with_sensitive_values<T>(
        &self,
        operation: &'static str,
        result: Result<T, BmcError>,
        sensitive_values: &[String],
    ) -> Result<T, BmcError> {
        let result = redact_bmc_error(result, sensitive_values);
        if let Err(error) = &result {
            log_bmc_error(operation, error, sensitive_values);
        }
        result
    }

    fn finish_repaired<T>(
        &self,
        operation: &'static str,
        result: Result<T, RepairError<BmcError>>,
    ) -> Result<T, RepairError<BmcError>> {
        let result = redact_repair_error(result, &self.sensitive_values);
        if let Err(RepairError::Transport(error)) = &result {
            log_bmc_error(operation, error, &self.sensitive_values);
        }
        result
    }

    /// Session creation can submit credentials that differ from the client's
    /// authentication credential. Include every non-empty string in that small
    /// request document when sanitizing the returned and logged HTTP error.
    fn finish_session<T, V: Serialize>(
        &self,
        operation: &'static str,
        query: &V,
        result: Result<T, BmcError>,
    ) -> Result<T, BmcError> {
        let mut sensitive_values = self.sensitive_values.to_vec();
        if let Ok(query) = serde_json::to_value(query) {
            collect_nonempty_json_strings(&query, &mut sensitive_values);
        }
        sensitive_values.sort_unstable();
        sensitive_values.dedup();

        self.finish_with_sensitive_values(operation, result, &sensitive_values)
    }
}

fn redact_repair_error<T>(
    result: Result<T, RepairError<BmcError>>,
    sensitive_values: &[String],
) -> Result<T, RepairError<BmcError>> {
    result.map_err(|error| match error {
        RepairError::Transport(error) => {
            RepairError::Transport(match redact_bmc_error::<()>(Err(error), sensitive_values) {
                Err(error) => error,
                Ok(()) => unreachable!("an Err result stays an Err"),
            })
        }
        error @ RepairError::Decode(_) => error,
    })
}

impl RedfishBmc {
    /// A write can carry a password other than the client's own, such as an
    /// account's new password or a BIOS password change. Include the values
    /// of request fields named like a password when sanitizing the returned
    /// and logged HTTP error.
    fn finish_request<T, V: Serialize>(
        &self,
        operation: &'static str,
        request: &V,
        result: Result<T, BmcError>,
    ) -> Result<T, BmcError> {
        if result.is_ok() {
            return result;
        }
        let mut sensitive_values = self.sensitive_values.to_vec();
        if let Ok(request) = serde_json::to_value(request) {
            collect_password_fields(&request, &mut sensitive_values);
        }
        sensitive_values.sort_unstable();
        sensitive_values.dedup();

        self.finish_with_sensitive_values(operation, result, &sensitive_values)
    }
}

fn redact_bmc_error<T>(
    result: Result<T, BmcError>,
    sensitive_values: &[String],
) -> Result<T, BmcError> {
    if sensitive_values.is_empty() {
        return result;
    }

    result.map_err(|error| match error {
        BmcError::InvalidResponse { url, status, text } => BmcError::InvalidResponse {
            url,
            status,
            text: redact_redfish_response_body(&text, sensitive_values.iter().map(String::as_str)),
        },
        error => error,
    })
}

fn log_bmc_error(operation: &str, error: &BmcError, sensitive_values: &[String]) {
    if let BmcError::InvalidResponse { url, status, text } = error {
        log_redfish_http_error(
            NV_REDFISH_BACKEND,
            operation,
            url.as_str(),
            status.as_u16(),
            text,
            sensitive_values.iter().map(String::as_str),
        );
    }
}

/// Collects each reusable credential representation that nv-redfish puts on
/// the wire so an untrusted BMC error cannot echo a derived value unchanged.
fn sensitive_values(credentials: &BmcCredentials) -> Arc<[String]> {
    let sensitive_values = match credentials {
        BmcCredentials::UsernamePassword { username, password } => {
            // Use the shared context so all Redfish clients protect plaintext,
            // bare payload, and complete-header forms consistently.
            let (_, sensitive_values) =
                redfish_basic_authorization_context(username, password.as_deref());
            sensitive_values
        }
        BmcCredentials::Token { token } if !token.is_empty() => {
            // Session tokens are already sent verbatim in X-Auth-Token.
            vec![token.clone()]
        }
        BmcCredentials::Token { .. } | BmcCredentials::Anonymous => Vec::new(),
    };
    Arc::from(sensitive_values)
}

/// Collects the non-empty strings under every field whose name contains
/// `password`, in any letter case and at any depth.
fn collect_password_fields(value: &serde_json::Value, strings: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                if name.to_ascii_lowercase().contains("password") {
                    collect_nonempty_json_strings(value, strings);
                } else {
                    collect_password_fields(value, strings);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_password_fields(value, strings);
            }
        }
        serde_json::Value::String(text) => collect_xml_password_attributes(text, strings),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Collects the values of XML attributes whose name contains `password`, as
/// a Dell Server Configuration Profile import buffer carries them, both as
/// written and with their XML escapes undone.
fn collect_xml_password_attributes(text: &str, strings: &mut Vec<String>) {
    const OPEN: &str = "<Attribute Name=\"";
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        rest = &rest[start + OPEN.len()..];
        let Some((name, after_name)) = rest.split_once("\">") else {
            return;
        };
        let Some((value, after_value)) = after_name.split_once("</Attribute>") else {
            return;
        };
        rest = after_value;
        if value.is_empty() || !name.to_ascii_lowercase().contains("password") {
            continue;
        }
        strings.push(value.to_string());
        let unescaped = value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        if unescaped != value {
            strings.push(unescaped);
        }
    }
}

fn collect_nonempty_json_strings(value: &serde_json::Value, strings: &mut Vec<String>) {
    match value {
        serde_json::Value::String(value) if !value.is_empty() => strings.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_nonempty_json_strings(value, strings);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_nonempty_json_strings(value, strings);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

impl Bmc for RedfishBmc {
    type Error = BmcError;

    // Time logical fetches here so cache waits and decoding remain in the
    // application trace while HTTP connection drivers stay detached.
    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "expand", resource_uri = %id)
    )]
    async fn expand<T: Expandable>(
        &self,
        id: &ODataId,
        query: ExpandQuery,
    ) -> Result<Arc<T>, Self::Error> {
        let result = self.inner.expand(id, query).await;
        self.finish("expand", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "get", resource_uri = %id)
    )]
    async fn get<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
    ) -> Result<Arc<T>, Self::Error> {
        let result = self.inner.get(id).await;
        self.finish("get", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "get_repaired", resource_uri = %id)
    )]
    async fn get_repaired<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
        repair: &impl Repair,
    ) -> Result<Arc<T>, RepairError<Self::Error>> {
        let result = self.inner.get_repaired(id, repair).await;
        self.finish_repaired("get", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "expand_repaired", resource_uri = %id)
    )]
    async fn expand_repaired<T: Expandable>(
        &self,
        id: &ODataId,
        query: ExpandQuery,
        repair: &impl Repair,
    ) -> Result<Arc<T>, RepairError<Self::Error>> {
        let result = self.inner.expand_repaired(id, query, repair).await;
        self.finish_repaired("expand", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "filter_repaired", resource_uri = %id)
    )]
    async fn filter_repaired<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
        query: FilterQuery,
        repair: &impl Repair,
    ) -> Result<Arc<T>, RepairError<Self::Error>> {
        let result = self.inner.filter_repaired(id, query, repair).await;
        self.finish_repaired("filter", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "poll", resource_uri = %id)
    )]
    async fn poll<R: Send + Sync + for<'de> Deserialize<'de>>(
        &self,
        id: &ODataId,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        let result = self.inner.poll(id).await;
        self.finish("poll", result)
    }

    #[tracing::instrument(
        name = "redfish_call",
        skip_all,
        fields(backend = NV_REDFISH_BACKEND, operation = "filter", resource_uri = %id)
    )]
    async fn filter<T: EntityTypeRef + for<'de> Deserialize<'de> + 'static>(
        &self,
        id: &ODataId,
        query: FilterQuery,
    ) -> Result<Arc<T>, Self::Error> {
        let result = self.inner.filter(id, query).await;
        self.finish("filter", result)
    }

    async fn create<V: Send + Sync + Serialize, R: Send + Sync + for<'de> Deserialize<'de>>(
        &self,
        id: &ODataId,
        query: &V,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        let result = self.inner.create(id, query).await;
        self.finish_request("create", query, result)
    }

    async fn create_session<
        V: Send + Sync + Serialize,
        R: Send + Sync + for<'de> Deserialize<'de>,
    >(
        &self,
        id: &ODataId,
        query: &V,
    ) -> Result<SessionCreateResponse<R>, Self::Error> {
        let result = self.inner.create_session(id, query).await;
        self.finish_session("create_session", query, result)
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
        let result = self.inner.update(id, etag, update).await;
        self.finish_request("update", update, result)
    }

    async fn delete<R: EntityTypeRef + for<'de> Deserialize<'de>>(
        &self,
        id: &ODataId,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        let result = self.inner.delete(id).await;
        self.finish("delete", result)
    }

    async fn action<
        T: Send + Sync + Serialize,
        R: Send + Sync + Sized + for<'de> Deserialize<'de>,
    >(
        &self,
        action: &Action<T, R>,
        params: &T,
    ) -> Result<ModificationResponse<R>, Self::Error> {
        let result = self.inner.action(action, params).await;
        self.finish_request("action", params, result)
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
        let result = self.inner.multipart_update(uri, request).await;
        self.finish("multipart_update", result)
    }

    async fn http_push_uri_update<U, R>(
        &self,
        uri: &str,
        request: HttpPushUriUpdateRequest<U>,
    ) -> Result<ModificationResponse<R>, Self::Error>
    where
        U: UploadReader,
        R: Send + Sync + for<'de> Deserialize<'de>,
    {
        let result = self.inner.http_push_uri_update(uri, request).await;
        self.finish("http_push_uri_update", result)
    }

    async fn stream<T: Sized + for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        uri: &str,
    ) -> Result<BoxTryStream<T, Self::Error>, Self::Error> {
        let result = self.inner.stream(uri).await;
        self.finish("stream", result)
    }
}

pub type ServiceRoot = NvServiceRoot<RedfishBmc>;
pub type Error = NvError<RedfishBmc>;

/// An authenticated transport and the service root created from that exact transport.
///
/// Keeping these together lets operation drivers use typed `Bmc` requests for
/// OEM resources without creating a second connection or carrying credentials.
///
/// This pair exists only because nv-redfish's `ServiceRoot<B>` keeps its
/// transport private. Once nv-redfish exposes a `bmc()` accessor on
/// `ServiceRoot` (it already owns an `NvBmc<B>` wrapping `Arc<B>`), the pool
/// can go back to caching `Arc<ServiceRoot>` alone, this type and
/// `connection_with_bmc_credentials` can be deleted, and
/// `bmc_runtime::ConnectedBmc` can hold just the root. That is an upstream
/// change to be agreed with the nv-redfish maintainers.
#[derive(Clone)]
pub struct NvRedfishConnection {
    /// The authenticated transport.
    pub bmc: Arc<RedfishBmc>,
    /// The service root fetched through `bmc`.
    pub service_root: Arc<ServiceRoot>,
}

/// Service roots are refreshed hourly so long-running processes eventually
/// observe BMC replacements, upgrades, and configuration changes.
const DEFAULT_SERVICE_ROOT_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

pub fn new_pool(proxy_address: Arc<ArcSwap<Option<HostPortPair>>>) -> Arc<NvRedfishClientPool> {
    NvRedfishClientPool::new(proxy_address).into()
}

/// A pool whose every client targets nico-bmc-proxy over mTLS: the proxy
/// resolves the BMC from the `Forwarded` header (reusing this pool's
/// existing redirect machinery) and authenticates upstream itself, so
/// callers pass `None` for credentials -- the proxy strips `Authorization`
/// regardless. Certificates are re-read from disk at most once per
/// [`MUTUAL_CLIENT_REBUILD_INTERVAL`], off the request path (see
/// [`NvRedfishClientPool::refresh_mutual_client`]), so a rotated SPIFFE
/// certificate is picked up within that interval.
pub fn new_proxied_pool(
    proxy: HostPortPair,
    client_cert: impl Into<std::path::PathBuf>,
    client_key: impl Into<std::path::PathBuf>,
    root_ca: impl Into<std::path::PathBuf>,
) -> Arc<NvRedfishClientPool> {
    Arc::new(NvRedfishClientPool {
        proxy_address: Arc::new(ArcSwap::from_pointee(Some(proxy))),
        cache: Default::default(),
        cache_ttl: DEFAULT_SERVICE_ROOT_CACHE_TTL,
        client_tls: NvClientTls::Mutual {
            client_cert: client_cert.into(),
            client_key: client_key.into(),
            root_ca: root_ca.into(),
            rebuild_claimed_at: Mutex::new(None),
            cached: ArcSwapOption::empty(),
        },
    })
}

/// How this pool's HTTP clients authenticate the TLS layer.
enum NvClientTls {
    /// Direct-to-BMC: BMCs present self-signed certificates, so
    /// verification is off (the pre-existing behavior).
    AcceptInvalid,
    /// To nico-bmc-proxy: present the client identity its mTLS listener
    /// expects and verify its certificate against `root_ca`. Paths are
    /// re-read at most once per [`MUTUAL_CLIENT_REBUILD_INTERVAL`], so
    /// certificate rotation is picked up within that interval.
    Mutual {
        client_cert: std::path::PathBuf,
        client_key: std::path::PathBuf,
        root_ca: std::path::PathBuf,
        /// Start of the current rebuild interval, claimed before a rebuild
        /// runs (see [`NvRedfishClientPool::refresh_mutual_client`]).
        rebuild_claimed_at: Mutex<Option<Instant>>,
        /// The built mTLS client, rebuilt from disk at most every
        /// [`MUTUAL_CLIENT_REBUILD_INTERVAL`] off the request path, so
        /// certificate rotation is picked up without a per-fetch blocking
        /// file read.
        cached: ArcSwapOption<libredfish::reqwest::Client>,
    },
}

/// How often the proxied pool re-reads its certificates; the same cadence
/// nico-api's other proxy-facing clients use.
const MUTUAL_CLIENT_REBUILD_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Reads the three PEM files and builds the verified mTLS client. Blocking
/// (file I/O): call from `spawn_blocking` on the async path.
fn build_mutual_client(
    client_cert: &std::path::Path,
    client_key: &std::path::Path,
    root_ca: &std::path::Path,
) -> Result<libredfish::reqwest::Client, Error> {
    let read = |what: &str, path: &std::path::Path| {
        std::fs::read(path).map_err(|err| {
            Error::Bmc(BmcError::InvalidRequest(format!(
                "reading bmc-proxy {what} {}: {err}",
                path.display()
            )))
        })
    };
    // Newline-separate the two PEM files: a file without a trailing newline
    // would otherwise fuse END and BEGIN lines.
    let mut identity_pem = read("client_cert", client_cert)?;
    identity_pem.push(b'\n');
    identity_pem.extend_from_slice(&read("client_key", client_key)?);
    let identity = libredfish::reqwest::Identity::from_pem(&identity_pem).map_err(|err| {
        Error::Bmc(BmcError::InvalidRequest(format!(
            "building bmc-proxy client identity: {err}"
        )))
    })?;
    let roots = libredfish::reqwest::Certificate::from_pem_bundle(&read("root_ca", root_ca)?)
        .map_err(|err| {
            Error::Bmc(BmcError::InvalidRequest(format!(
                "reading bmc-proxy root_ca bundle: {err}"
            )))
        })?;
    // `with_client` bypasses the same-origin redirect guard `with_params`
    // installs, so replicate it: following a cross-origin redirect would
    // present the SPIFFE client certificate and custom headers to an
    // arbitrary host.
    let redirect_policy = libredfish::reqwest::redirect::Policy::custom(move |attempt| {
        let Some(original_url) = attempt.previous().first() else {
            return attempt.error("redirect attempt is missing the original URL");
        };
        if attempt.url().origin() != original_url.origin() {
            return attempt.error("cross-origin redirects are not allowed");
        }
        if attempt.previous().len() > 10 {
            return attempt.error("too many redirects");
        }
        attempt.follow()
    });
    let mut builder = libredfish::reqwest::Client::builder()
        .use_rustls_tls()
        .identity(identity)
        .redirect(redirect_policy)
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(5));
    for root in roots {
        builder = builder.add_root_certificate(root);
    }
    builder.build().map_err(|err| {
        Error::Bmc(BmcError::InvalidRequest(format!(
            "building bmc-proxy HTTP client: {err}"
        )))
    })
}

pub struct NvRedfishClientPool {
    proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
    cache: Arc<Mutex<ServiceRootCache>>,
    cache_ttl: Duration,
    client_tls: NvClientTls,
}

#[derive(Default)]
struct ServiceRootCache {
    roots: HashMap<PoolKey, CachedServiceRoot>,
    expirations: BinaryHeap<Reverse<CacheExpiration>>,
    next_generation: u64,
}

impl ServiceRootCache {
    fn allocate_generation(&mut self) -> u64 {
        if self.next_generation == u64::MAX {
            self.roots.clear();
            self.expirations.clear();
            self.next_generation = 0;
        }

        let generation = self.next_generation;
        self.next_generation += 1;
        generation
    }

    fn invalidate_bmc(&mut self, bmc_address: SocketAddr) {
        self.roots.retain(|key, _| key.bmc_address != bmc_address);
        self.expirations
            .retain(|expiration| expiration.0.key.bmc_address != bmc_address);
    }
}

struct CachedServiceRoot {
    connection: NvRedfishConnection,
    generation: u64,
}

struct CacheExpiration {
    expires_at: Instant,
    generation: u64,
    key: PoolKey,
}

impl PartialEq for CacheExpiration {
    fn eq(&self, other: &Self) -> bool {
        self.expires_at == other.expires_at && self.generation == other.generation
    }
}

impl Eq for CacheExpiration {}

impl PartialOrd for CacheExpiration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CacheExpiration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.expires_at
            .cmp(&other.expires_at)
            .then_with(|| self.generation.cmp(&other.generation))
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    proxy_address: Arc<Option<HostPortPair>>,
    bmc_address: SocketAddr,
    credentials: BmcCredentials,
}

impl NvRedfishClientPool {
    pub fn new(proxy_address: Arc<ArcSwap<Option<HostPortPair>>>) -> Self {
        Self::with_cache_ttl(proxy_address, DEFAULT_SERVICE_ROOT_CACHE_TTL)
    }

    /// Creates a client pool with an explicit service-root cache lifetime.
    ///
    /// This is primarily useful for tests that need deterministic expiration
    /// without sleeping.
    pub fn with_cache_ttl(
        proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            proxy_address,
            cache: Default::default(),
            cache_ttl,
            client_tls: NvClientTls::AcceptInvalid,
        }
    }

    /// The BMC's service root, from cache when fresh.
    ///
    /// `credentials` is `None` only on the proxied pool, where
    /// nico-bmc-proxy authenticates upstream itself; a direct pool given
    /// `None` fails rather than dialing the BMC anonymously.
    pub async fn service_root(
        &self,
        bmc_address: SocketAddr,
        credentials: Option<Credentials>,
    ) -> Result<Arc<ServiceRoot>, Error> {
        self.service_root_with_cache_predicate(bmc_address, credentials, |_| true)
            .await
    }

    /// Same as [`Self::service_root`], but a freshly fetched root is cached
    /// only when `should_cache` returns true for it.
    #[tracing::instrument(
        name = "nv_redfish_service_root",
        skip_all,
        fields(bmc_address = %bmc_address, cache_hit = tracing::field::Empty)
    )]
    pub async fn service_root_with_cache_predicate(
        &self,
        bmc_address: SocketAddr,
        credentials: Option<Credentials>,
        should_cache: impl FnOnce(&ServiceRoot) -> bool,
    ) -> Result<Arc<ServiceRoot>, Error> {
        let bmc_credentials = self.bmc_credentials(credentials)?;
        self.connection_with_cache_predicate(bmc_address, bmc_credentials, should_cache)
            .await
            .map(|connection| connection.service_root)
    }

    /// Returns an authenticated BMC transport and its service root.
    ///
    /// The credentials, including the complete token or username/password
    /// material, are part of the cache key.
    pub async fn connection_with_bmc_credentials(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
    ) -> Result<NvRedfishConnection, Error> {
        self.connection_with_cache_predicate(bmc_address, credentials, |_| true)
            .await
    }

    async fn connection_with_cache_predicate(
        &self,
        bmc_address: SocketAddr,
        bmc_credentials: BmcCredentials,
        should_cache: impl FnOnce(&ServiceRoot) -> bool,
    ) -> Result<NvRedfishConnection, Error> {
        self.remove_expired(Instant::now());
        self.refresh_mutual_client().await?;

        let key = PoolKey {
            proxy_address: self.proxy_address.load_full(),
            bmc_address,
            credentials: bmc_credentials.clone(),
        };
        if let Some(cached) = self.cached_connection(&key) {
            tracing::Span::current().record("cache_hit", true);
            Ok(cached)
        } else {
            tracing::Span::current().record("cache_hit", false);
            let mut bmc = self.create_bmc(bmc_address, bmc_credentials.clone(), false)?;
            // A factory BMC that refuses the credentials until the password
            // changes still serves its service root anonymously; the
            // connection keeps the credentials so the account PATCH can be
            // sent, and is not cached past the change.
            let (service_root, cacheable) = match ServiceRoot::new(bmc.clone()).await {
                Err(error) if requires_password_change(&error) => {
                    let anonymous =
                        self.create_bmc(bmc_address, BmcCredentials::anonymous(), false)?;
                    let root = ServiceRoot::new(anonymous).await?;
                    (root.replace_bmc(bmc.clone()), false)
                }
                result => (result?, true),
            };
            let connection_close = service_root.vendor()
                == Some(nv_redfish::service_root::Vendor::new("HPE"))
                && service_root
                    .oem_hpe_ilo_service_ext()
                    .ok()
                    .as_ref()
                    .and_then(|v| v.as_ref())
                    .and_then(|v| v.manager_type())
                    .is_some_and(
                        |manager| matches!(manager, HpeManagerType::Ilo(version) if version < 7),
                    );
            let cache_responses = !keeps_etags_across_changes(&service_root);
            // Handle HPE BMC that closing connection right after
            // response. In this case, we add Connection: Close
            // HTTP header to prevent trying to reuse this
            // connection. Otherwise, race condition may happen
            // when reqwest thinks that connection is alive but it
            // is about to close by server. Reusing such
            // connections causes errors.
            let service_root = if connection_close || !cache_responses {
                bmc = self.create_bmc_with_response_cache(
                    bmc_address,
                    bmc_credentials,
                    connection_close,
                    cache_responses,
                )?;
                service_root.replace_bmc(bmc.clone())
            } else {
                service_root
            };
            let connection = NvRedfishConnection {
                bmc,
                service_root: Arc::new(service_root),
            };
            // nv-redfish picks its quirks from the root's vendor, which a BMC
            // still starting can omit; caching that root would keep the wrong
            // quirks. Power shelves never report a vendor, so theirs is read
            // on every connection.
            if cacheable
                && connection.service_root.vendor().is_some()
                && should_cache(&connection.service_root)
            {
                self.update_cache(key, connection.clone());
            }
            Ok(connection)
        }
    }

    /// The credential nv-redfish's client type carries for one BMC. `None`
    /// is valid only on the proxied pool, which sends none: nico-bmc-proxy
    /// authenticates upstream.
    fn bmc_credentials(&self, credentials: Option<Credentials>) -> Result<BmcCredentials, Error> {
        match credentials {
            Some(Credentials::UsernamePassword { username, password }) => {
                Ok(BmcCredentials::new(username, password))
            }
            None if matches!(self.client_tls, NvClientTls::Mutual { .. }) => {
                Ok(BmcCredentials::anonymous())
            }
            None => Err(Error::Bmc(BmcError::InvalidRequest(
                "BMC credentials are required for a direct (non-proxied) connection".to_string(),
            ))),
        }
    }

    fn cached_connection(&self, key: &PoolKey) -> Option<NvRedfishConnection> {
        self.cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned")
            .roots
            .get(key)
            .map(|entry| entry.connection.clone())
    }

    fn update_cache(&self, key: PoolKey, connection: NvRedfishConnection) {
        let mut cache = self
            .cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned");
        let expires_at = Instant::now() + self.cache_ttl;
        let generation = cache.allocate_generation();
        cache.roots.insert(
            key.clone(),
            CachedServiceRoot {
                connection,
                generation,
            },
        );
        cache.expirations.push(Reverse(CacheExpiration {
            expires_at,
            generation,
            key,
        }));
    }

    fn remove_expired(&self, now: Instant) {
        let mut cache = self
            .cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned");

        while cache
            .expirations
            .peek()
            .is_some_and(|expiration| expiration.0.expires_at <= now)
        {
            let Some(Reverse(expiration)) = cache.expirations.pop() else {
                break;
            };
            if cache
                .roots
                .get(&expiration.key)
                .is_some_and(|entry| entry.generation == expiration.generation)
            {
                cache.roots.remove(&expiration.key);
            }
        }
    }

    /// For the proxied pool: (re)build the mTLS client when none is cached
    /// or the current rebuild interval ([`MUTUAL_CLIENT_REBUILD_INTERVAL`])
    /// has elapsed. The interval is claimed before the rebuild runs, so
    /// concurrent callers past it do not each spawn one and a persistently
    /// failing rebuild is retried once per interval rather than on every
    /// request. The file reads run on the blocking pool so a hung secret
    /// mount cannot stall request threads; a failed rebuild keeps serving
    /// the previous client and surfaces the error only when there is none
    /// yet.
    async fn refresh_mutual_client(&self) -> Result<(), Error> {
        let NvClientTls::Mutual {
            client_cert,
            client_key,
            root_ca,
            rebuild_claimed_at,
            cached,
        } = &self.client_tls
        else {
            return Ok(());
        };
        {
            let mut claimed = rebuild_claimed_at
                .lock()
                .expect("nv-redfish mutual client rebuild claim mutex poisoned");
            if cached.load().is_some()
                && claimed.is_some_and(|at| at.elapsed() < MUTUAL_CLIENT_REBUILD_INTERVAL)
            {
                return Ok(());
            }
            *claimed = Some(Instant::now());
        }
        let (client_cert, client_key, root_ca) =
            (client_cert.clone(), client_key.clone(), root_ca.clone());
        let built = tokio::task::spawn_blocking(move || {
            build_mutual_client(&client_cert, &client_key, &root_ca)
        })
        .await
        .map_err(|err| {
            Error::Bmc(BmcError::InvalidRequest(format!(
                "bmc-proxy client rebuild task failed: {err}"
            )))
        })?;
        match built {
            Ok(client) => {
                cached.store(Some(Arc::new(client)));
                Ok(())
            }
            // Keep serving the previous client through a transient read
            // failure; only a pool that has never built one must fail.
            Err(err) if cached.load().is_some() => {
                tracing::warn!(error = %err, "bmc-proxy nv-redfish client rebuild failed; keeping the previous client");
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Evicts every cached service root for `bmc_address`.
    ///
    /// All credential and proxy variants for the address are removed.
    pub fn invalidate_service_roots_for_bmc(&self, bmc_address: SocketAddr) {
        self.cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned")
            .invalidate_bmc(bmc_address);
    }

    pub fn create_bmc(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
        connection_close: bool,
    ) -> Result<Arc<RedfishBmc>, Error> {
        self.create_bmc_with_response_cache(bmc_address, credentials, connection_close, true)
    }

    fn create_bmc_with_response_cache(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
        connection_close: bool,
        cache_responses: bool,
    ) -> Result<Arc<RedfishBmc>, Error> {
        // Mirrors the libredfish proxied pool: the Forwarded header carries
        // only the BMC's host and nico-bmc-proxy dials the https default, so
        // silently dropping a recorded non-443 Redfish port would strand
        // that BMC -- fail loudly instead.
        if matches!(self.client_tls, NvClientTls::Mutual { .. }) && bmc_address.port() != 443 {
            return Err(Error::Bmc(BmcError::InvalidRequest(format!(
                "BMC {} uses Redfish port {}, which bmc-proxy cannot address; \
                 route it via the direct pool or standard port 443",
                bmc_address.ip(),
                bmc_address.port()
            ))));
        }
        let proxy_address = self.proxy_address.load();
        let bmc_url = build_bmc_url(proxy_address.as_ref(), bmc_address)
            .map_err(|e| Error::Bmc(BmcError::InvalidRequest(format!("invalid BMC URL: {e}"))))?;

        let mut headers = HeaderMap::new();
        if proxy_address.is_some() {
            headers.insert(
                reqwest::header::FORWARDED,
                format_forwarded_host_parameter(&bmc_address.ip().to_string())
                    .parse()
                    .expect("Generated header is expected to be valid"),
            );
        }
        if connection_close {
            headers.insert(
                reqwest::header::CONNECTION,
                reqwest::header::HeaderValue::from_static("Close"),
            );
        }

        let client = match &self.client_tls {
            NvClientTls::AcceptInvalid => RedfishReqwestClient::with_params(
                RedfishReqwestClientParams::new().accept_invalid_certs(true),
            )
            .map_err(|err| Error::Bmc(err.into()))?,
            NvClientTls::Mutual {
                client_cert,
                client_key,
                root_ca,
                cached,
                ..
            } => {
                let client = match cached.load_full() {
                    Some(current) => (*current).clone(),
                    // Direct callers that skipped `refresh_mutual_client`
                    // still get a working client, at the cost of a
                    // blocking build here.
                    None => build_mutual_client(client_cert, client_key, root_ca)?,
                };
                RedfishReqwestClient::with_client(client)
            }
        };
        let sensitive_values = sensitive_values(&credentials);
        let bmc = InnerRedfishBmc::with_custom_headers(
            SpanIsolatedHttpClient::new(client),
            bmc_url,
            credentials,
            CacheSettings::with_capacity(if cache_responses { 10 } else { 0 }),
            headers,
        );
        Ok(Arc::new(RedfishBmc::new(bmc, sensitive_values)))
    }
}

/// Lenovo and AMI firmware keep a resource's ETag when the resource changes,
/// so a copy revalidated by ETag can be stale; their transports read every
/// resource afresh.
fn keeps_etags_across_changes(service_root: &ServiceRoot) -> bool {
    [
        nv_redfish::service_root::Vendor::new("Lenovo"),
        nv_redfish::service_root::Vendor::new("AMI"),
    ]
    .into_iter()
    .any(|vendor| service_root.vendor() == Some(vendor))
}

/// Whether the BMC refused a request with `PasswordChangeRequired`, as a
/// factory BMC does for every credentialed request until the password changes.
fn requires_password_change(error: &Error) -> bool {
    matches!(
        error,
        Error::Bmc(BmcError::InvalidResponse { status, text, .. })
            if *status == reqwest::StatusCode::FORBIDDEN && text.contains("PasswordChangeRequired")
    )
}

/// Builds the BMC base URL, applying any configured proxy override.
///
/// Mirrors `health::BmcAddr::to_url()`: IPv6 hosts are bracketed so the URL
/// authority parses — a bare `IpAddr` Display leaves IPv6 unbracketed
/// (e.g. `2001:db8::1`), which `Url::parse` rejects.
fn build_bmc_url(
    proxy_address: &Option<HostPortPair>,
    bmc_address: SocketAddr,
) -> Result<Url, url::ParseError> {
    // Bracket the BMC's own IP if IPv6; IPv4 renders unchanged.
    let bmc_host = match bmc_address.ip() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let (host, port) = match proxy_address {
        // No override: the BMC's own IP and port.
        None => (bmc_host, bmc_address.port()),
        // An operator-supplied override may replace the host, the port, or
        // both; `url_host()` brackets an IPv6 literal proxy host.
        Some(proxy) => (
            proxy.url_host().map_or(bmc_host, Cow::into_owned),
            proxy.port().unwrap_or_else(|| bmc_address.port()),
        ),
    };
    let mut url = Url::parse(&format!("https://{host}"))?;
    let _ = url.set_port(Some(port));
    Ok(url)
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedFieldKind, capture_logs};

    use super::*;

    fn pool_key(
        proxy_address: Option<HostPortPair>,
        bmc_address: &str,
        credentials: BmcCredentials,
    ) -> PoolKey {
        PoolKey {
            proxy_address: Arc::new(proxy_address),
            bmc_address: bmc_address.parse().unwrap(),
            credentials,
        }
    }

    #[test]
    fn bmc_invalidation_removes_matching_expirations() {
        let target_address = "127.0.0.1:443";
        let other_address = "127.0.0.2:443";
        let expirations = [
            pool_key(
                None,
                target_address,
                BmcCredentials::token("direct-token".to_string()),
            ),
            pool_key(
                Some(HostPortPair::PortOnly(8443)),
                target_address,
                BmcCredentials::token("proxied-token".to_string()),
            ),
            pool_key(
                None,
                other_address,
                BmcCredentials::token("other-token".to_string()),
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(generation, key)| {
            Reverse(CacheExpiration {
                expires_at: Instant::now(),
                generation: generation as u64,
                key,
            })
        })
        .collect();
        let mut cache = ServiceRootCache {
            expirations,
            ..Default::default()
        };

        cache.invalidate_bmc(target_address.parse().unwrap());

        assert_eq!(cache.expirations.len(), 1);
        assert_eq!(
            cache.expirations.peek().unwrap().0.key.bmc_address,
            other_address.parse().unwrap(),
        );
    }

    fn test_bmc(credentials: BmcCredentials) -> RedfishBmc {
        let sensitive_values = sensitive_values(&credentials);
        RedfishBmc::new(
            InnerRedfishBmc::with_custom_headers(
                SpanIsolatedHttpClient::new(
                    RedfishReqwestClient::with_params(
                        RedfishReqwestClientParams::new().accept_invalid_certs(true),
                    )
                    .expect("test HTTP client"),
                ),
                Url::parse("https://bmc.example").expect("valid test BMC URL"),
                credentials,
                CacheSettings::with_capacity(1),
                HeaderMap::new(),
            ),
            sensitive_values,
        )
    }

    #[tokio::test]
    async fn task_poll_preserves_pending_responses_and_redacts_http_failures() {
        use axum::Router;
        use axum::http::StatusCode;
        use axum::routing::get;

        let app = Router::new()
            .route("/pending", get(|| async { StatusCode::ACCEPTED }))
            .route(
                "/failed",
                get(|| async {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "credential secret rejected",
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let credentials = BmcCredentials::new("root".to_string(), "secret".to_string());
        let sensitive_values = sensitive_values(&credentials);

        let bmc = RedfishBmc::new(
            InnerRedfishBmc::with_custom_headers(
                SpanIsolatedHttpClient::new(RedfishReqwestClient::new().unwrap()),
                Url::parse(&format!("http://{address}")).unwrap(),
                credentials,
                CacheSettings::with_capacity(1),
                HeaderMap::new(),
            ),
            sensitive_values,
        );

        let pending = bmc
            .poll::<()>(&ODataId::from("/pending".to_string()))
            .await
            .unwrap();

        let ModificationResponse::Task(task) = pending else {
            panic!("202 must preserve the pending task");
        };

        assert_eq!(task.location.0.to_string(), "/pending");

        let error = bmc
            .poll::<()>(&ODataId::from("/failed".to_string()))
            .await
            .unwrap_err();

        let BmcError::InvalidResponse { status, text, .. } = error else {
            panic!("HTTP failure must preserve its status and sanitized body");
        };

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!text.contains("secret"));
        assert!(text.contains("rejected"));

        server.abort();

        assert!(server.await.unwrap_err().is_cancelled());
    }

    /// Verifies nv-redfish removes plaintext, full Basic header, and bare
    /// payload forms from its returned error and structured diagnostic.
    #[test]
    fn http_failure_logs_structured_context_and_masks_credentials() {
        // Build the client with Basic credentials and derive each reusable
        // representation that a hostile or broken BMC may echo.
        let (basic_authorization, sensitive_values) =
            redfish_basic_authorization_context("root", Some("secret"));
        let basic_payload = &sensitive_values[1];
        let credentials = BmcCredentials::new("root".to_string(), "secret".to_string());
        let bmc = test_bmc(credentials);
        let error = BmcError::InvalidResponse {
            url: Url::parse("https://bmc.example/redfish/v1/Systems/1").expect("valid test URL"),
            status: http::StatusCode::INTERNAL_SERVER_ERROR,
            text: format!(
                r#"{{
                "error": {{
                    "@Message.ExtendedInfo": [{{
                        "Message": "credential s\u0065cret or {basic_authorization} or BASIC {basic_payload} rejected"
                    }}]
                }}
            }}"#,
            ),
        };

        // Finish the operation through the same decorator used in production.
        let (error, logs) = {
            let mut result = None;
            let logs = capture_logs(|| {
                result = Some(bmc.finish::<()>("get", Err(error)));
            });
            (
                result
                    .expect("captured result")
                    .expect_err("HTTP failure remains an error"),
                logs,
            )
        };

        // Neither returned nor logged text may retain either credential form.
        let log = logs.first().expect("one nv-redfish failure log");
        assert_eq!(logs.len(), 1);
        assert_eq!(log.field("backend"), Some(NV_REDFISH_BACKEND));
        assert_eq!(log.field("operation"), Some("get"));
        assert_eq!(
            log.field("url"),
            Some("https://bmc.example/redfish/v1/Systems/1")
        );
        assert_eq!(log.field("http_status"), Some("500"));
        assert_eq!(log.field_kind("http_status"), Some(CapturedFieldKind::U64));
        assert_eq!(
            log.field("error"),
            Some("credential REDACTED or REDACTED or BASIC REDACTED rejected")
        );

        let BmcError::InvalidResponse { text, .. } = error else {
            panic!("HTTP error remains an HTTP error after redaction");
        };
        let response: serde_json::Value =
            serde_json::from_str(&text).expect("redacted body remains valid JSON");
        assert_eq!(
            response["error"]["@Message.ExtendedInfo"][0]["Message"],
            "credential REDACTED or REDACTED or BASIC REDACTED rejected"
        );
    }

    #[test]
    fn session_failure_redacts_client_and_payload_credentials_before_logging_and_returning() {
        let bmc = test_bmc(BmcCredentials::Token {
            token: "client-token".to_string(),
        });
        let query = serde_json::json!({
            "UserName": "session-user",
            "Password": "session-secret",
        });
        let error = BmcError::InvalidResponse {
            url: Url::parse("https://bmc.example/redfish/v1/SessionService/Sessions")
                .expect("valid test URL"),
            status: http::StatusCode::UNAUTHORIZED,
            text: r#"{"error":{"message":"client-token rejected s\u0065ssion-secret for session-user"}}"#
                .to_string(),
        };

        let mut result: Option<Result<(), BmcError>> = None;
        let logs = capture_logs(|| {
            result = Some(bmc.finish_session("create_session", &query, Err(error)));
        });
        let error = result
            .expect("captured result")
            .expect_err("HTTP failure remains an error");

        let log = logs.first().expect("one nv-redfish failure log");
        assert_eq!(logs.len(), 1);
        assert_eq!(log.field("operation"), Some("create_session"));
        assert_eq!(
            log.field("error"),
            Some("REDACTED rejected REDACTED for REDACTED")
        );

        let BmcError::InvalidResponse { text, .. } = error else {
            panic!("HTTP error remains an HTTP error after redaction");
        };
        let response: serde_json::Value =
            serde_json::from_str(&text).expect("redacted body remains valid JSON");
        assert_eq!(
            response["error"]["message"],
            "REDACTED rejected REDACTED for REDACTED"
        );
    }

    /// A write's password fields, at any depth, are masked in the logged and
    /// returned error; its other fields stay readable.
    #[test]
    fn write_failure_redacts_request_password_fields_before_logging_and_returning() {
        let bmc = test_bmc(BmcCredentials::Token {
            token: "client-token".to_string(),
        });
        let request = serde_json::json!({
            "UserName": "operator",
            "Password": "account-secret",
            "Attributes": { "AdministratorPassword_00B7": "uefi-secret" },
        });
        let error = BmcError::InvalidResponse {
            url: Url::parse("https://bmc.example/redfish/v1/AccountService/Accounts/3")
                .expect("valid test URL"),
            status: http::StatusCode::BAD_REQUEST,
            text: r#"{"error":{"message":"operator: account-secret and uefi-secret rejected"}}"#
                .to_string(),
        };

        let mut result: Option<Result<(), BmcError>> = None;
        let logs = capture_logs(|| {
            result = Some(bmc.finish_request("update", &request, Err(error)));
        });
        let error = result
            .expect("captured result")
            .expect_err("HTTP failure remains an error");

        let log = logs.first().expect("one nv-redfish failure log");
        assert_eq!(logs.len(), 1);
        assert_eq!(log.field("operation"), Some("update"));
        assert_eq!(
            log.field("error"),
            Some("operator: REDACTED and REDACTED rejected")
        );
        let BmcError::InvalidResponse { text, .. } = error else {
            panic!("HTTP error remains an HTTP error after redaction");
        };
        let response: serde_json::Value =
            serde_json::from_str(&text).expect("redacted body remains valid JSON");
        assert_eq!(
            response["error"]["message"],
            "operator: REDACTED and REDACTED rejected"
        );
    }

    #[test]
    fn generation_overflow_clears_expirations_and_restarts_from_zero() {
        let key = PoolKey {
            proxy_address: Arc::new(None),
            bmc_address: "127.0.0.1:443".parse().unwrap(),
            credentials: BmcCredentials::new("root".to_string(), "password".to_string()),
        };
        let mut cache = ServiceRootCache {
            expirations: BinaryHeap::from([Reverse(CacheExpiration {
                expires_at: Instant::now(),
                generation: u64::MAX - 1,
                key,
            })]),
            next_generation: u64::MAX,
            ..Default::default()
        };

        assert_eq!(cache.allocate_generation(), 0);
        assert!(cache.roots.is_empty());
        assert!(cache.expirations.is_empty());
        assert_eq!(cache.next_generation, 1);
    }

    fn sock(s: &str) -> SocketAddr {
        s.parse().expect("valid socket addr")
    }

    // Regression: an IPv6 BMC behind a port-only proxy must yield a bracketed
    // authority. Pre-fix the manual format produced `https://2001:db8::1:8443`,
    // which `Url::parse` rejects — and `create_bmc` `.expect()`s the parse, so it
    // panicked.
    #[test]
    fn port_only_proxy_brackets_ipv6_bmc() {
        let url = build_bmc_url(
            &Some(HostPortPair::PortOnly(8443)),
            sock("[2001:db8::1]:443"),
        )
        .expect("url should build");
        assert_eq!(url.host_str(), Some("[2001:db8::1]"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.as_str(), "https://[2001:db8::1]:8443/");
    }

    // IPv4 BMCs keep their unbracketed authority.
    #[test]
    fn port_only_proxy_leaves_ipv4_unchanged() {
        let url = build_bmc_url(&Some(HostPortPair::PortOnly(8443)), sock("10.0.0.5:443"))
            .expect("url should build");
        assert_eq!(url.host_str(), Some("10.0.0.5"));
        assert_eq!(url.port(), Some(8443));
    }

    // No proxy: the BMC's own IP and port form the authority; IPv6 is bracketed.
    // 443 is the https default, so the url crate canonicalizes it out of the
    // explicit port (as it always did when the old string was parsed).
    #[test]
    fn no_proxy_brackets_ipv6_bmc() {
        let url = build_bmc_url(&None, sock("[2001:db8::1]:443")).expect("url should build");
        assert_eq!(url.host_str(), Some("[2001:db8::1]"));
        assert_eq!(url.port_or_known_default(), Some(443));
        assert_eq!(url.as_str(), "https://[2001:db8::1]/");
    }

    // A proxy host supplied as a bare IPv6 literal is bracketed too.
    #[test]
    fn proxy_host_ipv6_literal_is_bracketed() {
        let host_only = build_bmc_url(
            &Some(HostPortPair::HostOnly("2001:db8::2".to_string())),
            sock("10.0.0.5:443"),
        )
        .expect("url should build");
        assert_eq!(host_only.host_str(), Some("[2001:db8::2]"));
        assert_eq!(host_only.port_or_known_default(), Some(443));

        let host_and_port = build_bmc_url(
            &Some(HostPortPair::HostAndPort("2001:db8::2".to_string(), 8443)),
            sock("10.0.0.5:443"),
        )
        .expect("url should build");
        assert_eq!(host_and_port.host_str(), Some("[2001:db8::2]"));
        assert_eq!(host_and_port.port(), Some(8443));
    }

    // A hostname proxy is passed through untouched.
    #[test]
    fn proxy_hostname_unchanged() {
        let url = build_bmc_url(
            &Some(HostPortPair::HostAndPort(
                "bmc-proxy.example".to_string(),
                8443,
            )),
            sock("10.0.0.5:443"),
        )
        .expect("url should build");
        assert_eq!(url.host_str(), Some("bmc-proxy.example"));
        assert_eq!(url.port(), Some(8443));
    }

    // Regression (#3008 review): the `Forwarded` `host` parameter that
    // `create_bmc` sends must bracket an IPv6 BMC per RFC 7239; IPv4 and
    // hostnames keep the bare token form, and the result must be a valid
    // header value (create_bmc `.expect()`s the parse).
    #[test]
    fn forwarded_host_parameter_brackets_ipv6_bmc() {
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        let v4: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(
            format_forwarded_host_parameter(&v6.to_string()),
            "host=\"[2001:db8::1]\""
        );
        assert_eq!(
            format_forwarded_host_parameter(&v4.to_string()),
            "host=10.0.0.5"
        );
        for ip in [v6, v4] {
            reqwest::header::HeaderValue::from_str(&format_forwarded_host_parameter(
                &ip.to_string(),
            ))
            .expect("valid Forwarded header value");
        }
    }

    // --- proxied (Mutual mTLS) pool ----------------------------------------

    fn proxied_pool_with_generated_pems(dir: &tempfile::TempDir) -> Arc<NvRedfishClientPool> {
        let key = rcgen::KeyPair::generate().expect("test key generates");
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key)
            .expect("test cert self-signs");
        let write = |name: &str, contents: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, contents).expect("test PEM writes");
            path
        };
        new_proxied_pool(
            HostPortPair::HostAndPort("bmc-proxy.example".to_string(), 1079),
            write("tls.crt", &cert.pem()),
            write("tls.key", &key.serialize_pem()),
            write("ca.crt", &cert.pem()),
        )
    }

    /// The Mutual client build path: valid PEMs produce a client (the whole
    /// identity/root/redirect construction succeeds), a missing file fails
    /// naming its path, and a non-443 BMC port is rejected loudly -- the
    /// proxy dials the https default, so dropping the port would silently
    /// strand that BMC (mirrors the libredfish proxied pool's guard).
    #[test]
    fn proxied_pool_builds_clients_from_pems_and_rejects_non_standard_ports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = proxied_pool_with_generated_pems(&dir);
        let bmc: SocketAddr = "192.0.2.10:443".parse().unwrap();
        let credentials = BmcCredentials::anonymous();

        pool.create_bmc(bmc, credentials.clone(), false)
            .expect("valid PEMs must build a proxied client");

        let odd_port: SocketAddr = "192.0.2.10:8443".parse().unwrap();
        let Err(err) = pool.create_bmc(odd_port, credentials.clone(), false) else {
            panic!("a non-443 BMC port must fail loudly, not drop the port");
        };
        assert!(
            err.to_string().contains("8443"),
            "the error should name the unaddressable port: {err}"
        );

        std::fs::remove_file(dir.path().join("tls.key")).expect("remove key");
        let Err(err) = pool.create_bmc(bmc, credentials, false) else {
            panic!("a missing PEM must fail the client build");
        };
        assert!(
            err.to_string().contains("tls.key"),
            "the error should name the unreadable file: {err}"
        );
    }

    /// A direct pool cannot dial a BMC anonymously: `None` credentials are
    /// only meaningful where nico-bmc-proxy authenticates upstream.
    #[tokio::test]
    async fn direct_pool_rejects_absent_credentials() {
        let pool = NvRedfishClientPool::new(Arc::new(ArcSwap::from_pointee(None)));
        let bmc: SocketAddr = "192.0.2.10:443".parse().unwrap();
        let Err(err) = pool.service_root(bmc, None).await else {
            panic!("a direct pool must not dial a BMC without credentials");
        };
        assert!(
            err.to_string().contains("credentials"),
            "the error should name the missing credentials: {err}"
        );
    }

    /// Serves `app` over HTTPS with a self-signed certificate on a free
    /// local port.
    async fn serve_tls(
        app: axum::Router,
    ) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .expect("test cert self-signs");
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            certified.cert.pem().into_bytes(),
            certified.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .expect("test TLS config");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(
            axum_server::from_tcp_rustls(listener, tls)
                .expect("test TLS server")
                .serve(app.into_make_service()),
        );
        (address, server)
    }

    /// Lenovo and AMI BMCs keep a resource's ETag across changes, so their
    /// transports read every resource afresh instead of accepting a stale
    /// 304; other BMCs still revalidate.
    #[tokio::test]
    async fn lenovo_and_ami_transports_never_revalidate_reads() {
        use axum::Router;
        use axum::http::{StatusCode, header};
        use axum::response::IntoResponse;
        use axum::routing::get;
        use serde_json::json;

        for (vendor, expected) in [
            ("NVIDIA", [false, true]),
            ("Lenovo", [false, false]),
            ("AMI", [false, false]),
        ] {
            let revalidations = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new()
                .route(
                    "/redfish/v1",
                    get(move || async move {
                        axum::Json(json!({
                            "@odata.id": "/redfish/v1", "Id": "RootService",
                            "Name": "Root Service", "Vendor": vendor,
                            "Chassis": {"@odata.id": "/redfish/v1/Chassis"},
                            "Links": {"Sessions": {
                                "@odata.id": "/redfish/v1/SessionService/Sessions",
                            }},
                        }))
                    }),
                )
                .route(
                    "/redfish/v1/Chassis",
                    get({
                        let revalidations = revalidations.clone();
                        move |headers: axum::http::HeaderMap| async move {
                            let revalidated = headers.contains_key(header::IF_NONE_MATCH);
                            revalidations.lock().unwrap().push(revalidated);
                            if revalidated {
                                StatusCode::NOT_MODIFIED.into_response()
                            } else {
                                (
                                    [(header::ETAG, "\"1\"")],
                                    axum::Json(json!({
                                        "@odata.id": "/redfish/v1/Chassis",
                                        "@odata.type": "#ChassisCollection.ChassisCollection",
                                        "@odata.etag": "\"1\"",
                                        "Name": "Chassis Collection", "Members": [],
                                    })),
                                )
                                    .into_response()
                            }
                        }
                    }),
                );
            let (address, server) = serve_tls(app).await;

            let pool = NvRedfishClientPool::new(Arc::new(ArcSwap::from_pointee(None)));
            let connection = pool
                .connection_with_bmc_credentials(
                    address,
                    BmcCredentials::new("root".to_string(), "password".to_string()),
                )
                .await
                .expect("the BMC connects");
            for _ in 0..2 {
                connection
                    .service_root
                    .chassis()
                    .await
                    .expect("the chassis collection reads");
            }
            assert_eq!(
                std::mem::take(&mut *revalidations.lock().unwrap()),
                expected,
                "{vendor}"
            );
            server.abort();
        }
    }

    /// A password inside a Server Configuration Profile import buffer is
    /// masked like a password field, as written and unescaped; other
    /// attributes and empty passwords are not.
    #[test]
    fn import_buffer_passwords_are_collected_for_masking() {
        let request = serde_json::json!({
            "ImportBuffer": concat!(
                r#"<SystemConfiguration><Component FQDD="BIOS.Setup.1-1">"#,
                r#"<Attribute Name="OldSetupPassword">a&amp;b</Attribute>"#,
                r#"<Attribute Name="NewSetupPassword"></Attribute>"#,
                r#"<Attribute Name="BootMode">Uefi</Attribute>"#,
                r#"</Component></SystemConfiguration>"#,
            ),
        });
        let mut strings = Vec::new();
        collect_password_fields(&request, &mut strings);
        assert_eq!(strings, ["a&amp;b", "a&b"]);
    }

    /// A factory BMC refusing its credentials until the password changes
    /// still connects: the service root is read without an `Authorization`
    /// header, which such a BMC refuses even when empty, later requests carry
    /// the credentials, and the connection is not cached.
    #[tokio::test]
    async fn password_change_required_reads_the_service_root_anonymously() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::Router;
        use axum::http::{StatusCode, header};
        use axum::response::IntoResponse;
        use axum::routing::get;
        use serde_json::json;

        let (factory, _) = redfish_basic_authorization_context("admin", Some("admin"));
        let factory = Arc::new(factory);
        let refused_roots = Arc::new(AtomicUsize::new(0));
        let is_factory = {
            let factory = factory.clone();
            move |headers: &axum::http::HeaderMap| {
                headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some(factory.as_str())
            }
        };
        let app = Router::new()
            .route(
                "/redfish/v1",
                get({
                    let is_factory = is_factory.clone();
                    let refused_roots = refused_roots.clone();
                    move |headers: axum::http::HeaderMap| async move {
                        if is_factory(&headers) {
                            refused_roots.fetch_add(1, Ordering::SeqCst);
                            let refused = json!({"error": {
                                "code": "Base.1.12.PasswordChangeRequired",
                                "@Message.ExtendedInfo": [{
                                    "MessageId": "Base.1.12.PasswordChangeRequired",
                                    "MessageArgs": ["/redfish/v1/AccountService/Accounts/4"],
                                }],
                            }});
                            (StatusCode::FORBIDDEN, axum::Json(refused)).into_response()
                        } else if headers.contains_key(header::AUTHORIZATION) {
                            StatusCode::UNAUTHORIZED.into_response()
                        } else {
                            axum::Json(json!({
                                "@odata.id": "/redfish/v1", "Id": "RootService",
                                "Name": "Root Service", "Vendor": "Lenovo",
                                "Chassis": {"@odata.id": "/redfish/v1/Chassis"},
                                "Links": {"Sessions": {
                                    "@odata.id": "/redfish/v1/SessionService/Sessions",
                                }},
                            }))
                            .into_response()
                        }
                    }
                }),
            )
            .route(
                "/redfish/v1/Chassis",
                get(move |headers: axum::http::HeaderMap| async move {
                    if is_factory(&headers) {
                        axum::Json(json!({
                            "@odata.id": "/redfish/v1/Chassis",
                            "@odata.type": "#ChassisCollection.ChassisCollection",
                            "Name": "Chassis Collection", "Members": [],
                        }))
                        .into_response()
                    } else {
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                }),
            );
        let (address, server) = serve_tls(app).await;

        let pool = NvRedfishClientPool::new(Arc::new(ArcSwap::from_pointee(None)));
        let credentials = BmcCredentials::new("admin".to_string(), "admin".to_string());
        for attempt in 1..=2 {
            let connection = pool
                .connection_with_bmc_credentials(address, credentials.clone())
                .await
                .expect("a factory BMC connects");
            assert_eq!(
                connection.service_root.vendor(),
                Some(nv_redfish::service_root::Vendor::new("Lenovo"))
            );
            assert!(
                connection
                    .service_root
                    .chassis()
                    .await
                    .expect("requests after the root carry the credentials")
                    .is_some()
            );
            assert_eq!(refused_roots.load(Ordering::SeqCst), attempt);
        }
        server.abort();
    }

    /// A root without a vendor, as a BMC still starting can serve, is not
    /// cached: the next connection reads the root again, and the complete
    /// root it then gets is cached.
    #[tokio::test]
    async fn a_root_without_a_vendor_is_not_cached() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::Router;
        use axum::routing::get;
        use serde_json::json;

        let roots = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/redfish/v1",
            get({
                let roots = roots.clone();
                move || async move {
                    let mut root = json!({
                        "@odata.id": "/redfish/v1", "Id": "RootService", "Name": "Root Service",
                        "Links": {"Sessions": {
                            "@odata.id": "/redfish/v1/SessionService/Sessions",
                        }},
                    });
                    if roots.fetch_add(1, Ordering::SeqCst) > 0 {
                        root["Vendor"] = json!("NVIDIA");
                    }
                    axum::Json(root)
                }
            }),
        );
        let (address, server) = serve_tls(app).await;

        let pool = NvRedfishClientPool::new(Arc::new(ArcSwap::from_pointee(None)));
        let credentials = BmcCredentials::new("root".to_string(), "password".to_string());
        let mut vendors = Vec::new();
        for _ in 0..3 {
            let connection = pool
                .connection_with_bmc_credentials(address, credentials.clone())
                .await
                .expect("the BMC connects");
            vendors.push(
                connection
                    .service_root
                    .vendor()
                    .map(|vendor| vendor.into_inner().to_string()),
            );
        }
        assert_eq!(
            vendors,
            [None, Some("NVIDIA".to_string()), Some("NVIDIA".to_string())]
        );
        assert_eq!(roots.load(Ordering::SeqCst), 2);
        server.abort();
    }

    /// The rebuild cadence: within the interval the cached client is served
    /// without touching disk, a failed rebuild past it keeps the previous
    /// client and claims the interval so the failure is not retried on the
    /// next call, and a rebuild past the interval picks up the PEMs on disk.
    #[tokio::test]
    async fn proxied_pool_rebuilds_once_per_interval_and_keeps_the_previous_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = proxied_pool_with_generated_pems(&dir);
        let NvClientTls::Mutual {
            rebuild_claimed_at,
            cached,
            ..
        } = &pool.client_tls
        else {
            panic!("the proxied pool authenticates with a client certificate");
        };
        // An unclaimed interval is an elapsed one.
        let expire_interval = || *rebuild_claimed_at.lock().unwrap() = None;
        let current = || cached.load_full().expect("a client is cached");

        pool.refresh_mutual_client()
            .await
            .expect("the first refresh builds a client");
        let first = current();

        let key = dir.path().join("tls.key");
        let key_pem = std::fs::read(&key).expect("read key");
        std::fs::remove_file(&key).expect("remove key");
        pool.refresh_mutual_client()
            .await
            .expect("within the interval the cached client is served without a disk read");
        assert!(Arc::ptr_eq(&first, &current()));

        expire_interval();
        pool.refresh_mutual_client()
            .await
            .expect("a failed rebuild keeps serving the previous client");
        assert!(Arc::ptr_eq(&first, &current()));
        std::fs::write(&key, key_pem).expect("restore key");
        pool.refresh_mutual_client()
            .await
            .expect("still within the claimed interval");
        assert!(
            Arc::ptr_eq(&first, &current()),
            "a failed rebuild must not be retried before the interval elapses"
        );

        expire_interval();
        pool.refresh_mutual_client()
            .await
            .expect("past the interval the restored PEMs rebuild the client");
        assert!(
            !Arc::ptr_eq(&first, &current()),
            "a rebuild past the interval must pick up the PEMs on disk"
        );
    }
}
