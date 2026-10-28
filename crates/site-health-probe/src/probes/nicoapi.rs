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

use std::sync::Arc;
use std::time::Duration;

use eyre::eyre;
use forge_tls::client_config::ClientCert;
use rpc::forge::{MachineSearchConfig, MachinesByIdsRequest};
use rpc::forge_tls_client::{ForgeClientConfig, ForgeTlsClient};

use crate::config;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::{CallError, CallGate, classify_grpc};

/// Probes nico-api with the two read-only calls behind `machine show`:
/// FindMachineIds (the lightest DB-touching read) and FindMachinesByIds on the
/// first page (the full read path: handler + machine and interface joins).
/// Both operations are observed separately, and both pass the nico-api gate
/// shared with the state readers.
pub(crate) struct MachinesProbe {
    cfg: config::GrpcProbe,
    gate: Arc<CallGate>,
}

impl MachinesProbe {
    pub(crate) fn new(cfg: config::GrpcProbe, gate: Arc<CallGate>) -> Self {
        Self { cfg, gate }
    }
}

#[async_trait::async_trait]
impl Probe for MachinesProbe {
    fn name(&self) -> &'static str {
        "grpc_machines"
    }

    fn api(&self) -> &'static str {
        self.gate.api()
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

        let ids = self
            .gate
            .call(recorder, "find_machine_ids", || async {
                client
                    .find_machine_ids(MachineSearchConfig::default())
                    .await
                    .map_err(|e| classify_grpc(&e))
            })
            .await
            .map_err(|e| call_failed("FindMachineIds", e))?;

        let mut page = ids.into_inner().machine_ids;
        if page.is_empty() {
            // An empty site is a healthy answer — the API and its DB read path
            // responded; there is just nothing to fetch details for.
            return Ok(());
        }
        page.truncate(self.cfg.effective_page_size());

        self.gate
            .call(recorder, "find_machines_by_ids", || async {
                client
                    .find_machines_by_ids(MachinesByIdsRequest {
                        machine_ids: page,
                        include_history: false,
                    })
                    .await
                    .map_err(|e| classify_grpc(&e))
            })
            .await
            .map_err(|e| call_failed("FindMachinesByIds", e))?;
        Ok(())
    }
}

/// Names the failed call and keeps the [`CallError`] in the chain, so the
/// scheduler can tell a gate timeout from a failure. `{err:#}` still reads
/// "FindMachineIds: call timed out".
fn call_failed(call: &'static str, err: CallError) -> eyre::Report {
    eyre::Report::new(err).wrap_err(call)
}

/// Fails with the offending path when the CA bundle is missing or contains no
/// parseable certificate.
pub(crate) async fn preflight_ca(path: &str) -> eyre::Result<()> {
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
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::TlsConfig;
    use crate::framework::{Outcome, ProbeResult, Sink, start};
    use crate::gate::testing::{Tally, counted, series, tally};
    use crate::metrics::Metrics;

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

        let gate = Arc::new(CallGate::new(
            "nico-api",
            2.0,
            Duration::from_secs(5),
            Duration::from_secs(30),
        ));
        let err = MachinesProbe::new(cfg.clone(), Arc::clone(&gate))
            .run(&recorder)
            .await
            .expect_err("missing CA fails");
        assert!(err.to_string().contains("read CA"), "got: {err:#}");

        std::fs::write(&cfg.tls.ca, "garbage").expect("write garbage ca");
        let err = MachinesProbe::new(cfg, gate)
            .run(&recorder)
            .await
            .expect_err("garbage CA fails");
        assert!(
            err.to_string().contains("no certificates parsed from"),
            "got: {err:#}"
        );
    }

    /// What the fake nico-api does with a call.
    #[derive(Clone, Copy)]
    enum Reply {
        /// Answers after this long.
        After(Duration),
        /// Answers DEADLINE_EXCEEDED at once.
        DeadlineExceeded,
    }

    /// The two gate calls of `MachinesProbe::run`, with its error wrapping,
    /// against a fake server: FindMachineIds answers at once, then
    /// FindMachinesByIds does what `reply` says.
    struct GateCallProbe {
        gate: Arc<CallGate>,
        timeout: Duration,
        reply: Reply,
    }

    #[async_trait::async_trait]
    impl Probe for GateCallProbe {
        fn name(&self) -> &'static str {
            "grpc_machines"
        }

        fn api(&self) -> &'static str {
            self.gate.api()
        }

        fn interval(&self) -> Duration {
            Duration::from_secs(10)
        }

        fn timeout(&self) -> Duration {
            self.timeout
        }

        async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
            self.gate
                .call(recorder, "find_machine_ids", || async { Ok(()) })
                .await
                .map_err(|e| call_failed("FindMachineIds", e))?;

            self.gate
                .call(recorder, "find_machines_by_ids", || async {
                    match self.reply {
                        Reply::After(took) => {
                            tokio::time::sleep(took).await;
                            Ok(())
                        }
                        Reply::DeadlineExceeded => {
                            Err(classify_grpc(&tonic::Status::deadline_exceeded("slow")))
                        }
                    }
                })
                .await
                .map_err(|e| call_failed("FindMachinesByIds", e))
        }
    }

    /// The first result of a probe run by the scheduler.
    async fn first_result(probe: GateCallProbe) -> ProbeResult {
        let cancel = CancellationToken::new();
        let (mut results, _loops) = start(vec![Arc::new(probe)], cancel.clone());
        let result = results.recv().await.expect("result");
        cancel.cancel();
        result
    }

    /// The operations a result carries observations of.
    fn observed(result: &ProbeResult) -> Vec<&'static str> {
        result.observations.iter().map(|o| o.operation).collect()
    }

    /// A call cut by the gate's call timeout (5 s) before the probe's own
    /// deadline (8 s), or answered DEADLINE_EXCEEDED by the server, is a probe
    /// timeout, not a failure. The observation made before it is kept, and the
    /// gate still counts the cut call as "timeout".
    #[tokio::test(start_paused = true)]
    async fn call_timeout_before_probe_deadline_is_a_timeout() {
        let replies = [
            Reply::After(Duration::from_secs(6)),
            Reply::DeadlineExceeded,
        ];
        for reply in replies {
            let gate = Arc::new(CallGate::new(
                "nico-api",
                2.0,
                Duration::from_secs(5),
                Duration::from_secs(30),
            ));
            let metrics = counted(&gate);

            let result = first_result(GateCallProbe {
                gate,
                timeout: Duration::from_secs(8),
                reply,
            })
            .await;

            assert_eq!(result.outcome, Outcome::Timeout);
            assert_eq!(
                result.error.as_deref(),
                Some("FindMachinesByIds: call timed out")
            );
            assert_eq!(
                observed(&result),
                ["find_machine_ids"],
                "the call before the cut one is kept, the cut one is not recorded"
            );
            // The cut call is a timeout, not also a cancelled one.
            assert_eq!(
                tally(&metrics.registry),
                Tally {
                    ok: 1.0,
                    timeout: 1.0,
                    ..Tally::default()
                }
            );
        }
    }

    /// A probe timeout (5 s) that lands inside a call still running under the
    /// gate's call timeout (10 s) drops the call's future. The call is counted
    /// as cancelled, not as a timeout, and the observation made before it is
    /// kept.
    #[tokio::test(start_paused = true)]
    async fn probe_timeout_cancels_the_call_in_flight() {
        let gate = Arc::new(CallGate::new(
            "nico-api",
            2.0,
            Duration::from_secs(10),
            Duration::from_secs(30),
        ));
        let setup = counted(&gate);

        let result = first_result(GateCallProbe {
            gate,
            timeout: Duration::from_secs(5),
            reply: Reply::After(Duration::from_secs(20)),
        })
        .await;

        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(result.error.as_deref(), Some("probe timed out"));
        assert_eq!(observed(&result), ["find_machine_ids"]);
        let call = |name, outcome| {
            series(
                &setup.registry,
                "carbide_site_health_probe_calls_total",
                &[("call", name), ("result", outcome)],
            )
        };
        assert_eq!(call("find_machine_ids", "ok"), 1.0);
        assert_eq!(call("find_machines_by_ids", "cancelled"), 1.0);
        assert_eq!(
            tally(&setup.registry),
            Tally {
                ok: 1.0,
                cancelled: 1.0,
                ..Tally::default()
            },
            "the cut call is not also a timeout"
        );

        // The sink counts the run as a timeout.
        Metrics::new(&setup.meter).record(&result);
        assert_eq!(
            series(
                &setup.registry,
                "carbide_site_health_probe_requests_total",
                &[("api", "nico-api"), ("outcome", "timeout")],
            ),
            1.0
        );
    }
}
