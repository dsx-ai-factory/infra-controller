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
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nv_redfish::core::Bmc;
use tokio::net::TcpStream;

use super::{IterationResult, PeriodicCollector};
use crate::HealthError;
use crate::config::ReachabilityLogMode;
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, DataSink, EventContext, LogRecord, LogSeverity, MetricSample};

pub(crate) const COLLECTOR_TYPE: &str = "reachability";
const METRIC_NAME: &str = "tcp_port";

/// Collector service represented by a bounded reachability telemetry label.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ReachabilityService {
    Redfish,
    NvueRest,
    Gnmi,
    Nmxt,
    Nmxc,
}

impl ReachabilityService {
    /// Returns the stable telemetry label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Redfish => "redfish",
            Self::NvueRest => "nvue_rest",
            Self::Gnmi => "gnmi",
            Self::Nmxt => "nmxt",
            Self::Nmxc => "nmxc",
        }
    }
}

/// TCP target and effective collector port selected by discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReachabilityTarget {
    /// Collector service whose port is probed.
    pub(crate) service: ReachabilityService,

    /// Socket address selected from collector configuration.
    pub(crate) address: SocketAddr,
}

impl ReachabilityTarget {
    fn identity(&self) -> String {
        format!("{}@{}", self.service.as_str(), self.address)
    }
}

/// Result of one transport-level connection attempt.
struct ProbeOutcome {
    reachable: bool,
    duration: Duration,
    error: Option<String>,
}

/// Attempts a bounded TCP handshake without DNS or protocol exchange.
async fn probe(address: SocketAddr, timeout: Duration) -> ProbeOutcome {
    let started = Instant::now();
    let result = tokio::time::timeout(timeout, TcpStream::connect(address)).await;
    let duration = started.elapsed();

    match result {
        Ok(Ok(_stream)) => ProbeOutcome {
            reachable: true,
            duration,
            error: None,
        },
        Ok(Err(error)) => ProbeOutcome {
            reachable: false,
            duration,
            error: Some(error.to_string()),
        },
        Err(error) => ProbeOutcome {
            reachable: false,
            duration,
            error: Some(error.to_string()),
        },
    }
}

/// Discovery-selected inputs for one reachability collector.
pub(crate) struct ReachabilityCollectorStartConfig {
    /// Effective collector ports to probe each cycle.
    pub(crate) targets: Vec<ReachabilityTarget>,

    /// Maximum duration for one connection attempt.
    pub(crate) timeout: Duration,

    /// Log policy, or `None` when no configured sink consumes log events.
    pub(crate) log_mode: Option<ReachabilityLogMode>,

    /// Sink route for metric and structured log observations.
    pub(crate) sink: Arc<dyn DataSink>,
}

/// Periodic TCP probe collector that emits telemetry but no health reports.
pub(crate) struct ReachabilityCollector {
    targets: Vec<ReachabilityTarget>,
    timeout: Duration,
    log_mode: Option<ReachabilityLogMode>,
    sink: Arc<dyn DataSink>,
    event_context: EventContext,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for ReachabilityCollector {
    type Config = ReachabilityCollectorStartConfig;

    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        start_config: Self::Config,
    ) -> Result<Self, HealthError> {
        Ok(Self {
            targets: start_config.targets,
            timeout: start_config.timeout,
            log_mode: start_config.log_mode,
            sink: start_config.sink,
            event_context: EventContext::from_endpoint(&endpoint, COLLECTOR_TYPE),
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let mut fetch_failures = 0;

        // Probe a target at a time so one endpoint cannot multiply connection fan-out.
        for target in &self.targets {
            let outcome = probe(target.address, self.timeout).await;

            // Failed probes count as runtime fetch failures, but still emit observations.
            fetch_failures += usize::from(!outcome.reachable);
            self.emit_result(target, &outcome);
        }

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(self.targets.len()),
            fetch_failures,
        })
    }

    fn collector_type(&self) -> &'static str {
        COLLECTOR_TYPE
    }

    async fn stop(&mut self) {
        // Prometheus uses this event to remove this collector's endpoint series.
        self.sink
            .handle_event(&self.event_context, &CollectorEvent::CollectorRemoved);
    }
}

impl ReachabilityCollector {
    /// Emits a metric and any log selected by the stateless policy.
    fn emit_result(&self, target: &ReachabilityTarget, outcome: &ProbeOutcome) {
        self.sink.handle_event(
            &self.event_context,
            &CollectorEvent::Metric(Box::new(MetricSample {
                key: target.identity(),
                name: METRIC_NAME.to_string(),
                metric_type: "reachable".to_string(),
                unit: "state".to_string(),
                value: f64::from(outcome.reachable),
                labels: vec![
                    (
                        Cow::Borrowed("service"),
                        target.service.as_str().to_string(),
                    ),
                    (Cow::Borrowed("target_ip"), target.address.ip().to_string()),
                    (Cow::Borrowed("port"), target.address.port().to_string()),
                ],
                // This is a transport observation, not a health threshold.
                context: None,
            })),
        );

        let emit_log = match self.log_mode {
            Some(ReachabilityLogMode::All) => true,
            Some(ReachabilityLogMode::Unreachable) => !outcome.reachable,
            None => false,
        };

        if !emit_log {
            return;
        }

        let (body, severity, message_id, state) = if outcome.reachable {
            (
                "TCP port is reachable",
                LogSeverity::Info,
                "CarbideHealth.1.0.TcpPortReachable",
                "reachable",
            )
        } else {
            (
                "TCP port is unreachable",
                LogSeverity::Warn,
                "CarbideHealth.1.0.TcpPortUnreachable",
                "unreachable",
            )
        };

        let mut attributes = vec![
            (Cow::Borrowed("message_id"), message_id.to_string()),
            (
                Cow::Borrowed("message_args"),
                format!("[\"{}\"]", target.identity()),
            ),
            (
                Cow::Borrowed("reachability.service"),
                target.service.as_str().to_string(),
            ),
            (
                Cow::Borrowed("reachability.address"),
                target.address.ip().to_string(),
            ),
            (
                Cow::Borrowed("reachability.port"),
                target.address.port().to_string(),
            ),
            (Cow::Borrowed("reachability.state"), state.to_string()),
            (
                Cow::Borrowed("reachability.probe_duration_seconds"),
                outcome.duration.as_secs_f64().to_string(),
            ),
        ];

        if let Some(error) = &outcome.error {
            attributes.push((Cow::Borrowed("reachability.error"), error.clone()));
        }

        self.sink.handle_event(
            &self.event_context,
            &CollectorEvent::Log(Box::new(LogRecord {
                body: body.to_string(),
                severity,
                attributes,
                diagnostic_record: None,
            })),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::metrics::MetricsManager;
    use crate::processor::{EventProcessingPipeline, HealthReportProcessor};
    use crate::sink::PrometheusSink;

    #[derive(Default)]
    struct CapturingSink(Mutex<Vec<CollectorEvent>>);

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            self.0
                .lock()
                .expect("capturing sink mutex")
                .push(event.clone());

            Ok(())
        }
    }

    fn target() -> ReachabilityTarget {
        ReachabilityTarget {
            service: ReachabilityService::Redfish,
            address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
        }
    }

    fn collector(
        sink: Arc<dyn DataSink>,
        log_mode: Option<ReachabilityLogMode>,
    ) -> ReachabilityCollector {
        let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        endpoint
            .labels
            .insert("location".to_string(), "rack-a".to_string());

        ReachabilityCollector {
            targets: Vec::new(),
            timeout: Duration::from_secs(1),
            log_mode,
            sink,
            event_context: EventContext::from_endpoint(&endpoint, COLLECTOR_TYPE),
        }
    }

    #[tokio::test]
    async fn tcp_probe_reports_an_accepting_port_as_reachable() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("test listener should bind");

        let address = listener
            .local_addr()
            .expect("test listener should have an address");

        let outcome = probe(address, Duration::from_secs(1)).await;

        assert!(outcome.reachable);
        assert_eq!(outcome.error, None);

        drop(listener);
        let outcome = probe(address, Duration::from_secs(1)).await;

        assert!(!outcome.reachable);
        assert!(outcome.error.is_some());
    }

    #[tokio::test]
    async fn iteration_emits_one_metric_for_each_target() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("test listener should bind");

        let address = listener
            .local_addr()
            .expect("test listener should have an address");

        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector(sink.clone(), Some(ReachabilityLogMode::Unreachable));

        collector.targets.push(ReachabilityTarget {
            service: ReachabilityService::Redfish,
            address,
        });

        <ReachabilityCollector as PeriodicCollector<crate::bmc::BmcClient>>::run_iteration(
            &mut collector,
        )
        .await
        .expect("probe cycle should complete");

        let events = sink.0.lock().expect("sink mutex");

        assert!(matches!(events.as_slice(), [CollectorEvent::Metric(_)]));
    }

    #[test]
    fn log_modes_control_logs_without_suppressing_metrics() {
        let cases = [
            ("all success", Some(ReachabilityLogMode::All), true, 1),
            ("all failure", Some(ReachabilityLogMode::All), false, 1),
            (
                "unreachable success",
                Some(ReachabilityLogMode::Unreachable),
                true,
                0,
            ),
            (
                "unreachable failure",
                Some(ReachabilityLogMode::Unreachable),
                false,
                1,
            ),
            ("no log sink", None, false, 0),
        ];

        for (name, log_mode, reachable, expected_logs) in cases {
            let sink = Arc::new(CapturingSink::default());
            let collector = collector(sink.clone(), log_mode);

            collector.emit_result(
                &target(),
                &ProbeOutcome {
                    reachable,
                    duration: Duration::from_millis(2),
                    error: (!reachable).then(|| "probe failed".to_string()),
                },
            );

            let events = sink.0.lock().expect("sink mutex");

            let logs = events
                .iter()
                .filter_map(|event| match event {
                    CollectorEvent::Log(log) => Some(log),
                    _ => None,
                })
                .collect::<Vec<_>>();

            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, CollectorEvent::Metric(metric) if metric.context.is_none()))
                    .count(),
                1,
                "{name}",
            );

            assert_eq!(logs.len(), expected_logs, "{name}");

            if log_mode == Some(ReachabilityLogMode::Unreachable) && !reachable {
                let log = logs.first().expect("failed probe should emit a log record");

                for (key, value) in [
                    ("message_id", "CarbideHealth.1.0.TcpPortUnreachable"),
                    ("reachability.service", "redfish"),
                    ("reachability.address", "127.0.0.1"),
                    ("reachability.port", "443"),
                    ("reachability.state", "unreachable"),
                    ("reachability.probe_duration_seconds", "0.002"),
                    ("reachability.error", "probe failed"),
                ] {
                    assert!(
                        log.attributes.iter().any(|(actual_key, actual_value)| {
                            actual_key == key && actual_value == value
                        }),
                        "{name} should include {key}={value}",
                    );
                }
            }
        }
    }

    #[test]
    fn metrics_are_created_on_observation_and_removed_when_collector_stops() {
        let manager = Arc::new(MetricsManager::new("reachability_test").expect("metrics manager"));

        let sink = Arc::new(
            PrometheusSink::new(manager.clone(), "test").expect("prometheus sink should start"),
        );

        let collector = collector(sink.clone(), Some(ReachabilityLogMode::Unreachable));

        collector.emit_result(
            &target(),
            &ProbeOutcome {
                reachable: true,
                duration: Duration::from_millis(2),
                error: None,
            },
        );

        let exposition = manager.export_telemetry().expect("telemetry exposition");
        for expected in [
            "test_tcp_port_reachable_state".to_string(),
            "collector_type=\"reachability\"".to_string(),
            "endpoint_key=\"00:11:22:33:44:55\"".to_string(),
            "endpoint_ip=\"10.0.0.1\"".to_string(),
            "location=\"rack-a\"".to_string(),
            "service=\"redfish\"".to_string(),
            "target_ip=\"127.0.0.1\"".to_string(),
            "port=\"443\"".to_string(),
        ] {
            assert!(exposition.contains(&expected), "missing {expected}");
        }

        sink.handle_event(&collector.event_context, &CollectorEvent::CollectorRemoved);

        assert!(
            !manager
                .export_telemetry()
                .expect("telemetry exposition after collector removal")
                .contains("test_tcp_port_reachable_state")
        );
    }

    #[test]
    fn normal_processor_pipeline_does_not_create_reachability_health_reports() {
        let manager =
            Arc::new(MetricsManager::new("reachability_pipeline").expect("metrics manager"));

        let captured = Arc::new(CapturingSink::default());

        let sink = Arc::new(EventProcessingPipeline::new(
            vec![Arc::new(HealthReportProcessor::new())],
            captured.clone(),
            manager,
        ));

        let collector = collector(sink, Some(ReachabilityLogMode::Unreachable));

        collector.emit_result(
            &target(),
            &ProbeOutcome {
                reachable: true,
                duration: Duration::from_millis(2),
                error: None,
            },
        );

        let events = captured.0.lock().expect("sink mutex");

        assert!(matches!(events.as_slice(), [CollectorEvent::Metric(_)]));
    }
}
