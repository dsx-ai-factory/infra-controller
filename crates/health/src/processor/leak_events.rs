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

use std::collections::BTreeSet;
use std::sync::Arc;

use super::{EventContext, EventProcessor};
use crate::sink::{
    Classification, CollectorEvent, HealthReport, HealthReportAlert, HealthReportSuccess,
    HealthReportTarget, Probe, ReportSource,
};

pub struct LeakEventProcessor {
    minimum_alerts_per_report: usize,
}

impl LeakEventProcessor {
    pub fn new(minimum_alerts_per_report: usize) -> Self {
        Self {
            minimum_alerts_per_report,
        }
    }

    fn is_leaking(&self, alerts: usize) -> bool {
        alerts >= self.minimum_alerts_per_report
    }
}

fn leak_details(alerts: &[&HealthReportAlert]) -> String {
    let targets: BTreeSet<String> = alerts
        .iter()
        .filter_map(|alert| alert.target.as_ref().cloned())
        .collect();

    if targets.is_empty() {
        return "unknown".to_string();
    }

    targets.iter().cloned().collect::<Vec<_>>().join(", ")
}

impl EventProcessor for LeakEventProcessor {
    fn processor_type(&self) -> &'static str {
        "leak_event_processor"
    }

    fn process_event(
        &self,
        _context: &EventContext,
        event: &CollectorEvent,
    ) -> Vec<CollectorEvent> {
        let CollectorEvent::HealthReport(report) = event else {
            return Vec::new();
        };

        // Normalize each source's leak signal into the derived tray leak report
        // consumed by rack leak aggregation.
        let (target, leak_classification, alert_kind, detail_kind) = match report.source {
            ReportSource::BmcLeakDetectors => (
                HealthReportTarget::Machine,
                Classification::LeakDetector,
                "leak-detector",
                "detectors",
            ),
            ReportSource::NvueLeakage => (
                HealthReportTarget::Switch,
                Classification::Leak,
                "nvue-leakage",
                "leakage sensors",
            ),
            _ => return Vec::new(),
        };

        let leak_alerts: Vec<&HealthReportAlert> = report
            .alerts
            .iter()
            .filter(|alert| alert.classifications.contains(&leak_classification))
            .collect();

        let leaking = self.is_leaking(leak_alerts.len());

        // An alert that is not a leak alert is a read failure: an unavailable
        // NVUE endpoint, an unknown sensor state, a detector this client could
        // not decode. The sensors behind it have unknown state, so a report
        // carrying one can raise a leak but can never clear one. Deriving an
        // all-clear here would retract a machine or rack leak on evidence that
        // never arrived, and a read failure also leaves the alert count below
        // the threshold it would otherwise have met.
        if !leaking && leak_alerts.len() < report.alerts.len() {
            return Vec::new();
        }

        let (successes, alerts) = if leaking {
            let details = leak_details(&leak_alerts);

            (
                vec![],
                vec![HealthReportAlert {
                    probe_id: Probe::LeakDetection,
                    target: None,
                    message: format!(
                        "Leak detected: {} {} alerts reached threshold {} ({}: {})",
                        leak_alerts.len(),
                        alert_kind,
                        self.minimum_alerts_per_report,
                        detail_kind,
                        details
                    ),
                    classifications: vec![Classification::Leak],
                }],
            )
        } else {
            (
                vec![HealthReportSuccess {
                    probe_id: Probe::LeakDetection,
                    target: None,
                }],
                vec![],
            )
        };

        let leak_report = HealthReport {
            source: ReportSource::TrayLeakDetection,
            target: Some(target),
            observed_at: Some(chrono::Utc::now()),
            successes,
            alerts,
        };

        vec![CollectorEvent::HealthReport(Arc::new(leak_report))]
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;

    fn context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "leak_detector_collector",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn leak_alert(target: &str) -> HealthReportAlert {
        HealthReportAlert {
            probe_id: Probe::LeakDetection,
            target: Some(target.to_string()),
            message: "LeakDetector found leak".to_string(),
            classifications: vec![Classification::LeakDetector],
        }
    }

    #[test]
    fn does_not_emit_alert_when_threshold_not_met() {
        let processor = LeakEventProcessor::new(2);
        let report = HealthReport {
            source: ReportSource::BmcLeakDetectors,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![leak_alert("LeakDetector_Probe")],
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));
        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(derived) = &emitted[0] else {
            panic!("expected derived health report");
        };

        assert_eq!(derived.target, Some(HealthReportTarget::Machine));
        assert_eq!(derived.alerts.len(), 0);
        assert_eq!(derived.successes.len(), 1);
        assert_eq!(derived.successes[0].probe_id, Probe::LeakDetection);
    }

    #[test]
    fn emits_derived_leak_report_when_threshold_met() {
        let processor = LeakEventProcessor::new(1);
        let report = HealthReport {
            source: ReportSource::BmcLeakDetectors,
            target: Some(HealthReportTarget::Machine),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![leak_alert("LeakDetector_Probe")],
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));
        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(derived) = &emitted[0] else {
            panic!("expected derived health report");
        };
        assert_eq!(derived.source, ReportSource::TrayLeakDetection);
        assert_eq!(derived.target, Some(HealthReportTarget::Machine));
        assert_eq!(derived.alerts.len(), 1);
        assert_eq!(derived.alerts[0].probe_id, Probe::LeakDetection);
        assert!(
            derived.alerts[0]
                .classifications
                .iter()
                .any(|classification| classification == &Classification::Leak)
        );
    }

    #[test]
    fn emits_switch_leak_report_for_nvue_leakage() {
        let processor = LeakEventProcessor::new(1);

        let report = HealthReport {
            source: ReportSource::NvueLeakage,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                probe_id: Probe::NvueLeakage,
                target: Some("LEAK1".to_string()),
                message: "NVUE leakage sensor state".to_string(),
                classifications: vec![Classification::Leak],
            }],
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));

        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(derived) = &emitted[0] else {
            panic!("expected derived health report");
        };

        assert_eq!(derived.source, ReportSource::TrayLeakDetection);
        assert_eq!(derived.target, Some(HealthReportTarget::Switch));
        assert_eq!(derived.alerts.len(), 1);

        assert!(
            derived.alerts[0]
                .classifications
                .contains(&Classification::Leak)
        );
    }

    #[test]
    fn emits_switch_success_for_nvue_leakage_clear_report() {
        let processor = LeakEventProcessor::new(1);

        let report = HealthReport {
            source: ReportSource::NvueLeakage,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes: vec![HealthReportSuccess {
                probe_id: Probe::NvueLeakage,
                target: Some("LEAK1".to_string()),
            }],
            alerts: Vec::new(),
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));

        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(derived) = &emitted[0] else {
            panic!("expected derived health report");
        };

        assert_eq!(derived.target, Some(HealthReportTarget::Switch));
        assert!(derived.alerts.is_empty());
        assert_eq!(derived.successes.len(), 1);
        assert_eq!(derived.successes[0].probe_id, Probe::LeakDetection);
    }

    #[test]
    fn ignores_nvue_leakage_failure_without_leak_alert() {
        let processor = LeakEventProcessor::new(1);

        let report = HealthReport {
            source: ReportSource::NvueLeakage,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                probe_id: Probe::NvueLeakage,
                target: Some("LEAK1".to_string()),
                message: "NVUE leakage sensor state".to_string(),
                classifications: vec![Classification::SensorFailure],
            }],
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));

        assert!(emitted.is_empty());
    }

    fn unreadable_detector_alert(target: &str) -> HealthReportAlert {
        HealthReportAlert {
            probe_id: Probe::LeakDetection,
            target: Some(target.to_string()),
            message: format!("Leak detector '{target}' could not be read"),
            classifications: vec![Classification::SensorFailure],
        }
    }

    /// An incomplete read can raise a leak but must never clear one. A detector
    /// the collector could not decode leaves that detector's state unknown, so
    /// the surviving alert count is not evidence that the tray is dry.
    #[test]
    fn incomplete_read_never_derives_an_all_clear() {
        check_values(
            [
                Check {
                    scenario: "a complete read below threshold is an all-clear",
                    input: vec![leak_alert("Detector_0")],
                    expect: Some("all-clear"),
                },
                Check {
                    scenario: "an unreadable detector alone does not clear the tray",
                    input: vec![unreadable_detector_alert("Detector_1")],
                    expect: None,
                },
                Check {
                    scenario: "an unreadable detector can hold the count below threshold",
                    input: vec![
                        leak_alert("Detector_0"),
                        unreadable_detector_alert("Detector_1"),
                    ],
                    expect: None,
                },
                Check {
                    scenario: "an incomplete read still raises a leak that meets threshold",
                    input: vec![
                        leak_alert("Detector_0"),
                        leak_alert("Detector_2"),
                        unreadable_detector_alert("Detector_1"),
                    ],
                    expect: Some("leak"),
                },
            ],
            |alerts| {
                let processor = LeakEventProcessor::new(2);
                let report = HealthReport {
                    source: ReportSource::BmcLeakDetectors,
                    target: Some(HealthReportTarget::Machine),
                    observed_at: Some(chrono::Utc::now()),
                    successes: Vec::new(),
                    alerts,
                };

                let emitted = processor
                    .process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));

                let [CollectorEvent::HealthReport(derived)] = emitted.as_slice() else {
                    return None;
                };
                Some(if derived.alerts.is_empty() {
                    "all-clear"
                } else {
                    "leak"
                })
            },
        );
    }

    #[test]
    fn ignores_non_health_report_events() {
        let processor = LeakEventProcessor::new(1);
        let metric_event = CollectorEvent::Metric(
            crate::sink::MetricSample {
                key: "k".to_string(),
                name: "n".to_string(),
                metric_type: "gauge".to_string(),
                unit: "count".to_string(),
                value: 1.0,
                labels: Vec::new(),
                context: None,
            }
            .into(),
        );
        let emitted = processor.process_event(&context(), &metric_event);
        assert!(emitted.is_empty());
    }

    #[test]
    fn ignores_sensor_health_reports() {
        let processor = LeakEventProcessor::new(1);
        let report = HealthReport {
            source: ReportSource::BmcSensors,
            observed_at: Some(chrono::Utc::now()),
            successes: vec![HealthReportSuccess {
                probe_id: Probe::Sensor,
                target: Some("Voltage_1".to_string()),
            }],
            alerts: vec![],
            target: Some(HealthReportTarget::Machine),
        };

        let emitted =
            processor.process_event(&context(), &CollectorEvent::HealthReport(Arc::new(report)));

        assert!(emitted.is_empty());
    }
}
