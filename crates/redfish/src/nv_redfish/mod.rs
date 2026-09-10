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
use carbide_utils::redfish::format_forwarded_host_parameter;
pub use nv_redfish::bmc_http::reqwest::BmcError;
use nv_redfish::bmc_http::reqwest::{
    Client as RedfishReqwestClient, ClientParams as RedfishReqwestClientParams,
};
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use nv_redfish::oem::hpe::ilo_service_ext::ManagerType as HpeManagerType;
use nv_redfish::{Error as NvError, ServiceRoot as NvServiceRoot};
use reqwest::header::HeaderMap;
use url::Url;

pub type RedfishBmc = HttpBmc<RedfishReqwestClient>;
pub type ServiceRoot = NvServiceRoot<RedfishBmc>;
pub type Error = NvError<RedfishBmc>;

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
}

struct CachedServiceRoot {
    root: Arc<ServiceRoot>,
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
    pub async fn service_root_with_cache_predicate(
        &self,
        bmc_address: SocketAddr,
        credentials: Option<Credentials>,
        should_cache: impl FnOnce(&ServiceRoot) -> bool,
    ) -> Result<Arc<ServiceRoot>, Error> {
        let bmc_credentials = self.bmc_credentials(credentials)?;
        self.remove_expired(Instant::now());
        self.refresh_mutual_client().await?;

        if let Some(sevice_root) = self.cached_root(bmc_address, bmc_credentials.clone()) {
            Ok(sevice_root)
        } else {
            let bmc = self.create_bmc(bmc_address, bmc_credentials.clone(), false)?;
            let service_root = ServiceRoot::new(bmc).await?;
            let service_root = if service_root.vendor()
                == Some(nv_redfish::service_root::Vendor::new("HPE"))
                && let Some(HpeManagerType::Ilo(version)) = service_root
                    .oem_hpe_ilo_service_ext()
                    .ok()
                    .as_ref()
                    .and_then(|v| v.as_ref())
                    .and_then(|v| v.manager_type())
                && version < 7
            {
                // Handle HPE BMC that closing connection right after
                // response. In this case, we add Connection: Close
                // HTTP header to prevent trying to reuse this
                // connection. Otherwise, race condition may happen
                // when reqwest thinks that connection is alive but it
                // is about to close by server. Reusing such
                // connections causes errors.
                let bmc = self.create_bmc(bmc_address, bmc_credentials.clone(), true)?;
                service_root.replace_bmc(bmc.clone())
            } else {
                service_root
            };
            let service_root = Arc::new(service_root);
            if should_cache(&service_root) {
                self.update_cache(bmc_address, bmc_credentials, service_root.clone());
            }
            Ok(service_root)
        }
    }

    /// The credential nv-redfish's client type carries for one BMC. `None`
    /// is valid only on the proxied pool: nico-bmc-proxy authenticates
    /// upstream and strips any `Authorization` header, so an empty
    /// credential stands in for the type's sake and nothing else.
    fn bmc_credentials(&self, credentials: Option<Credentials>) -> Result<BmcCredentials, Error> {
        match credentials {
            Some(Credentials::UsernamePassword { username, password }) => {
                Ok(BmcCredentials::new(username, password))
            }
            None if matches!(self.client_tls, NvClientTls::Mutual { .. }) => {
                Ok(BmcCredentials::new(String::new(), String::new()))
            }
            None => Err(Error::Bmc(BmcError::InvalidRequest(
                "BMC credentials are required for a direct (non-proxied) connection".to_string(),
            ))),
        }
    }

    fn cached_root(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
    ) -> Option<Arc<ServiceRoot>> {
        let proxy_address = self.proxy_address.load();
        let key = PoolKey {
            proxy_address: proxy_address.clone(),
            bmc_address,
            credentials,
        };
        self.cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned")
            .roots
            .get(&key)
            .map(|entry| entry.root.clone())
    }

    fn update_cache(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
        root: Arc<ServiceRoot>,
    ) {
        let proxy_address = self.proxy_address.load();
        let key = PoolKey {
            proxy_address: proxy_address.clone(),
            bmc_address,
            credentials,
        };
        let mut cache = self
            .cache
            .lock()
            .expect("nv-redfish client cache mutex poisoned");
        let expires_at = Instant::now() + self.cache_ttl;
        let generation = cache.allocate_generation();
        cache
            .roots
            .insert(key.clone(), CachedServiceRoot { root, generation });
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

    pub fn create_bmc(
        &self,
        bmc_address: SocketAddr,
        credentials: BmcCredentials,
        connection_close: bool,
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
        Ok(Arc::new(RedfishBmc::with_custom_headers(
            client,
            bmc_url,
            credentials,
            CacheSettings::with_capacity(10),
            headers,
        )))
    }
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
    use super::*;

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
        let credentials = BmcCredentials::new(String::new(), String::new());

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
