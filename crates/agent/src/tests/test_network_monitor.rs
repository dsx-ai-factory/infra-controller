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
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ::rpc::forge::{self as rpc};
use ::rpc::forge_tls_client::ForgeClientConfig;
use axum::Router;
use axum::extract::State as AxumState;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use carbide_host_support::agent_config::AgentConfig;
use eyre::{Context, Result};
use forge_tls::client_config::ClientCert;
use opentelemetry::metrics::{Meter, MeterProvider};
use opentelemetry_sdk::metrics;
use prometheus::{Encoder, TextEncoder};
use tokio::sync::{Mutex, watch};
use tokio::time::sleep;
use tonic::async_trait;
use tracing::info;

use crate::instrumentation::NetworkMonitorMetricsState;
use crate::network_monitor::{DpuInfo, DpuPingResult, NetworkMonitor, NetworkMonitorError, Ping};
use crate::tests::common;

// DPU machine ids for testing purposes
const DPU_ID: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";
const DEST_DPU_ID: &str = "fm100dsjd1vuk6gklgvh0ao8t7r7tk1pt101ub5ck0g3j7lqcm8h3rf1p8g";

#[derive(Default, Debug)]
struct State {
    num_get_dpu_ips: AtomicUsize,
}

#[tokio::test]
async fn test_network_monitor() -> eyre::Result<()> {
    let state: Arc<Mutex<State>> = Arc::new(Mutex::new(Default::default()));

    // Start carbide API
    let app = Router::new()
        .route("/up", get(handle_up))
        .route(
            ::rpc::service_path!("GetDpuInfoList"),
            post(handle_get_dpu_info_list),
        )
        // ForgeApiClient needs a working Version route for connection retrying
        .route(::rpc::service_path!("Version"), post(handle_version))
        .fallback(handler)
        .with_state(state.clone());
    let (addr, join_handle) = common::run_grpc_server(app).await?;

    let td = tempfile::tempdir()?;
    let agent_config_file = tempfile::NamedTempFile::new()?;
    let opts = match common::setup_agent_run_env(&addr, &td, &agent_config_file, false) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            return Ok(());
        }
        Err(e) => {
            return Err(e);
        }
    };

    let (agent, _path) = match opts.config_path {
        // normal production case
        None => (AgentConfig::default(), "default".to_string()),
        // development overrides
        Some(config_path) => (
            AgentConfig::load_from(&config_path).wrap_err(format!(
                "error loading agent configuration from {}",
                config_path.display()
            ))?,
            config_path.display().to_string(),
        ),
    };

    let forge_client_config = Arc::new(
        ForgeClientConfig::new(
            agent.forge_system.root_ca.clone(),
            Some(ClientCert {
                cert_path: agent.forge_system.client_cert.clone(),
                key_path: agent.forge_system.client_key.clone(),
            }),
        )
        .use_mgmt_vrf()?,
    );

    let forge_api = agent.forge_system.api_server;

    let machine_id = DPU_ID.parse()?;

    // Initialize the test metric meter
    info!("Initializing test meter");
    let test_meter = TestMeter::default();
    let metrics_states = NetworkMonitorMetricsState::initialize(test_meter.meter(), machine_id);

    // Initialize network monitor
    let forge_api_clone = forge_api.clone();
    let forge_client_config_clone = Arc::clone(&forge_client_config);
    let (close_sender, mut close_receiver) = watch::channel(false);

    info!("Initializing network monitor");
    let mut network_monitor = NetworkMonitor::new(
        machine_id,
        Some(metrics_states.clone()),
        Arc::new(MockPinger),
    );

    info!("Starting network monitor");
    tokio::spawn(async move {
        network_monitor
            .run(
                &forge_api_clone,
                forge_client_config_clone,
                &mut close_receiver,
            )
            .await
    });

    sleep(Duration::from_secs(5)).await;
    info!("Sending close signal");
    let _ = close_sender.send(true);

    join_handle.abort();

    verify_metrics(&test_meter);
    Ok(())
}

async fn handle_up() -> &'static str {
    "OK"
}

async fn handle_get_dpu_info_list(
    AxumState(state): AxumState<Arc<Mutex<State>>>,
) -> impl axum::response::IntoResponse {
    {
        state
            .lock()
            .await
            .num_get_dpu_ips
            .fetch_add(1, Ordering::SeqCst);
    }
    common::respond(rpc::GetDpuInfoListResponse {
        dpu_list: vec![
            rpc::DpuInfo {
                id: DPU_ID.to_string(),
                loopback_ip: "172.20.0.119".to_string(),
                observed_status: None,
            },
            rpc::DpuInfo {
                id: DEST_DPU_ID.to_string(),
                loopback_ip: "172.20.0.200".to_string(),
                observed_status: None,
            },
        ],
    })
}

async fn handle_version() -> impl IntoResponse {
    common::respond(rpc::BuildInfo::default())
}

async fn handler(uri: Uri) -> impl IntoResponse {
    tracing::debug!(?uri, "General request handler received request");
    StatusCode::NOT_FOUND
}

fn verify_metrics(test_meter: &TestMeter) {
    let attribute = format!("{{dest_dpu_id=\"{DEST_DPU_ID}\",source_dpu_id=\"{DPU_ID}\"}}");

    let expected_network_latency_count = format!("{attribute} 1");
    let expected_network_loss_percentage_sum = format!("{attribute} 0.8");

    // Verify network_latency_count
    match test_meter.formatted_metric("forge_dpu_agent_network_latency_milliseconds_count") {
        Some(network_latency_count) => {
            assert_eq!(
                network_latency_count, expected_network_latency_count,
                "forge_dpu_agent_network_latency_milliseconds_count does not match"
            );
        }
        None => panic!("forge_dpu_agent_network_latency_milliseconds_count metric not found"),
    }

    // Verify network_loss_percentage_sum
    match test_meter.formatted_metric("forge_dpu_agent_network_loss_percentage_sum") {
        Some(network_loss_percentage_sum) => {
            assert_eq!(
                network_loss_percentage_sum, expected_network_loss_percentage_sum,
                "forge_dpu_agent_network_loss_percentage_sum does not match"
            );
        }
        None => panic!("forge_dpu_agent_network_loss_percentage_sum metric not found"),
    }
}

struct TestMeter {
    meter: Meter,
    _meter_provider: metrics::SdkMeterProvider,
    registry: prometheus::Registry,
}

impl TestMeter {
    /// Returns the latest accumulated metrics in prometheus format
    fn export_metrics(&self) -> String {
        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }

    fn meter(&self) -> Meter {
        self.meter.clone()
    }

    /// Returns the value of a single metric with a given name
    fn formatted_metric(&self, metric_name: &str) -> Option<String> {
        let mut metrics = self.formatted_metrics(metric_name);
        match metrics.len() {
            0 => None,
            1 => metrics.pop(),
            n => panic!(
                "Expected to find a single metric with name \"{metric_name}\", but found {n}. Full metrics:\n{metrics:?}"
            ),
        }
    }

    /// Returns the value of multiple metrics with the given name
    /// This can be used if the metric is duplicated due to attributes
    fn formatted_metrics(&self, metric_name: &str) -> Vec<String> {
        let formatted = self.export_metrics();
        let mut result = Vec::new();
        for line in formatted.lines() {
            // Metrics look like "metric_name $value" if without attributes
            // and "metric_name{$attrs} value" if with attributes
            if !line.starts_with(metric_name) {
                continue;
            }
            let line = line.trim_start_matches(metric_name);
            if line.starts_with('{') {
                result.push(line.to_string());
            } else {
                result.push(line.strip_prefix(' ').unwrap_or(line).to_string());
            }
        }
        result.sort();
        result
    }
}

impl Default for TestMeter {
    /// Builds an OpenTelemetry `Meter` for unit-testing purposes
    fn default() -> Self {
        // Note: This configures metrics bucket between 5.0 and 10000.0, which are best suited
        // for tracking milliseconds
        // See https://github.com/open-telemetry/opentelemetry-rust/blob/495330f63576cfaec2d48946928f3dc3332ba058/opentelemetry-sdk/src/metrics/reader.rs#L155-L158
        let prometheus_registry = prometheus::Registry::new();
        let metrics_exporter = opentelemetry_prometheus::exporter()
            .with_registry(prometheus_registry.clone())
            .without_scope_info()
            .without_target_info()
            .build()
            .unwrap();
        let view = carbide_metrics_utils::new_view(
            "*_network_*", // Match all instruments with "network" in their name
            None,
            metrics::Aggregation::ExplicitBucketHistogram {
                boundaries: vec![0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0],
                record_min_max: true,
            },
        )
        .ok();
        let meter_provider = match view {
            Some(metric_view) => metrics::SdkMeterProvider::builder()
                .with_reader(metrics_exporter)
                .with_view(metric_view)
                .build(),
            None => metrics::SdkMeterProvider::builder()
                .with_reader(metrics_exporter)
                .build(),
        };

        let meter = meter_provider.meter("dpu-agent");

        TestMeter {
            // For some reason, letting the SdkMeterProvider go out of scope
            // shuts down the reader associated with the Meter that we created
            // from it. Make sure it stays around for the lifetime of the Meter.
            _meter_provider: meter_provider,
            meter,
            registry: prometheus_registry,
        }
    }
}

struct MockPinger;
#[async_trait]
impl Ping for MockPinger {
    async fn ping_dpu(
        &self,
        dpu_info: DpuInfo,
        _interface: IpAddr,
    ) -> Result<DpuPingResult, (NetworkMonitorError, eyre::Report)> {
        info!(%dpu_info, "Received ping request");
        let ping_result = DpuPingResult {
            dpu_info,
            success_count: 1,
            average_latency: Some(Duration::from_millis(1)),
        };

        Ok(ping_result)
    }
}
