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
use std::net::SocketAddr;
use std::sync::Arc;

use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{Method, Request, Response};
use http_body_util::Full;
use hyper::body;
use hyper::body::Bytes;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use prometheus::Encoder;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::config::Config;
use crate::shutdown_handle::ShutdownHandle;
use crate::{fork_cancel_token, tcp_listener};

pub(crate) async fn spawn(
    config: Arc<Config>,
    metrics_state: Arc<MetricsState>,
    cancel_token: CancellationToken,
) -> Result<MetricsHandle, SpawnError> {
    let (listener, metrics_address) = tcp_listener::bind(config.metrics_address)
        .await
        .map_err(SpawnError::Listen)?;

    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);

    tracing::info!(
        %metrics_address,
        "metrics service listening"
    );

    let join_handle = tokio::spawn(async move {
        while let Some(res) = cancel_token.run_until_cancelled(listener.accept()).await {
            match res {
                Ok((stream, addr)) => {
                    tracing::info!(peer_address = %addr, "accepted metrics connection");
                    tokio::task::spawn({
                        let metrics_state = metrics_state.clone();
                        async move {
                            let io = TokioIo::new(stream);
                            auto::Builder::new(TokioExecutor::new())
                                .serve_connection(
                                    io,
                                    hyper::service::service_fn(move |req| {
                                        let metrics_state = metrics_state.clone();
                                        async move { serve_metrics(req, metrics_state) }
                                    }),
                                )
                                .await
                        }
                    });
                }
                Err(error) => {
                    tracing::error!(%error, "error accepting metrics connection");
                    // Avoid a hot loop when the listener keeps failing (e.g. file descriptor limits).
                    cancel_token
                        .run_until_cancelled(tokio::time::sleep(std::time::Duration::from_millis(
                            100,
                        )))
                        .await;
                }
            }
        }
    });

    Ok(MetricsHandle {
        metrics_address,
        join_handle,
        drop_guard,
    })
}

#[derive(thiserror::Error, Debug)]
// Kept public because `crate::SpawnError` exposes it as a payload.
pub enum SpawnError {
    #[error("error listening on metrics address: {0}")]
    Listen(std::io::Error),
}

fn serve_metrics(
    req: Request<body::Incoming>,
    state: Arc<MetricsState>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let response = match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let mut buffer = vec![];
            let encoder = prometheus::TextEncoder::new();
            let metric_families = state.registry.gather();
            match encoder.encode(&metric_families, &mut buffer) {
                Ok(_) => Response::builder()
                    .status(200)
                    .header(CONTENT_TYPE, encoder.format_type())
                    .header(CONTENT_LENGTH, buffer.len())
                    .body(buffer.into()),
                Err(e) => Response::builder()
                    .status(500)
                    .body(format!("Encoding error: {e}").into()),
            }
        }
        (&Method::GET, "/") => Response::builder().status(200).body("/metrics".into()),
        _ => Response::builder().status(404).body("Invalid URL".into()),
    };

    Ok(response.expect("BUG: Response::builder error"))
}

pub(crate) struct MetricsHandle {
    metrics_address: std::net::SocketAddr,
    join_handle: JoinHandle<()>,
    drop_guard: DropGuard,
}

impl MetricsHandle {
    pub(crate) fn metrics_address(&self) -> SocketAddr {
        self.metrics_address
    }
}

impl ShutdownHandle<()> for MetricsHandle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

pub(crate) struct MetricsState {
    pub(crate) meter: Meter,
    pub(crate) registry: prometheus::Registry,
    _meter_provider: SdkMeterProvider,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsState {
    /// `MetricsState::new` creates ssh-console's registry and installs the
    /// same provider globally for generated Forge-client RED metrics.
    pub(crate) fn new() -> Self {
        let registry = prometheus::Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .expect("BUG: could not build default metrics state");
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_reader(exporter)
            .build();

        // Generated Forge clients record RED through
        // `opentelemetry::global::meter`, while this registry is what
        // `/metrics` exports. Install the same provider before the first
        // client call so those measurements land here.
        opentelemetry::global::set_meter_provider(meter_provider.clone());
        let meter = meter_provider.meter("ssh-console");

        Self {
            meter,
            registry,
            _meter_provider: meter_provider,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const CHILD_PROCESS_ENV: &str = "CARBIDE_SSH_CONSOLE_METRICS_TEST_CHILD";

    #[test]
    fn metrics_state_exports_global_red_metrics() {
        if std::env::var_os(CHILD_PROCESS_ENV).is_some() {
            assert_global_red_metrics_exported();
            return;
        }

        // `MetricsState::new` installs process-global state, and RED caches
        // its histogram. Run the assertion in a fresh process so parallel
        // tests cannot inherit or replace either one.
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "metrics::tests::metrics_state_exports_global_red_metrics",
                "--nocapture",
            ])
            .env(CHILD_PROCESS_ENV, "1")
            .output()
            .expect("run ssh-console metrics test child");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "ssh-console metrics child failed\nstdout:\n{}\nstderr:\n{}",
            stdout,
            stderr,
        );
    }

    fn assert_global_red_metrics_exported() {
        let state = MetricsState::new();
        carbide_instrument::red::record("ssh-console-test", "connect", "ok", 1.0);

        let family = state
            .registry
            .gather()
            .into_iter()
            .find(|family| family.name() == "carbide_external_call_duration_milliseconds")
            .expect("RED histogram should use the ssh-console registry");
        let expected_labels = [
            ("backend", "ssh-console-test"),
            ("operation", "connect"),
            ("outcome", "ok"),
        ];
        let metric = family
            .get_metric()
            .iter()
            .find(|metric| {
                expected_labels.iter().all(|(name, value)| {
                    metric
                        .get_label()
                        .iter()
                        .any(|label| label.name() == *name && label.value() == *value)
                })
            })
            .expect("RED histogram should contain the recorded series");

        assert_eq!(
            metric.get_label().len(),
            expected_labels.len(),
            "RED histogram should not expose unexpected labels",
        );
        assert_eq!(metric.get_histogram().get_sample_count(), 1);
    }
}
