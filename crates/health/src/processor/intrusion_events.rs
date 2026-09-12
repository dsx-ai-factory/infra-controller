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
use std::sync::Arc;

use super::{CollectorEvent, EventContext, EventProcessor};
use crate::sink::{
    Classification, HealthReport, HealthReportAlert, HealthReportSuccess, HealthReportTarget,
    LogRecord, LogSeverity, Probe, ReportSource,
};

const HOST_BMC_TARGET: &str = "HostBMC";
const INTRUSION_ALERT_MESSAGE: &str = "Physical Chassis Intrusion Alert";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum IntrusionEventState {
    Alert,
    Clear,
}

#[derive(Default)]
pub struct BmcIntrusionEventProcessor;

impl BmcIntrusionEventProcessor {
    pub fn new() -> Self {
        Self
    }

    fn attr<'a>(attributes: &'a [(Cow<'static, str>, String)], key: &str) -> Option<&'a str> {
        attributes
            .iter()
            .find(|(name, _)| name.as_ref() == key)
            .map(|(_, value)| value.as_str())
    }

    fn message_args(record: &LogRecord) -> Vec<String> {
        Self::attr(&record.attributes, "message_args")
            .and_then(|args| serde_json::from_str::<Vec<String>>(args).ok())
            .unwrap_or_default()
    }

    fn intrusion_event_state(record: &LogRecord) -> Option<IntrusionEventState> {
        let mut text = record.body.clone();
        text.push(' ');
        for arg in Self::message_args(record) {
            text.push_str(&arg);
            text.push(' ');
        }

        let text = text.to_ascii_lowercase();
        let mentions_intrusion = text.contains("chassis intrusion")
            || text.contains("intrusion sensor")
            || text.contains("physical chassis intrusion")
            || text.contains("reset intrusion");

        if !mentions_intrusion {
            return None;
        }

        if text.contains("clear") || text.contains("deassert") || text.contains("reset") {
            return Some(IntrusionEventState::Clear);
        }

        // Typed severity and state words in the payload are both signals. Some
        // BMCs omit Severity while describing the transition in Message.
        let severe = matches!(
            record.severity,
            LogSeverity::Warn | LogSeverity::Error | LogSeverity::Fatal
        );

        if severe
            || text.contains("physical chassis intrusion alert")
            || text.contains("trigger")
            || text.contains("triggered")
            || text.contains("assert")
            || text.contains("alert")
            || text.contains("critical")
            || text.contains("warning")
        {
            return Some(IntrusionEventState::Alert);
        }

        if text.contains("normal") {
            return Some(IntrusionEventState::Clear);
        }

        None
    }
}

impl EventProcessor for BmcIntrusionEventProcessor {
    fn processor_type(&self) -> &'static str {
        "bmc_intrusion_event_processor"
    }

    fn process_event(
        &self,
        _context: &EventContext,
        event: &CollectorEvent,
    ) -> Vec<CollectorEvent> {
        let CollectorEvent::Log(record) = event else {
            return Vec::new();
        };

        let Some(state) = Self::intrusion_event_state(record) else {
            return Vec::new();
        };

        let (successes, alerts) = match state {
            IntrusionEventState::Alert => (
                Vec::new(),
                vec![HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: INTRUSION_ALERT_MESSAGE.to_string(),
                    classifications: vec![
                        Classification::SensorCritical,
                        Classification::PreventAllocations,
                    ],
                }],
            ),
            IntrusionEventState::Clear => (
                vec![HealthReportSuccess {
                    attribution: None,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                }],
                Vec::new(),
            ),
        };

        let report = HealthReport {
            source: ReportSource::BmcEvents,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes,
            alerts,
        };

        vec![CollectorEvent::HealthReport(Arc::new(report))]
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::value_scenarios;
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;

    #[derive(Clone, Copy)]
    struct IntrusionLogCase {
        body: &'static str,
        severity: LogSeverity,
        message_args: Option<&'static str>,
    }

    #[derive(Debug, PartialEq)]
    struct IntrusionReportSummary {
        alert_count: usize,
        success_count: usize,
        probe_id: Probe,
        target: Option<String>,
        message: Option<String>,
        classifications: Vec<Classification>,
    }

    fn context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "logs_collector",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn log(body: &str, severity: LogSeverity, message_args: Option<&str>) -> CollectorEvent {
        let mut attributes = Vec::new();
        if let Some(message_args) = message_args {
            attributes.push((Cow::Borrowed("message_args"), message_args.to_string()));
        }

        CollectorEvent::Log(Box::new(LogRecord {
            body: body.to_string(),
            severity,
            attributes,
            diagnostic_record: None,
        }))
    }

    fn emitted_report(event: CollectorEvent) -> Arc<HealthReport> {
        let processor = BmcIntrusionEventProcessor::new();
        let emitted = processor.process_event(&context(), &event);
        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };

        Arc::clone(report)
    }

    fn summarize_report(case: IntrusionLogCase) -> IntrusionReportSummary {
        let report = emitted_report(log(case.body, case.severity, case.message_args));

        assert_eq!(report.source, ReportSource::BmcEvents);
        assert_eq!(report.target, Some(HealthReportTarget::Machine));

        if let Some(alert) = report.alerts.first() {
            return IntrusionReportSummary {
                alert_count: report.alerts.len(),
                success_count: report.successes.len(),
                probe_id: alert.probe_id,
                target: alert.target.clone(),
                message: Some(alert.message.clone()),
                classifications: alert.classifications.clone(),
            };
        }

        let success = report.successes.first().expect("success report");
        IntrusionReportSummary {
            alert_count: report.alerts.len(),
            success_count: report.successes.len(),
            probe_id: success.probe_id,
            target: success.target.clone(),
            message: None,
            classifications: Vec::new(),
        }
    }

    #[test]
    fn intrusion_report_cases() {
        value_scenarios!(
            run = summarize_report;
            "physical chassis intrusion log body" {
                IntrusionLogCase {
                    body: "Physical Chassis Intrusion Alert",
                    severity: LogSeverity::Fatal,
                    message_args: None,
                } => IntrusionReportSummary {
                    alert_count: 1,
                    success_count: 0,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: Some(INTRUSION_ALERT_MESSAGE.to_string()),
                    classifications: vec![
                        Classification::SensorCritical,
                        Classification::PreventAllocations,
                    ],
                },
            }

            "intrusion message args" {
                IntrusionLogCase {
                    body: "",
                    severity: LogSeverity::Warn,
                    message_args: Some(r#"["Physical Chassis Intrusion Alert"]"#),
                } => IntrusionReportSummary {
                    alert_count: 1,
                    success_count: 0,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: Some(INTRUSION_ALERT_MESSAGE.to_string()),
                    classifications: vec![
                        Classification::SensorCritical,
                        Classification::PreventAllocations,
                    ],
                },
            }

            "normal to critical intrusion event" {
                IntrusionLogCase {
                    body: "Physical Chassis Intrusion changed from Normal to Critical",
                    severity: LogSeverity::Unspecified,
                    message_args: None,
                } => IntrusionReportSummary {
                    alert_count: 1,
                    success_count: 0,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: Some(INTRUSION_ALERT_MESSAGE.to_string()),
                    classifications: vec![
                        Classification::SensorCritical,
                        Classification::PreventAllocations,
                    ],
                },
            }

            "cleared intrusion event" {
                IntrusionLogCase {
                    body: "Physical Chassis Intrusion cleared",
                    severity: LogSeverity::Warn,
                    message_args: None,
                } => IntrusionReportSummary {
                    alert_count: 0,
                    success_count: 1,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: None,
                    classifications: vec![],
                },
            }

            "reset intrusion event" {
                IntrusionLogCase {
                    body: "Reset Intrusion",
                    severity: LogSeverity::Info,
                    message_args: None,
                } => IntrusionReportSummary {
                    alert_count: 0,
                    success_count: 1,
                    probe_id: Probe::IntrusionSensorTriggered,
                    target: Some(HOST_BMC_TARGET.to_string()),
                    message: None,
                    classifications: vec![],
                },
            }
        );
    }

    #[test]
    fn ignores_unrelated_logs() {
        let processor = BmcIntrusionEventProcessor::new();
        let emitted = processor.process_event(
            &context(),
            &log("CPU temperature threshold warning", LogSeverity::Warn, None),
        );

        assert!(emitted.is_empty());
    }
}
