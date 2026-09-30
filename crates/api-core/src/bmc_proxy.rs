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

//! Clients that carry this instance's identity to nico-bmc-proxy.
//!
//! Both clients here present the SPIFFE workload certificate and verify the
//! proxy against the configured roots. Those files rotate on disk, so a
//! client is never built once for the process lifetime: each holder rebuilds
//! from disk after [`IDENTITY_REFRESH_INTERVAL`], mirroring nico-bmc-proxy's
//! own certificate reloading. A rebuild failure keeps serving the previous
//! client -- a stale certificate that still handshakes beats an outage --
//! and retries on the next interval.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};
use carbide_secrets::credentials::CredentialReader;
use carbide_utils::HostPortPair;
use libredfish::Redfish;
use libredfish::model::service_root::RedfishVendor;

use crate::cfg::file::BmcProxyConfig;

/// How long a built client may serve before its certificates are re-read
/// from disk. Matches nico-bmc-proxy's own TLS refresh interval.
const IDENTITY_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn read(what: &str, path: &str) -> eyre::Result<Vec<u8>> {
    std::fs::read(path).map_err(|err| eyre::eyre!("bmc_proxy.{what} {path:?}: {err}"))
}

/// A value rebuilt from disk after [`IDENTITY_REFRESH_INTERVAL`].
struct Reloading<T> {
    build: Arc<dyn Fn() -> eyre::Result<T> + Send + Sync>,
    state: Mutex<(Instant, T)>,
}

impl<T: Clone + Send + 'static> Reloading<T> {
    fn new(build: impl Fn() -> eyre::Result<T> + Send + Sync + 'static) -> eyre::Result<Self> {
        let initial = build()?;
        Ok(Self {
            build: Arc::new(build),
            state: Mutex::new((Instant::now(), initial)),
        })
    }

    async fn current(&self) -> T {
        // Claim the refresh under the lock, but run the rebuild -- file
        // reads and TLS construction -- on the blocking pool, so concurrent
        // callers keep serving the previous value and no tokio worker sits
        // on disk I/O (a hung secret mount must not stall request threads).
        // Claiming also means a persistently broken rebuild is retried once
        // per interval rather than on every request.
        let stale = {
            let mut state = self.state.lock().expect("reloading state mutex poisoned");
            if state.0.elapsed() < IDENTITY_REFRESH_INTERVAL {
                return state.1.clone();
            }
            state.0 = Instant::now();
            state.1.clone()
        };

        let build = self.build.clone();
        let rebuilt = tokio::task::spawn_blocking(move || build())
            .await
            .map_err(|join_error| eyre::eyre!("rebuild task panicked: {join_error}"))
            .and_then(|result| result);
        match rebuilt {
            Ok(value) => {
                self.state.lock().expect("reloading state mutex poisoned").1 = value.clone();
                value
            }
            Err(error) => {
                carbide_instrument::emit(BmcProxyClientReloadFailed {
                    error: error.to_string(),
                });
                stale
            }
        }
    }
}

/// A bmc-proxy-facing client could not be rebuilt from the certificates on
/// disk and the previous client stays in service. Persistent failures mean
/// proxied BMC traffic dies when the stale certificate expires, so this is
/// worth alerting on -- the proxy side counts the same condition.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "bmc_proxy_client_reload_failed",
    metric_name = "carbide_api_bmc_proxy_client_reload_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "rebuilding bmc-proxy client from rotated certificates failed; keeping the previous client",
    describe = "Number of failed rebuilds of the nico-bmc-proxy-facing client from on-disk certificates."
)]
struct BmcProxyClientReloadFailed {
    #[context]
    error: String,
}

/// A [`RedfishClientPool`] whose clients all target nico-bmc-proxy,
/// rebuilding the underlying HTTP pool as the SPIFFE certificates rotate.
pub(crate) struct ProxiedRedfishPool {
    inner: Reloading<Arc<dyn RedfishClientPool>>,
    credential_reader: Arc<dyn CredentialReader>,
}

impl ProxiedRedfishPool {
    pub(crate) fn new(
        proxy_config: &BmcProxyConfig,
        credential_reader: Arc<dyn CredentialReader>,
    ) -> eyre::Result<Self> {
        let proxy: HostPortPair = proxy_config
            .proxy_target()
            .map_err(|err| eyre::eyre!(err))?;
        let proxy_config = proxy_config.clone();
        let reader = credential_reader.clone();
        let inner = Reloading::new(move || {
            // The proxy presents a verifiable certificate, so unlike direct
            // BMC connections this client keeps certificate checking on.
            let pool = libredfish::RedfishClientPool::builder()
                .identity(
                    read("client_cert", &proxy_config.client_cert)?,
                    read("client_key", &proxy_config.client_key)?,
                )
                .add_root_certificates(read("root_ca", &proxy_config.root_ca)?)
                .build()
                .map_err(|err| eyre::eyre!("building bmc-proxy redfish pool: {err}"))?;
            Ok(carbide_redfish::libredfish::new_proxied_pool(
                reader.clone(),
                pool,
                proxy.clone(),
            ))
        })?;
        Ok(Self {
            inner,
            credential_reader,
        })
    }
}

#[async_trait]
impl RedfishClientPool for ProxiedRedfishPool {
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.inner
            .current()
            .await
            .create_client(host, port, auth, vendor)
            .await
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        &*self.credential_reader
    }
}

/// A redirect policy that follows at most `max` hops and never leaves the
/// original origin: following a cross-origin redirect would present the
/// SPIFFE client certificate and the `Forwarded` header to an arbitrary host.
fn same_origin_redirect_policy(max: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        let Some(original) = attempt.previous().first() else {
            return attempt.error("redirect attempt is missing the original URL");
        };
        if attempt.url().origin() != original.origin() {
            return attempt.error("cross-origin redirects are not allowed");
        }
        if attempt.previous().len() > max {
            return attempt.error("too many redirects");
        }
        attempt.follow()
    })
}

/// The raw HTTP client the Redfish passthrough handlers use to reach
/// nico-bmc-proxy, shared and rebuilt on certificate rotation instead of
/// being read from disk and rebuilt per request.
pub(crate) struct PassthroughClient {
    client: Reloading<reqwest_middleware::ClientWithMiddleware>,
    target: HostPortPair,
}

impl PassthroughClient {
    pub(crate) fn new(proxy_config: &BmcProxyConfig) -> eyre::Result<Self> {
        let target = proxy_config
            .proxy_target()
            .map_err(|err| eyre::eyre!(err))?;
        let proxy_config = proxy_config.clone();
        let client = Reloading::new(move || {
            // Newline-separate the two PEM files: a file without a trailing
            // newline would otherwise fuse its END line with the next BEGIN.
            let identity = reqwest::Identity::from_pem(
                &[
                    read("client_key", &proxy_config.client_key)?,
                    b"\n".to_vec(),
                    read("client_cert", &proxy_config.client_cert)?,
                ]
                .concat(),
            )
            .map_err(|err| eyre::eyre!("bmc_proxy client identity: {err}"))?;

            let mut builder = reqwest::Client::builder()
                .identity(identity)
                .redirect(same_origin_redirect_policy(5))
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60));
            for certificate in
                reqwest::Certificate::from_pem_bundle(&read("root_ca", &proxy_config.root_ca)?)
                    .map_err(|err| eyre::eyre!("bmc_proxy.root_ca: {err}"))?
            {
                builder = builder.add_root_certificate(certificate);
            }
            let client = builder
                .build()
                .map_err(|err| eyre::eyre!("building bmc-proxy HTTP client: {err}"))?;
            // The `reqwest-tracing` middleware injects the current span's W3C
            // trace context into every outgoing request (#2438).
            Ok(reqwest_middleware::ClientBuilder::new(client)
                .with(reqwest_tracing::TracingMiddleware::default())
                .build())
        })?;
        Ok(Self { client, target })
    }

    pub(crate) async fn client(&self) -> reqwest_middleware::ClientWithMiddleware {
        self.client.current().await
    }

    pub(crate) fn target(&self) -> &HostPortPair {
        &self.target
    }
}

/// Test-only: a valid enabled config whose PEM files are generated into
/// `dir`, so pool/passthrough construction succeeds without real SPIFFE
/// mounts on disk.
#[cfg(test)]
pub(crate) fn test_config_with_generated_pems(
    dir: &tempfile::TempDir,
) -> crate::cfg::file::BmcProxyConfig {
    let key = rcgen::KeyPair::generate().expect("test key generates");
    let cert = rcgen::CertificateParams::default()
        .self_signed(&key)
        .expect("test cert self-signs");
    let write = |name: &str, contents: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("test PEM writes");
        path.to_string_lossy().into_owned()
    };
    crate::cfg::file::BmcProxyConfig {
        enabled: true,
        address: "bmc-proxy.example:1079".to_string(),
        client_cert: write("tls.crt", &cert.pem()),
        client_key: write("tls.key", &key.serialize_pem()),
        root_ca: write("ca.crt", &cert.pem()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // The reload contract: serve cached until the interval elapses, pick up
    // a successful rebuild, and keep the previous value through a failing
    // one instead of erroring the request path.
    #[tokio::test]
    async fn reloading_serves_cached_then_rebuilds_and_survives_failures() {
        let metrics = carbide_instrument::testing::MetricsCapture::start();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let reloading = Reloading::new(|| {
            let call = CALLS.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                Err(eyre::eyre!("injected rebuild failure"))
            } else {
                Ok(call)
            }
        })
        .expect("initial build succeeds");

        assert_eq!(reloading.current().await, 0, "served from cache");
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "no rebuild before expiry");

        // Force expiry: a failing rebuild keeps the old value...
        reloading.state.lock().unwrap().0 = Instant::now() - IDENTITY_REFRESH_INTERVAL;
        assert_eq!(
            reloading.current().await,
            0,
            "failure keeps the previous value"
        );
        // ...and does not retry until the next interval.
        assert_eq!(reloading.current().await, 0);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert_eq!(
            metrics.counter_delta("carbide_api_bmc_proxy_client_reload_failures_total", &[]),
            1.0,
            "the failed rebuild must be counted"
        );

        // A successful rebuild replaces the value.
        reloading.state.lock().unwrap().0 = Instant::now() - IDENTITY_REFRESH_INTERVAL;
        assert_eq!(reloading.current().await, 2);
    }
}
