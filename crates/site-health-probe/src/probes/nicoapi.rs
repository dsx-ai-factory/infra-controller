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

//! Probes against nico-api's gRPC surface, built on the `carbide-rpc` crate's
//! TLS client and generated types. Adding a nico-api probe means adding to
//! this module — nothing in the framework or the REST module changes.
//!
//! TODO(#5360-followup): the site-stats probe family (host-ingestion progress,
//! firmware-update tracking) belongs here as further read-only gRPC probes.

use std::time::{Duration, Instant};

use eyre::eyre;
use forge_tls::client_config::ClientCert;
use rpc::forge::{MachineSearchConfig, MachinesByIdsRequest};
use rpc::forge_tls_client::{ForgeClientConfig, ForgeTlsClient};

use crate::config;
use crate::framework::{ObservationRecorder, Probe};

/// Probes nico-api with the two read-only calls behind `machine show`:
/// FindMachineIds (the lightest DB-touching read) and FindMachinesByIds on the
/// first page (the full read path: handler + machine and interface joins).
/// Both operations are observed separately.
pub(crate) struct MachinesProbe {
    cfg: config::GrpcProbe,
}

impl MachinesProbe {
    pub(crate) fn new(cfg: config::GrpcProbe) -> Self {
        Self { cfg }
    }
}

#[async_trait::async_trait]
impl Probe for MachinesProbe {
    fn name(&self) -> &'static str {
        "grpc_machines"
    }

    fn api(&self) -> &'static str {
        "nico-api"
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    /// Each run builds a fresh client, so find_machine_ids includes TCP + TLS
    /// + HTTP/2 setup on top of the RPC itself. That is deliberate for a
    /// canary: it measures the cold path a new client experiences (and
    /// re-reads the certs, making rotation need no restart) rather than the
    /// warm path of a pooled connection. For the same reason this uses
    /// `ForgeTlsClient::build` (one attempt, like agent/main_loop and
    /// host-support/registration) rather than `retry_build` or the pooled
    /// `ForgeApiClient` wrapper — their connect retries and per-connect
    /// version() health check would hide exactly the failures a canary
    /// exists to count.
    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        // The rpc client treats an unreadable-but-present CA bundle as an
        // empty root store, which would only surface as a confusing handshake
        // failure; validate it up front so a bad mount names itself.
        preflight_ca(&self.cfg.tls.ca).await?;

        let mut client_config = ForgeClientConfig::new(
            self.cfg.tls.ca.clone(),
            Some(ClientCert {
                cert_path: self.cfg.tls.cert.clone(),
                key_path: self.cfg.tls.key.clone(),
            }),
        );
        // Server verification is always on — there is deliberately no insecure
        // mode. (The constructor honors DISABLE_TLS_ENFORCEMENT for local
        // development of other components; a canary measuring the real TLS
        // path must not.)
        client_config.enforce_tls = true;

        let url = format!("https://{}", self.cfg.target);
        let mut client = ForgeTlsClient::new(&client_config)
            .build(&url)
            .await
            .map_err(|e| eyre!("dial: {e}"))?;

        let start = Instant::now();
        let ids = client
            .find_machine_ids(MachineSearchConfig::default())
            .await
            .map_err(|e| eyre!("FindMachineIds: {e}"))?;
        recorder.record("find_machine_ids", start.elapsed());

        let mut page = ids.into_inner().machine_ids;
        if page.is_empty() {
            // An empty site is a healthy answer — the API and its DB read path
            // responded; there is just nothing to fetch details for.
            return Ok(());
        }
        page.truncate(self.cfg.effective_page_size());

        let start = Instant::now();
        client
            .find_machines_by_ids(MachinesByIdsRequest {
                machine_ids: page,
                include_history: false,
            })
            .await
            .map_err(|e| eyre!("FindMachinesByIds: {e}"))?;
        recorder.record("find_machines_by_ids", start.elapsed());
        Ok(())
    }
}

/// Fails with the offending path when the CA bundle is missing or contains no
/// parseable certificate.
async fn preflight_ca(path: &str) -> eyre::Result<()> {
    let pem = tokio::fs::read(path)
        .await
        .map_err(|e| eyre!("read CA: {e}"))?;
    let mut cursor = std::io::Cursor::new(&pem[..]);
    let parsed = rustls_pemfile::certs(&mut cursor).flatten().count();
    if parsed == 0 {
        return Err(eyre!("no certificates parsed from {path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TlsConfig;

    /// The CA preflight must name the offending path before any dial is
    /// attempted, instead of surfacing a generic handshake error later.
    /// This is the probe's only in-process gRPC test: everything requiring a
    /// live server (TLS verification, paging, partial observations) is
    /// exercised end to end against a real site, because faking nico-api
    /// would require test-only server stubs the workspace deliberately does
    /// not carry.
    #[tokio::test]
    async fn bad_ca_bundle_names_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = |name: &str| {
            dir.path()
                .join(name)
                .to_str()
                .expect("utf-8 path")
                .to_string()
        };
        let cfg = config::GrpcProbe {
            enabled: true,
            // Never dialed: both runs below fail in the CA preflight.
            target: "127.0.0.1:1".to_string(),
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            page_size: 2,
            tls: TlsConfig {
                ca: path("absent.crt"),
                cert: path("tls.crt"),
                key: path("tls.key"),
            },
        };
        let recorder = ObservationRecorder::default();

        let err = MachinesProbe::new(cfg.clone())
            .run(&recorder)
            .await
            .expect_err("missing CA fails");
        assert!(err.to_string().contains("read CA"), "got: {err:#}");

        std::fs::write(&cfg.tls.ca, "garbage").expect("write garbage ca");
        let err = MachinesProbe::new(cfg)
            .run(&recorder)
            .await
            .expect_err("garbage CA fails");
        assert!(
            err.to_string().contains("no certificates parsed from"),
            "got: {err:#}"
        );
    }
}
