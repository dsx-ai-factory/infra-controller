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

use super::{CollectorEvent, DataSink, EventContext};
use crate::HealthError;
use crate::collectors::REACHABILITY_COLLECTOR_TYPE;
use crate::config::TracingSinkConfig;

/// Sink that writes health events through the process tracing subscriber.
pub struct TracingSink {
    include_diagnostics: bool,
}

impl TracingSink {
    /// Builds a tracing sink from configuration.
    pub fn new(config: &TracingSinkConfig) -> Self {
        Self {
            include_diagnostics: config.include_diagnostics,
        }
    }
}

impl DataSink for TracingSink {
    fn sink_type(&self) -> &'static str {
        "tracing_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        if context.collector_type == REACHABILITY_COLLECTOR_TYPE
            && matches!(event, CollectorEvent::Metric(_))
        {
            // Metric samples are not rendered as logs; log_mode controls probe logs.
            return Ok(());
        }

        match event {
            CollectorEvent::MetricCollectionStart => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    "Metric collection start"
                );
            }
            CollectorEvent::Metric(metric) => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    metric = %metric.name,
                    key = %metric.key,
                    metric_type = %metric.metric_type,
                    unit = %metric.unit,
                    value = metric.value,
                    "Metric event"
                );
            }
            CollectorEvent::MetricCollectionEnd => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    "Metric collection end"
                );
            }
            CollectorEvent::CollectorRemoved => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    "Collector removed"
                );
            }
            CollectorEvent::Log(record) => {
                let record = record.emitted_log_record(self.include_diagnostics);
                let record = record.without_decoded_protobuf_payload();

                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    machine_id = context.machine_id().map(tracing::field::display),
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    machine_serial = context.machine_serial(),
                    driver_version = context.driver_version(),
                    component_type = context.component_type(),
                    nvlink_domain_uuid = context.nvlink_domain_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    severity = %record.severity,
                    body = %record.body,
                    attributes = ?record.attributes,
                    "Log event"
                );
            }
            CollectorEvent::Firmware(info) => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    firmware_name = %info.component,
                    version = %info.version,
                    "Firmware info event"
                );
            }
            CollectorEvent::HealthReport(report) => {
                tracing::info!(
                    endpoint = %context.endpoint_key(),
                    collector = %context.collector_type,
                    machine_id = ?context.machine_id(),
                    rack_id = context.rack_id().map(tracing::field::display),
                    system_uuid = context.system_uuid().map(tracing::field::display),
                    labels = ?context.labels(),
                    success_count = report.successes.len(),
                    alert_count = report.alerts.len(),
                    alerts = ?report.alerts,
                    report_source = report.source.as_str(),
                    target = ?report.target,
                    "Health report event"
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use carbide_instrument::testing::capture_logs;
    use carbide_uuid::rack::RackId;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::sink::{LogRecord, LogSeverity};

    #[test]
    fn decoded_protobuf_payload_is_not_emitted_by_tracing_sink() {
        let sink = TracingSink::new(&TracingSinkConfig {
            include_diagnostics: true,
        });

        let endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        let context = EventContext::from_endpoint(&endpoint, "test_collector");

        let event = CollectorEvent::Log(Box::new(LogRecord {
            body: "Test notification".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![
                (Cow::Borrowed("gentime"), "2026-07-05 12:34:56".to_string()),
                (Cow::Borrowed("user"), "admin".to_string()),
                (
                    Cow::Borrowed(LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE),
                    serde_json::json!({ "testField": {} }).to_string(),
                ),
            ],
            diagnostic_record: None,
        }));

        let logs = capture_logs(|| sink.handle_event(&context, &event));

        let log = logs.first().expect("tracing sink emits one log");

        assert_eq!(logs.len(), 1);
        assert_eq!(log.message, "Log event");

        assert_eq!(
            log.field("attributes"),
            Some(r#"[("gentime", "2026-07-05 12:34:56"), ("user", "admin")]"#)
        );
    }

    #[test]
    fn tracing_sink_includes_optional_rack_id() {
        let sink = TracingSink::new(&TracingSinkConfig {
            include_diagnostics: false,
        });

        for (rack_id, expected_rack_id) in
            [(Some(RackId::new("RACK_1")), Some("RACK_1")), (None, None)]
        {
            let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
            endpoint.rack_id = rack_id;
            let context = EventContext::from_endpoint(&endpoint, "test_collector");

            let logs = capture_logs(|| {
                sink.handle_event(&context, &CollectorEvent::MetricCollectionStart)
            });

            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].field("rack_id"), expected_rack_id);
        }
    }

    #[test]
    fn reachability_metrics_do_not_bypass_structured_log_policy() {
        let sink = TracingSink::new(&TracingSinkConfig {
            include_diagnostics: false,
        });

        let endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        let context = EventContext::from_endpoint(&endpoint, REACHABILITY_COLLECTOR_TYPE);

        let metric = CollectorEvent::Metric(
            crate::sink::MetricSample {
                key: "redfish@127.0.0.1:443".to_string(),
                name: "tcp_port".to_string(),
                metric_type: "reachable".to_string(),
                unit: "state".to_string(),
                value: 1.0,
                labels: vec![],
                context: None,
            }
            .into(),
        );

        assert!(capture_logs(|| sink.handle_event(&context, &metric)).is_empty());

        let log = CollectorEvent::Log(Box::new(LogRecord {
            body: "TCP port is reachable".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![],
            diagnostic_record: None,
        }));

        assert_eq!(capture_logs(|| sink.handle_event(&context, &log)).len(), 1);
    }
}
