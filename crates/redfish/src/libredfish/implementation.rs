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
use std::net::Ipv6Addr;
use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use carbide_secrets::credentials::{CredentialReader, Credentials};
use carbide_utils::HostPortPair;
use carbide_utils::redfish::{
    format_forwarded_host_parameter, redfish_basic_authorization_context,
};
use libredfish::model::service_root::RedfishVendor;
use libredfish::{Endpoint, Redfish};
use tracing::Instrument;

use crate::libredfish::instrumented::{InstrumentedRedfish, instrumented_redfish_initialization};
use crate::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};

/// Formats a host for the URL authority that `libredfish` constructs internally.
///
/// `libredfish::Endpoint` accepts a host and port separately, but the pinned
/// implementation interpolates them into a URL string. Keep callers' host values
/// bare and add IPv6 brackets only at this external serialization boundary.
fn libredfish_endpoint_host(host: &str) -> Cow<'_, str> {
    if host.parse::<Ipv6Addr>().is_ok() {
        Cow::Owned(format!("[{host}]"))
    } else {
        Cow::Borrowed(host)
    }
}

pub(super) struct RedfishClientPoolImpl {
    instrumentation: carbide_instrument::Instrumentation,
    pool: libredfish::RedfishClientPool,
    credential_reader: Arc<dyn CredentialReader>,
    proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
}

impl RedfishClientPoolImpl {
    pub(super) fn new(
        credential_reader: Arc<dyn CredentialReader>,
        pool: libredfish::RedfishClientPool,
        proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Self {
        RedfishClientPoolImpl {
            instrumentation,
            credential_reader,
            pool,
            proxy_address,
        }
    }
}

impl RedfishClientPoolImpl {
    /// Build a client from the supplied pool, preserving its transport retry policy.
    async fn create_client_with_pool(
        &self,
        pool: &libredfish::RedfishClientPool,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        let original_host = host;

        // Allow globally overriding the bmc port via site-config. We read this on every call to
        // create_client, because self.proxy_address is a dynamic setting.
        let proxy_address = self.proxy_address.load();
        let (host, port, add_custom_header) = match proxy_address.as_ref() {
            // No override
            None => (host, port, false),
            // Override the host and port
            Some(HostPortPair::HostAndPort(h, p)) => (h.as_str(), Some(*p), true),
            // Only override the host
            Some(HostPortPair::HostOnly(h)) => (h.as_str(), port, true),
            // Only override the port
            Some(HostPortPair::PortOnly(p)) => (host, Some(*p), false),
        };

        let (username, password) = match auth {
            RedfishAuth::Anonymous => (None, None), // anonymous login, usually to get service root Vendor info
            RedfishAuth::Direct(username, password) => (Some(username), Some(password)),
            RedfishAuth::Key(credential_key) => {
                let credentials = self
                    .credential_reader
                    .get_credentials(&credential_key)
                    .instrument(tracing::info_span!("redfish_credentials"))
                    .await?
                    .ok_or_else(|| RedfishClientCreationError::MissingCredentials {
                        key: credential_key.to_key_str().to_string(),
                    })?;

                let (username, password) = match credentials {
                    Credentials::UsernamePassword { username, password } => {
                        (Some(username), Some(password))
                    }
                };

                (username, password)
            }
        };
        // Keep every secret representation libredfish places on the wire as
        // redaction context for initialization and later operation failures.
        let authentication_sensitive_values = if let Some(username) = username.as_deref() {
            let (_, sensitive_values) =
                redfish_basic_authorization_context(username, password.as_deref());
            sensitive_values
        } else {
            password
                .as_deref()
                .filter(|password| !password.is_empty())
                .map(str::to_string)
                .into_iter()
                .collect()
        };

        let endpoint = Endpoint {
            host: libredfish_endpoint_host(host).into_owned(),
            port,
            user: username,
            password,
        };

        let custom_headers = if add_custom_header {
            // If we're overriding the host, inject a header indicating the IP address we were
            // originally going to use, using the HTTP "Forwarded" header:
            // https://datatracker.ietf.org/doc/html/rfc7239

            // Override host only if host value is provided in config.
            vec![(
                http::HeaderName::from_str("forwarded")
                    .map_err(|err| RedfishClientCreationError::InvalidHeader(err.to_string()))?,
                format_forwarded_host_parameter(original_host),
            )]
        } else {
            Vec::default()
        };

        // The initializing paths below make HTTP calls of their own, so they
        // are metered like any other Redfish operation.
        let client = match vendor {
            // Auto-detect vendor from the service root.
            None => instrumented_redfish_initialization(
                &self.instrumentation,
                "create_client",
                authentication_sensitive_values.iter().map(String::as_str),
                pool.create_client_with_custom_headers(endpoint, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
            // Unknown means "no vendor" — return a standard client without
            // making any HTTP calls (used by the anonymous probe client).
            // This restores the behavior of the old `initialize: false` path
            // which called create_standard_client. The full initialization
            // path (create_client_with_vendor) makes HTTP calls to /Systems,
            // /Managers, etc. that fail with 401 on BMCs requiring auth.
            // With no I/O here, there is no external call to meter either.
            Some(RedfishVendor::Unknown) => pool
                .create_standard_client_with_custom_headers(endpoint, custom_headers)
                .map_err(RedfishClientCreationError::RedfishError)
                .map(|c| c as Box<dyn Redfish>)?,
            // Use the provided vendor directly.
            Some(vendor) => instrumented_redfish_initialization(
                &self.instrumentation,
                "create_client",
                authentication_sensitive_values.iter().map(String::as_str),
                pool.create_client_with_vendor(endpoint, vendor, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
        };

        // Every client the pool creates goes out decorated, so each Redfish
        // call records the per-operation RED triad no matter the call site.
        Ok(Box::new(InstrumentedRedfish::new(
            client,
            authentication_sensitive_values,
            self.instrumentation.clone(),
        )))
    }
}

#[async_trait]
impl RedfishClientPool for RedfishClientPoolImpl {
    #[tracing::instrument(name = "redfish_create_client", skip_all, fields(bmc_host = host))]
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.create_client_with_pool(&self.pool, host, port, auth, vendor)
            .await
    }

    #[tracing::instrument(name = "redfish_create_client", skip_all, fields(bmc_host = host))]
    async fn create_client_without_mutation_retries(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        let pool = self.pool.without_mutation_retries();
        self.create_client_with_pool(&pool, host, port, auth, vendor)
            .await
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        &*self.credential_reader
    }
}

impl super::sealed::Sealed for RedfishClientPoolImpl {}

#[async_trait]
impl super::BmcCredentialOps for RedfishClientPoolImpl {}

/// A [`RedfishClientPool`] whose every client targets nico-bmc-proxy instead
/// of the BMC itself.
///
/// The proxy resolves the BMC from the RFC 7239 `Forwarded` header, fetches
/// credentials from nico-api, and authenticates upstream -- so clients from
/// this pool carry no BMC credentials at all. [`RedfishAuth::Key`] is
/// accepted and ignored (the key names credentials the proxy will resolve
/// itself), while [`RedfishAuth::Direct`] is rejected: explicit credentials
/// mean the caller is doing credential-subject work, which must never route
/// through the proxy, whose header stripping would silently discard them.
///
/// This pool deliberately implements only [`RedfishClientPool`], never
/// [`super::BmcCredentialOps`] (which is sealed): handing it
/// credential-lifecycle work is a compile error, so the `Direct` rejection
/// above is the only credential path left to guard at runtime.
pub(super) struct ProxiedRedfishClientPoolImpl {
    instrumentation: carbide_instrument::Instrumentation,
    pool: libredfish::RedfishClientPool,
    credential_reader: Arc<dyn CredentialReader>,
    proxy: HostPortPair,
}

impl ProxiedRedfishClientPoolImpl {
    pub(super) fn new(
        credential_reader: Arc<dyn CredentialReader>,
        pool: libredfish::RedfishClientPool,
        proxy: HostPortPair,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Self {
        ProxiedRedfishClientPoolImpl {
            instrumentation,
            credential_reader,
            pool,
            proxy,
        }
    }
}

#[async_trait]
impl RedfishClientPool for ProxiedRedfishClientPoolImpl {
    #[tracing::instrument(name = "redfish_create_client", skip_all, fields(bmc_host = host))]
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.create_client_with_pool(&self.pool, host, port, auth, vendor)
            .await
    }

    async fn create_client_without_mutation_retries(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        let pool = self.pool.without_mutation_retries();
        self.create_client_with_pool(&pool, host, port, auth, vendor)
            .await
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        &*self.credential_reader
    }
}

impl ProxiedRedfishClientPoolImpl {
    /// Preserve proxy routing and authentication while selecting the caller's retry policy.
    async fn create_client_with_pool(
        &self,
        pool: &libredfish::RedfishClientPool,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        if matches!(auth, RedfishAuth::Direct(..)) {
            return Err(RedfishClientCreationError::Unsupported(
                "explicit credentials cannot be forwarded through bmc-proxy; \
                 credential-subject operations must use the direct pool"
                    .to_string(),
            ));
        }

        // The Forwarded header carries only the BMC's host; bmc-proxy dials
        // it at the https default. Silently dropping a recorded non-443
        // Redfish port would strand that BMC, so fail loudly instead.
        if let Some(port) = port
            && port != 443
        {
            return Err(RedfishClientCreationError::Unsupported(format!(
                "BMC {host} uses Redfish port {port}, which bmc-proxy cannot \
                 address; route it via the direct pool or standard port 443"
            )));
        }

        let endpoint = Endpoint {
            host: self
                .proxy
                .url_host()
                .map(Cow::into_owned)
                .unwrap_or_else(|| host.to_string()),
            port: self.proxy.port().or(port),
            // An empty username keeps the client off libredfish's anonymous
            // path (which changes vendor detection) while sending a
            // credential the proxy strips anyway.
            user: Some(String::new()),
            password: None,
        };

        let custom_headers = vec![(
            http::HeaderName::from_str("forwarded")
                .map_err(|err| RedfishClientCreationError::InvalidHeader(err.to_string()))?,
            format_forwarded_host_parameter(host),
        )];

        let client = match vendor {
            None => instrumented_redfish_initialization(
                &self.instrumentation,
                "create_client",
                [],
                pool.create_client_with_custom_headers(endpoint, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
            Some(RedfishVendor::Unknown) => pool
                .create_standard_client_with_custom_headers(endpoint, custom_headers)
                .map_err(RedfishClientCreationError::RedfishError)
                .map(|c| c as Box<dyn Redfish>)?,
            Some(vendor) => instrumented_redfish_initialization(
                &self.instrumentation,
                "create_client",
                [],
                pool.create_client_with_vendor(endpoint, vendor, custom_headers),
            )
            .await
            .map_err(RedfishClientCreationError::RedfishError)?,
        };

        Ok(Box::new(InstrumentedRedfish::new(
            client,
            Vec::new(),
            self.instrumentation.clone(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::value_scenarios;
    use carbide_utils::HostPortPair;

    use super::super::RedfishAuth;
    use super::{ProxiedRedfishClientPoolImpl, libredfish_endpoint_host};
    use crate::libredfish::{RedfishClientPool as _, RedfishVendor};

    /// The direct and proxied adapters preserve one failed mutation attempt;
    /// its ordinary pool remains a positive control for the existing retry policy.
    #[tokio::test]
    async fn mutation_retry_policy_reaches_the_actual_http_client() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        use axum::body::{Body, Bytes};
        use axum::extract::State;
        use axum::http::Response;

        async fn reset(State(calls): State<Arc<AtomicUsize>>) -> Response<Body> {
            // The POST reaches this route, but the immediate body error prevents
            // HTTP/1.1 headers from flushing. The caller receives no response.
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Response::builder()
                    .header("content-length", "10")
                    .body(Body::from_stream(futures_util::stream::once(async {
                        Err::<Bytes, _>(std::io::Error::other("lost reset response"))
                    })))
                    .unwrap()
            } else {
                Response::new(Body::from("{}"))
            }
        }

        for (name, proxied, no_retry, expected) in [
            ("durable mutation", false, true, 1),
            ("ordinary mutation", false, false, 2),
            ("proxied durable mutation", true, true, 1),
        ] {
            tokio::time::timeout(Duration::from_secs(10), async {
                let calls = Arc::new(AtomicUsize::new(0));
                let router = axum::Router::new()
                    .fallback(axum::routing::post(reset)).with_state(calls.clone());
                let (mut server, url) = bmc_mock::test_support::serve_https("reset-reply-fault", router);
                let transport = libredfish::RedfishClientPool::builder()
                    .danger_accept_invalid_certs().build().unwrap();
                let credentials = Arc::new(TestCredentialManager::default());
                let pool: Box<dyn crate::libredfish::RedfishClientPool> = if proxied {
                    Box::new(super::ProxiedRedfishClientPoolImpl::new(
                        credentials, transport,
                        HostPortPair::HostAndPort("127.0.0.1".to_string(), url.port().unwrap()),
                        carbide_instrument::Instrumentation::noop(),
                    ))
                } else {
                    Box::new(super::RedfishClientPoolImpl::new(
                        credentials, transport, Arc::new(arc_swap::ArcSwap::from_pointee(None)),
                        carbide_instrument::Instrumentation::noop(),
                    ))
                };
                let port = if proxied { Some(443) } else { url.port() };
                // Unknown selects the standard client without discovery for this mutation-policy proof.
                let client = if no_retry {
                    pool.create_client_without_mutation_retries("127.0.0.1", port, RedfishAuth::Anonymous, Some(RedfishVendor::Unknown)).await
                } else {
                    pool.create_client("127.0.0.1", port, RedfishAuth::Anonymous, Some(RedfishVendor::Unknown)).await
                }.unwrap();
                let result = client.power(libredfish::SystemPowerControl::ForceRestart).await;
                if no_retry {
                    assert!(matches!(result, Err(libredfish::RedfishError::NetworkError { source, .. }) if !source.is_timeout()), "{name}: the failed response must remain uncertain");
                } else {
                    result.unwrap();
                }
                assert_eq!(calls.load(Ordering::SeqCst), expected, "{name}");
                server.stop().await.unwrap();
            }).await.unwrap_or_else(|_| panic!("{name}: reset proof exceeded its watchdog"));
        }
    }

    fn proxied_pool() -> ProxiedRedfishClientPoolImpl {
        ProxiedRedfishClientPoolImpl::new(
            Arc::new(TestCredentialManager::default()),
            libredfish::RedfishClientPool::builder()
                .build()
                .expect("test pool builds"),
            HostPortPair::HostAndPort("bmc-proxy.example".to_string(), 1079),
            carbide_instrument::Instrumentation::noop(),
        )
    }

    // The safety invariant of the proxied/direct pool split: explicit
    // credentials mean credential-subject work, which must never route
    // through the proxy -- its header stripping would silently discard them.
    #[tokio::test]
    async fn proxied_pool_rejects_explicit_credentials() {
        let result = proxied_pool()
            .create_client(
                "192.0.2.10",
                None,
                RedfishAuth::Direct("root".to_string(), "password".to_string()),
                Some(RedfishVendor::Unknown),
            )
            .await;

        // `Box<dyn Redfish>` has no `Debug`, so unwrap the error by hand.
        let Err(err) = result else {
            panic!("Direct credentials must not be forwardable");
        };
        assert!(
            err.to_string().contains("bmc-proxy"),
            "error should name the misrouting: {err}"
        );
    }

    // bmc-proxy dials the BMC at the https default -- a recorded non-443
    // Redfish port would be silently dropped, so it must fail loudly.
    #[tokio::test]
    async fn proxied_pool_rejects_non_standard_bmc_ports() {
        let result = proxied_pool()
            .create_client(
                "192.0.2.10",
                Some(8443),
                RedfishAuth::Anonymous,
                Some(RedfishVendor::Unknown),
            )
            .await;

        let Err(err) = result else {
            panic!("a non-443 BMC port must not be silently dropped");
        };
        assert!(
            err.to_string().contains("8443"),
            "error should name the unaddressable port: {err}"
        );
    }

    // A `Key` names credentials the proxy resolves itself; the client must
    // target the proxy, not the BMC, and stay off libredfish's anonymous
    // path (which changes vendor detection).
    #[tokio::test]
    async fn proxied_pool_targets_the_proxy_without_bmc_credentials() {
        let client = proxied_pool()
            .create_client(
                "192.0.2.10",
                Some(443),
                RedfishAuth::Key(
                    carbide_secrets::credentials::CredentialKey::BmcCredentials {
                        credential_type: carbide_secrets::credentials::BmcCredentialType::BmcRoot {
                            bmc_mac_address: mac_address::MacAddress::new([2, 0, 0, 0, 0, 1]),
                        },
                    },
                ),
                // `Unknown` builds a standard client without any HTTP calls.
                Some(RedfishVendor::Unknown),
            )
            .await
            .expect("client builds without resolving credentials");

        let http = &client.std_redfish().client;
        assert_eq!(http.host(), "bmc-proxy.example");
        assert!(
            !http.is_anonymous(),
            "an anonymous endpoint changes libredfish's vendor-detection path"
        );
    }

    #[test]
    fn endpoint_host_brackets_only_bare_ipv6_literals() {
        value_scenarios!(run = |host| libredfish_endpoint_host(host).into_owned();
            "unchanged hosts" {
                "bmc.example.com" => "bmc.example.com".to_string(),
                "192.0.2.10" => "192.0.2.10".to_string(),
                "[2001:db8::10]" => "[2001:db8::10]".to_string(),
            }

            "bracketed IPv6 host" {
                "2001:db8::10" => "[2001:db8::10]".to_string(),
            }
        );
    }
}
