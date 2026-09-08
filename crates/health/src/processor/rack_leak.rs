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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use carbide_uuid::rack::RackId;
use dashmap::DashMap;

use super::{EventContext, EventProcessor};
use crate::sink::{
    Classification, CollectorEvent, HealthReport, HealthReportAlert, HealthReportSuccess,
    HealthReportTarget, Probe, ReportSource,
};

struct RackLeakState {
    /// Maps each leaking tray's endpoint key to the collector types reporting it.
    /// The tray remains active until every reporting collector retracts its state.
    leaking_trays: HashMap<String, HashSet<&'static str>>,
}

impl RackLeakState {
    fn remove_reporter(&mut self, endpoint_key: &str, collector_type: &'static str) -> bool {
        let last_reporter_removed =
            self.leaking_trays
                .get_mut(endpoint_key)
                .is_some_and(|reporters| {
                    reporters.remove(collector_type);
                    reporters.is_empty()
                });

        if last_reporter_removed {
            self.leaking_trays.remove(endpoint_key);
        }

        last_reporter_removed
    }
}

pub struct RackLeakProcessor {
    racks: DashMap<RackId, RackLeakState>,
    leaking_tray_threshold: usize,
}

impl RackLeakProcessor {
    pub fn new(leaking_tray_threshold: usize) -> Self {
        Self {
            racks: DashMap::new(),
            leaking_tray_threshold,
        }
    }

    fn build_report(&self, leaking_count: usize) -> HealthReport {
        if leaking_count >= self.leaking_tray_threshold {
            HealthReport {
                source: ReportSource::RackLeakDetection,
                target: Some(HealthReportTarget::Rack),
                observed_at: Some(chrono::Utc::now()),
                successes: vec![],
                alerts: vec![HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::LeakDetection,
                    target: None,
                    message: format!(
                        "Rack leak detected: {} leaking trays reached threshold {}",
                        leaking_count, self.leaking_tray_threshold,
                    ),
                    classifications: vec![Classification::Leak],
                }],
            }
        } else {
            HealthReport {
                source: ReportSource::RackLeakDetection,
                target: Some(HealthReportTarget::Rack),
                observed_at: Some(chrono::Utc::now()),
                successes: vec![HealthReportSuccess {
                    attribution: None,
                    probe_id: Probe::LeakDetection,
                    target: None,
                }],
                alerts: vec![],
            }
        }
    }
}

impl EventProcessor for RackLeakProcessor {
    fn processor_type(&self) -> &'static str {
        "rack_leak_processor"
    }

    fn process_event(&self, context: &EventContext, event: &CollectorEvent) -> Vec<CollectorEvent> {
        let Some(rack_id) = context.rack_id() else {
            return Vec::new();
        };

        if matches!(event, CollectorEvent::CollectorRemoved) {
            let Some(mut entry) = self.racks.get_mut(rack_id) else {
                return Vec::new();
            };

            if !entry.remove_reporter(context.endpoint_key(), context.collector_type) {
                return Vec::new();
            }

            let report = self.build_report(entry.leaking_trays.len());

            return vec![CollectorEvent::HealthReport(Arc::new(report))];
        }

        let CollectorEvent::HealthReport(report) = event else {
            return Vec::new();
        };

        if report.source != ReportSource::TrayLeakDetection {
            return Vec::new();
        }

        if !matches!(
            report.target,
            Some(HealthReportTarget::Machine | HealthReportTarget::Switch)
        ) {
            return Vec::new();
        }

        let tray_key = context.endpoint_key().to_owned();
        let is_leaking = !report.alerts.is_empty();

        let mut entry = self
            .racks
            .entry(rack_id.clone())
            .or_insert_with(|| RackLeakState {
                leaking_trays: HashMap::new(),
            });

        if is_leaking {
            entry
                .leaking_trays
                .entry(tray_key)
                .or_default()
                .insert(context.collector_type);
        } else {
            entry.remove_reporter(&tray_key, context.collector_type);
        }

        let leaking_count = entry.leaking_trays.len();
        let report = self.build_report(leaking_count);

        vec![CollectorEvent::HealthReport(Arc::new(report))]
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::BmcAddr;

    fn context_with_rack(mac: &str, rack: &str) -> EventContext {
        EventContext {
            endpoint_key: mac.to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str(mac).expect("valid mac"),
            },
            collector_type: "sensor_collector",
            metadata: None,
            rack_id: Some(RackId::new(rack)),
            labels: Default::default(),
        }
    }

    fn context_without_rack(mac: &str) -> EventContext {
        EventContext {
            endpoint_key: mac.to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str(mac).expect("valid mac"),
            },
            collector_type: "sensor_collector",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn tray_leak_report(leaking: bool) -> CollectorEvent {
        let report = if leaking {
            HealthReport {
                source: ReportSource::TrayLeakDetection,
                target: Some(HealthReportTarget::Machine),
                observed_at: Some(chrono::Utc::now()),
                successes: vec![],
                alerts: vec![HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::LeakDetection,
                    target: None,
                    message: "tray leaking".to_string(),
                    classifications: vec![Classification::Leak],
                }],
            }
        } else {
            HealthReport {
                source: ReportSource::TrayLeakDetection,
                target: Some(HealthReportTarget::Machine),
                observed_at: Some(chrono::Utc::now()),
                successes: vec![HealthReportSuccess {
                    attribution: None,
                    probe_id: Probe::LeakDetection,
                    target: None,
                }],
                alerts: vec![],
            }
        };
        CollectorEvent::HealthReport(Arc::new(report))
    }

    #[test]
    fn ignores_non_tray_leak_reports() {
        let processor = RackLeakProcessor::new(2);
        let ctx = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let report = HealthReport {
            source: ReportSource::BmcSensors,
            target: Some(HealthReportTarget::Machine),
            observed_at: None,
            successes: vec![],
            alerts: vec![],
        };
        let emitted =
            processor.process_event(&ctx, &CollectorEvent::HealthReport(Arc::new(report)));
        assert!(emitted.is_empty());
    }

    #[test]
    fn ignores_events_without_rack_id() {
        let processor = RackLeakProcessor::new(2);
        let ctx = context_without_rack("42:9e:b1:bd:9d:dd");
        let emitted = processor.process_event(&ctx, &tray_leak_report(true));
        assert!(emitted.is_empty());
    }

    #[test]
    fn emits_success_below_threshold() {
        let processor = RackLeakProcessor::new(2);
        let ctx = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");

        let emitted = processor.process_event(&ctx, &tray_leak_report(true));
        assert_eq!(emitted.len(), 1);

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };
        assert_eq!(report.source, ReportSource::RackLeakDetection);
        assert_eq!(report.target, Some(HealthReportTarget::Rack));
        assert!(report.alerts.is_empty());
        assert_eq!(report.successes.len(), 1);
    }

    #[test]
    fn counts_switch_target_tray_leak_reports() {
        let processor = RackLeakProcessor::new(1);
        let ctx = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");

        let event = CollectorEvent::HealthReport(Arc::new(HealthReport {
            source: ReportSource::TrayLeakDetection,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes: vec![],
            alerts: vec![HealthReportAlert {
                attribution: None,
                probe_id: Probe::LeakDetection,
                target: None,
                message: "switch leaking".to_string(),
                classifications: vec![Classification::Leak],
            }],
        }));

        let emitted = processor.process_event(&ctx, &event);

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };

        assert_eq!(report.source, ReportSource::RackLeakDetection);
        assert_eq!(report.target, Some(HealthReportTarget::Rack));
        assert_eq!(report.alerts.len(), 1);
    }

    #[test]
    fn emits_alert_at_threshold() {
        let processor = RackLeakProcessor::new(2);

        let ctx_a = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let ctx_b = context_with_rack("42:9e:b1:bd:9d:ee", "rack-1");

        processor.process_event(&ctx_a, &tray_leak_report(true));
        let emitted = processor.process_event(&ctx_b, &tray_leak_report(true));

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };
        assert_eq!(report.source, ReportSource::RackLeakDetection);
        assert_eq!(report.target, Some(HealthReportTarget::Rack));
        assert_eq!(report.alerts.len(), 1);
        assert!(report.alerts[0].message.contains("2 leaking trays"));
    }

    #[test]
    fn clears_alert_when_tray_reports_no_leak() {
        let processor = RackLeakProcessor::new(2);

        let ctx_a = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let ctx_b = context_with_rack("42:9e:b1:bd:9d:ee", "rack-1");

        processor
            .process_event(&ctx_a, &tray_leak_report(true))
            .len();
        processor.process_event(&ctx_b, &tray_leak_report(true));

        let emitted = processor.process_event(&ctx_a, &tray_leak_report(false));

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };
        assert!(report.alerts.is_empty());
        assert_eq!(report.successes.len(), 1);
    }

    #[test]
    fn silent_tray_retains_leak_state() {
        let processor = RackLeakProcessor::new(2);

        let ctx_a = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let ctx_b = context_with_rack("42:9e:b1:bd:9d:ee", "rack-1");
        let ctx_c = context_with_rack("42:9e:b1:bd:9d:ff", "rack-1");

        processor.process_event(&ctx_a, &tray_leak_report(true));
        processor.process_event(&ctx_b, &tray_leak_report(true));

        let emitted = processor.process_event(&ctx_c, &tray_leak_report(false));

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };
        assert_eq!(report.alerts.len(), 1, "rack should still be in alert");
    }

    #[test]
    fn removed_tray_is_no_longer_counted() {
        let processor = RackLeakProcessor::new(2);

        let ctx_a = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let ctx_b = context_with_rack("42:9e:b1:bd:9d:ee", "rack-1");

        processor.process_event(&ctx_a, &tray_leak_report(true));
        processor.process_event(&ctx_b, &tray_leak_report(true));

        let emitted = processor.process_event(&ctx_a, &CollectorEvent::CollectorRemoved);

        let Some(CollectorEvent::HealthReport(report)) = emitted.first() else {
            panic!("expected updated rack health report");
        };

        assert!(report.alerts.is_empty());
        assert_eq!(report.successes.len(), 1);

        let Some(rack) = processor.racks.get(ctx_a.rack_id().expect("rack id")) else {
            panic!("expected rack state");
        };

        assert_eq!(rack.leaking_trays.len(), 1);
        assert!(rack.leaking_trays.contains_key(ctx_b.endpoint_key()));
    }

    #[test]
    fn unrelated_collector_removal_keeps_tray_state() {
        let processor = RackLeakProcessor::new(1);
        let leak_context = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let mut unrelated_context = leak_context.clone();
        unrelated_context.collector_type = "unrelated_collector";

        processor.process_event(&leak_context, &tray_leak_report(true));

        let emitted =
            processor.process_event(&unrelated_context, &CollectorEvent::CollectorRemoved);

        assert!(emitted.is_empty());

        let Some(rack) = processor
            .racks
            .get(leak_context.rack_id().expect("rack id"))
        else {
            panic!("expected rack state");
        };

        assert!(rack.leaking_trays.contains_key(leak_context.endpoint_key()));
    }

    #[test]
    fn tray_state_remains_until_every_reporting_collector_retracts() {
        let processor = RackLeakProcessor::new(1);
        let first_context = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let mut second_context = first_context.clone();
        second_context.collector_type = "second_leak_collector";

        processor.process_event(&first_context, &tray_leak_report(true));
        processor.process_event(&second_context, &tray_leak_report(true));

        let emitted = processor.process_event(&second_context, &tray_leak_report(false));

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };

        assert_eq!(report.alerts.len(), 1);

        {
            let rack = processor
                .racks
                .get(first_context.rack_id().expect("rack id"))
                .expect("expected rack state");

            let reporters = rack
                .leaking_trays
                .get(first_context.endpoint_key())
                .expect("expected leaking tray state");

            assert_eq!(reporters.len(), 1);
            assert!(reporters.contains(first_context.collector_type));
        }

        processor.process_event(&second_context, &tray_leak_report(true));

        let emitted = processor.process_event(&first_context, &CollectorEvent::CollectorRemoved);

        assert!(emitted.is_empty());

        {
            let rack = processor
                .racks
                .get(first_context.rack_id().expect("rack id"))
                .expect("expected rack state");

            let reporters = rack
                .leaking_trays
                .get(first_context.endpoint_key())
                .expect("expected leaking tray state");

            assert_eq!(reporters.len(), 1);
            assert!(reporters.contains(second_context.collector_type));
        }

        processor.process_event(&second_context, &CollectorEvent::CollectorRemoved);

        let rack = processor
            .racks
            .get(first_context.rack_id().expect("rack id"))
            .expect("expected rack state");

        assert!(
            !rack
                .leaking_trays
                .contains_key(first_context.endpoint_key())
        );
    }

    #[test]
    fn separate_racks_are_independent() {
        let processor = RackLeakProcessor::new(2);

        let ctx_r1 = context_with_rack("42:9e:b1:bd:9d:dd", "rack-1");
        let ctx_r2 = context_with_rack("42:9e:b1:bd:9d:ee", "rack-2");

        processor.process_event(&ctx_r1, &tray_leak_report(true));
        let emitted = processor.process_event(&ctx_r2, &tray_leak_report(true));

        let CollectorEvent::HealthReport(report) = &emitted[0] else {
            panic!("expected health report");
        };

        assert!(report.alerts.is_empty());
    }
}
