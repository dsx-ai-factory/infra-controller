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

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbide_instrument::{Outcome, emit};
use carbide_uuid::machine::MachineId;

use super::dedup_queue::DedupQueue;
use super::{
    CollectorEvent, DataSink, EventContext, HealthReport, HealthReportSubmitted,
    HealthReportTarget, ReportSource,
};
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::config::HealthReportSinkConfig;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HealthReportKey {
    id: MachineId,
    source: ReportSource,
}

struct LastSent {
    content_hash: u64,
    sent_at: Instant,
}

struct LastSentCache {
    entries: HashMap<HealthReportKey, LastSent>,
    last_evicted: Instant,
}

impl LastSentCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            last_evicted: Instant::now(),
        }
    }

    fn evict_if_due(&mut self, evict_after: Duration) {
        if self.last_evicted.elapsed() >= evict_after {
            self.entries
                .retain(|_, v| v.sent_at.elapsed() < evict_after);
            self.last_evicted = Instant::now();
        }
    }
}

pub struct HealthReportSink {
    queue: Arc<DedupQueue<HealthReportKey, Arc<HealthReport>>>,
    skip_empty_reports: bool,
    suppress_unchanged_interval: Option<Duration>,
    last_sent: Mutex<LastSentCache>,
}

fn content_hash(report: &HealthReport) -> u64 {
    use std::collections::BTreeSet;
    let mut hasher = DefaultHasher::new();

    let mut successes = BTreeSet::new();
    for s in &report.successes {
        successes.insert((s.probe_id.as_str(), s.target.as_deref()));
    }
    successes.hash(&mut hasher);

    hasher.finish()
}

impl HealthReportSink {
    pub fn new(config: &HealthReportSinkConfig) -> Result<Self, HealthError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            HealthError::GenericError(format!(
                "health report sink requires active Tokio runtime: {error}"
            ))
        })?;

        let client = Arc::new(ApiClientWrapper::new(
            config.connection.root_ca.clone(),
            config.connection.client_cert.clone(),
            config.connection.client_key.clone(),
            &config.connection.api_url,
        ));

        let queue: Arc<DedupQueue<HealthReportKey, Arc<HealthReport>>> =
            Arc::new(DedupQueue::new());

        for worker_id in 0..config.workers {
            let worker_client = Arc::clone(&client);
            let worker_queue = Arc::clone(&queue);
            handle.spawn(async move {
                loop {
                    let (key, report) = worker_queue.next().await;

                    match report.as_ref().try_into() {
                        Ok(converted) => {
                            let result =
                                worker_client.submit_health_report(&key.id, converted).await;
                            emit(HealthReportSubmitted {
                                target: HealthReportTarget::Machine,
                                outcome: Outcome::from(&result),
                                id: key.id.to_string(),
                                worker_id,
                                error: result.err().map(|e| e.to_string()).unwrap_or_default(),
                            });
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                worker_id,
                                machine_id = %key.id,
                                "Failed to convert health report"
                            );
                        }
                    }
                }
            });
        }

        Ok(Self {
            queue,
            skip_empty_reports: config.skip_empty_reports,
            suppress_unchanged_interval: config.suppress_unchanged_interval,
            last_sent: Mutex::new(LastSentCache::new()),
        })
    }

    #[cfg(feature = "bench-hooks")]
    pub fn new_for_bench() -> Result<Self, HealthError> {
        Ok(Self {
            queue: Arc::new(DedupQueue::new()),
            skip_empty_reports: true,
            suppress_unchanged_interval: None,
            last_sent: Mutex::new(LastSentCache::new()),
        })
    }

    #[cfg(feature = "bench-hooks")]
    pub fn pop_pending_for_bench(&self) -> Option<(MachineId, Arc<HealthReport>)> {
        self.queue.pop().map(|(key, report)| (key.id, report))
    }

    #[cfg(feature = "bench-hooks")]
    pub fn content_hash_for_bench(report: &HealthReport) -> u64 {
        content_hash(report)
    }
}

impl DataSink for HealthReportSink {
    fn sink_type(&self) -> &'static str {
        "health_report_sink"
    }

    fn try_handle_event(
        &self,
        context: &EventContext,
        event: &CollectorEvent,
    ) -> Result<(), HealthError> {
        let CollectorEvent::HealthReport(report) = event else {
            return Ok(());
        };
        if report.target != Some(HealthReportTarget::Machine) {
            return Ok(());
        }

        if self.skip_empty_reports && report.is_empty() {
            tracing::debug!(
                source = ?report.source,
                "Skipping empty machine health report"
            );
            return Ok(());
        }

        if let Some(machine_id) = context.machine_id() {
            let key = HealthReportKey {
                id: machine_id,
                source: report.source,
            };

            if let Some(suppress_interval) = self.suppress_unchanged_interval {
                let mut cache = self.last_sent.lock().expect("last_sent mutex poisoned");
                cache.evict_if_due(suppress_interval * 2);

                if report.alerts.is_empty() {
                    let hash = content_hash(report);
                    if let Some(prev) = cache.entries.get(&key)
                        && prev.content_hash == hash
                        && prev.sent_at.elapsed() < suppress_interval
                    {
                        tracing::debug!(
                            source = ?report.source,
                            machine_id = %key.id,
                            "Suppressing unchanged success-only health report"
                        );
                        return Ok(());
                    }
                    cache.entries.insert(
                        key.clone(),
                        LastSent {
                            content_hash: hash,
                            sent_at: Instant::now(),
                        },
                    );
                } else {
                    // Clear the suppression entry so the first all-clear report
                    // after an alert is never throttled.
                    cache.entries.remove(&key);
                }
            }

            self.queue.save_latest(key, Arc::clone(report));
            Ok(())
        } else {
            tracing::warn!(
                report = ?report,
                "Received machine-target HealthReport event without machine_id context"
            );
            Err(HealthError::GenericError(
                "machine-target health report event without machine_id context".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::str::FromStr;

    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
    use crate::sink::events::{Classification, HealthReportAlert, HealthReportSuccess, Probe};

    fn machine_id(value: &str) -> MachineId {
        value.parse().expect("valid machine id")
    }

    fn key(id: MachineId, source: ReportSource) -> HealthReportKey {
        HealthReportKey { id, source }
    }

    fn report(source: ReportSource) -> Arc<HealthReport> {
        Arc::new(HealthReport {
            source,
            target: None,
            observed_at: None,
            successes: Vec::new(),
            alerts: Vec::new(),
        })
    }

    fn machine_context(id: MachineId) -> EventContext {
        EventContext {
            endpoint_key: "00:00:00:00:00:01".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse::<IpAddr>().unwrap(),
                port: Some(443),
                mac: MacAddress::from_str("00:00:00:00:00:01").unwrap(),
            },
            collector_type: "test",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(id),
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        }
    }

    fn success_report(source: ReportSource) -> Arc<HealthReport> {
        Arc::new(HealthReport {
            source,
            target: Some(HealthReportTarget::Machine),
            observed_at: None,
            successes: vec![HealthReportSuccess {
                attribution: None,
                probe_id: Probe::Sensor,
                target: Some("fan0".to_string()),
            }],
            alerts: Vec::new(),
        })
    }

    fn alert_report(source: ReportSource) -> Arc<HealthReport> {
        Arc::new(HealthReport {
            source,
            target: Some(HealthReportTarget::Machine),
            observed_at: None,
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                attribution: None,
                probe_id: Probe::Sensor,
                target: Some("fan0".to_string()),
                message: "Fan speed critical".to_string(),
                classifications: vec![Classification::SensorCritical],
            }],
        })
    }

    #[tokio::test]
    async fn latest_reports_are_preserved() {
        let queue: DedupQueue<HealthReportKey, Arc<HealthReport>> = DedupQueue::new();
        let machine_a = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let machine_b = machine_id("fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g");
        let machine_c = machine_id("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30");

        queue.save_latest(
            key(machine_a, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );
        queue.save_latest(
            key(machine_a, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );
        queue.save_latest(
            key(machine_b, ReportSource::TrayLeakDetection),
            report(ReportSource::TrayLeakDetection),
        );
        queue.save_latest(
            key(machine_c, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );
        queue.save_latest(
            key(machine_b, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );

        let mut drained = HashMap::new();
        while let Some((k, r)) = queue.pop() {
            drained.insert((k.id, r.source), ());
        }

        assert_eq!(drained.len(), 4);
    }

    #[tokio::test]
    async fn reinserting_hot_key_moves_it_to_back() {
        let queue: DedupQueue<HealthReportKey, Arc<HealthReport>> = DedupQueue::new();
        let machine_a = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let machine_b = machine_id("fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g");

        queue.save_latest(
            key(machine_a, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );
        queue.save_latest(
            key(machine_b, ReportSource::BmcSensors),
            report(ReportSource::BmcSensors),
        );

        let (first_key, _) = queue.pop().unwrap();
        assert_eq!(first_key.id, machine_a);

        queue.save_latest(
            key(machine_a, ReportSource::TrayLeakDetection),
            report(ReportSource::TrayLeakDetection),
        );

        let (second_key, _) = queue.pop().unwrap();
        let (third_key, third_report) = queue.pop().unwrap();

        assert_eq!(second_key.id, machine_b);
        assert_eq!(third_key.id, machine_a);
        assert_eq!(third_report.source, ReportSource::TrayLeakDetection);
    }

    fn make_sink(suppress_unchanged_interval: Option<Duration>) -> HealthReportSink {
        HealthReportSink {
            queue: Arc::new(DedupQueue::new()),
            skip_empty_reports: false,
            suppress_unchanged_interval,
            last_sent: Mutex::new(LastSentCache::new()),
        }
    }

    #[test]
    fn unchanged_success_only_report_suppressed_within_interval() {
        let mid = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let ctx = machine_context(mid);
        let sink = make_sink(Some(Duration::from_secs(300)));
        let report = success_report(ReportSource::BmcSensors);
        let event = CollectorEvent::HealthReport(Arc::clone(&report));

        sink.handle_event(&ctx, &event);
        assert!(sink.queue.pop().is_some(), "first send should go through");

        sink.handle_event(&ctx, &event);
        assert!(
            sink.queue.pop().is_none(),
            "identical repeat within interval should be suppressed"
        );
    }

    #[test]
    fn report_with_alerts_never_suppressed() {
        let mid = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let ctx = machine_context(mid);
        let sink = make_sink(Some(Duration::from_secs(300)));
        let alert = alert_report(ReportSource::BmcSensors);
        let success = success_report(ReportSource::BmcSensors);

        // Send a success first to populate last_sent, then send an alert.
        // The alert must not be suppressed, and the subsequent success must
        // also go through (alert clears the suppression entry).
        sink.handle_event(&ctx, &CollectorEvent::HealthReport(Arc::clone(&success)));
        sink.queue.pop();

        sink.handle_event(&ctx, &CollectorEvent::HealthReport(Arc::clone(&alert)));
        assert!(sink.queue.pop().is_some(), "alert should not be suppressed");

        sink.handle_event(&ctx, &CollectorEvent::HealthReport(Arc::clone(&success)));
        assert!(
            sink.queue.pop().is_some(),
            "first success after alert should not be suppressed"
        );
    }

    #[test]
    fn changed_content_always_forwarded() {
        let mid = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let ctx = machine_context(mid);
        let sink = make_sink(Some(Duration::from_secs(300)));

        let report_a = success_report(ReportSource::BmcSensors);
        sink.handle_event(&ctx, &CollectorEvent::HealthReport(Arc::clone(&report_a)));
        sink.queue.pop();

        let report_b = Arc::new(HealthReport {
            source: ReportSource::BmcSensors,
            target: Some(HealthReportTarget::Machine),
            observed_at: None,
            successes: vec![HealthReportSuccess {
                attribution: None,
                probe_id: Probe::Sensor,
                target: Some("fan1".to_string()),
            }],
            alerts: Vec::new(),
        });
        sink.handle_event(&ctx, &CollectorEvent::HealthReport(Arc::clone(&report_b)));

        assert!(
            sink.queue.pop().is_some(),
            "changed content should bypass suppression"
        );
    }

    #[test]
    fn suppression_disabled_forwards_all_reports() {
        let mid = machine_id("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0");
        let ctx = machine_context(mid);
        let sink = make_sink(None);
        let report = success_report(ReportSource::BmcSensors);
        let event = CollectorEvent::HealthReport(Arc::clone(&report));

        sink.handle_event(&ctx, &event);
        sink.queue.pop();
        sink.handle_event(&ctx, &event);

        assert!(
            sink.queue.pop().is_some(),
            "with suppression disabled all sends should go through"
        );
    }
}
