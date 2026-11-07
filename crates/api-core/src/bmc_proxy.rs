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
//! client is never built once for the process lifetime: each holder checks
//! them in the background once [`IDENTITY_REFRESH_INTERVAL`] has passed,
//! mirroring nico-bmc-proxy's own certificate reloading, and rebuilds when
//! they changed. A failed check keeps serving the previous client -- a stale
//! certificate that still handshakes beats an outage -- and is retried on
//! the next interval.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};
use carbide_secrets::credentials::CredentialReader;
use carbide_utils::HostPortPair;
use carbide_utils::reloading::{ReloadError, ReloadSchedule, Reloading};
use libredfish::Redfish;
use libredfish::model::service_root::RedfishVendor;

use crate::cfg::file::BmcProxyConfig;

/// How long a built client may serve before its certificates are checked
/// again. Matches nico-bmc-proxy's own TLS refresh interval.
const IDENTITY_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A failed check is retried on the next interval too.
const IDENTITY_RELOAD: ReloadSchedule = ReloadSchedule {
    every: IDENTITY_REFRESH_INTERVAL,
    retry: IDENTITY_REFRESH_INTERVAL,
};

/// The files a client is built from, named by the keys that give their
/// paths: its certificate, its key, and the roots that verify the proxy.
fn identity_files(proxy_config: &BmcProxyConfig) -> [(String, PathBuf); 3] {
    [
        ("bmc_proxy.client_cert", &proxy_config.client_cert),
        ("bmc_proxy.client_key", &proxy_config.client_key),
        ("bmc_proxy.root_ca", &proxy_config.root_ca),
    ]
    .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
}

/// Counts a check that could not rebuild a client.
fn report_reload_failure<T>(_: &T, failure: Option<&ReloadError>) {
    if let Some(error) = failure {
        carbide_instrument::emit(BmcProxyClientReloadFailed {
            error: error.to_string(),
        });
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
        let reader = credential_reader.clone();
        let inner = Reloading::new(
            identity_files(proxy_config),
            IDENTITY_RELOAD,
            move |[client_cert, client_key, root_ca]: &[Vec<u8>; 3]| -> eyre::Result<_> {
                // The proxy presents a verifiable certificate, so unlike direct
                // BMC connections this client keeps certificate checking on.
                let pool = libredfish::RedfishClientPool::builder()
                    .identity(client_cert.clone(), client_key.clone())
                    .add_root_certificates(root_ca.clone())
                    .build()
                    .map_err(|err| eyre::eyre!("building bmc-proxy redfish pool: {err}"))?;
                Ok(carbide_redfish::libredfish::new_proxied_pool(
                    reader.clone(),
                    pool,
                    proxy.clone(),
                ))
            },
            report_reload_failure,
        )?;
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
        let client = Reloading::new(
            identity_files(proxy_config),
            IDENTITY_RELOAD,
            |[client_cert, client_key, root_ca]: &[Vec<u8>; 3]| -> eyre::Result<_> {
                // Newline-separate the two PEM files: a file without a trailing
                // newline would otherwise fuse its END line with the next BEGIN.
                let identity = reqwest::Identity::from_pem(
                    &[client_key.as_slice(), b"\n", client_cert.as_slice()].concat(),
                )
                .map_err(|err| eyre::eyre!("bmc_proxy client identity: {err}"))?;

                let mut builder = reqwest::Client::builder()
                    .identity(identity)
                    .redirect(same_origin_redirect_policy(5))
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(60));
                for certificate in reqwest::Certificate::from_pem_bundle(root_ca)
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
            },
            report_reload_failure,
        )?;
        Ok(Self { client, target })
    }

    /// The client, as last built from the certificate files.
    pub(crate) fn client(&self) -> reqwest_middleware::ClientWithMiddleware {
        self.client.current()
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
    use super::*;

    // The reload contract itself is carbide_utils::reloading's; this proves
    // a check that cannot rebuild the client is counted as this client's
    // failed reload, and leaves the client in service.
    #[tokio::test(start_paused = true)]
    async fn a_client_that_cannot_be_rebuilt_counts_a_failed_reload() {
        let metrics = carbide_instrument::testing::MetricsCapture::start();
        let dir = tempfile::tempdir().expect("temp dir");
        let config = test_config_with_generated_pems(&dir);
        let passthrough = PassthroughClient::new(&config).expect("the first build succeeds");

        std::fs::write(&config.client_key, "not a key").expect("the key rewrites");
        tokio::time::advance(IDENTITY_REFRESH_INTERVAL).await;
        passthrough.client();
        passthrough.client.settled().await;

        assert_eq!(
            metrics.counter_delta("carbide_api_bmc_proxy_client_reload_failures_total", &[]),
            1.0,
            "the failed rebuild must be counted"
        );
    }
}
