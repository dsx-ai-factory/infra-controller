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
use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use model::site_explorer::{ExplorationReportWarning, ExploredEndpoint};

const EXPLORATION_WARNING_REPORT_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
/// Tentative maximum number of endpoints reported per iteration. New and
/// changed warnings are always reported and can cause this limit to be exceeded.
const EXPLORATION_WARNING_MAX_LOGGED_ENDPOINTS_PER_ITERATION: usize = 10;

pub(super) trait ExplorationWarningReporter: Send + Sync {
    fn report(
        &self,
        reason: ExplorationWarningReportReason,
        bmc_ip: IpAddr,
        warning: &ExplorationReportWarning,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExplorationWarningReportReason {
    NewlyDetected,
    PeriodicUpdate,
}

impl fmt::Display for ExplorationWarningReportReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NewlyDetected => "newly detected",
            Self::PeriodicUpdate => "periodic update",
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TracingExplorationWarningReporter;

impl ExplorationWarningReporter for TracingExplorationWarningReporter {
    fn report(
        &self,
        reason: ExplorationWarningReportReason,
        bmc_ip: IpAddr,
        warning: &ExplorationReportWarning,
    ) {
        match warning {
            ExplorationReportWarning::LocallyAdministeredManagerMac {
                manager_id,
                mac_address,
            } => tracing::warn!(
                target: "carbide_diagnostics::locally_administered_mac",
                bmc_ip_address = %bmc_ip,
                %manager_id,
                eth0_mac_address = %mac_address,
                %reason,
                "manager eth0 MAC is locally-administered (transient pre-sync data?)",
            ),
            ExplorationReportWarning::MissingDpuOobInterface => tracing::warn!(
                bmc_ip_address = %bmc_ip,
                %reason,
                "Error getting OOB interface for the DPU",
            ),
            ExplorationReportWarning::MissingBf4BaseMac => tracing::warn!(
                bmc_ip_address = %bmc_ip,
                %reason,
                "BF4 NDF0 fallback did not provide PF0 base MAC (NIC inventory unavailable/uninitialized?)",
            ),
        }
    }
}

#[derive(Default)]
pub(super) struct ExplorationWarningTracker<R = TracingExplorationWarningReporter> {
    last_reported: HashMap<IpAddr, HashMap<ExplorationReportWarning, Instant>>,
    reporter: R,
}

#[cfg(test)]
impl<R> ExplorationWarningTracker<R> {
    fn new(reporter: R) -> Self {
        Self {
            last_reported: HashMap::new(),
            reporter,
        }
    }
}

impl<R: ExplorationWarningReporter> ExplorationWarningTracker<R> {
    pub(super) fn track_reports(&mut self, now: Instant, reports: &[ExploredEndpoint]) {
        let mut periodic_candidates = Vec::new();
        let mut reported_endpoints = HashSet::new();
        let mut seen_endpoints = HashSet::with_capacity(reports.len());

        for endpoint in reports {
            let bmc_ip = endpoint.address;
            seen_endpoints.insert(bmc_ip);
            let current = endpoint.report.warnings();
            if current.is_empty() {
                self.last_reported.remove(&bmc_ip);
                continue;
            }

            let status = self.last_reported.entry(bmc_ip).or_default();
            status.retain(|warning, _| current.contains(warning));

            for warning in current {
                if let Some(last_reported) = status.get(&warning) {
                    if now.saturating_duration_since(*last_reported)
                        > EXPLORATION_WARNING_REPORT_INTERVAL
                    {
                        periodic_candidates.push((bmc_ip, warning, *last_reported));
                    }
                } else {
                    self.reporter.report(
                        ExplorationWarningReportReason::NewlyDetected,
                        bmc_ip,
                        &warning,
                    );
                    reported_endpoints.insert(bmc_ip);
                    status.insert(warning, now);
                }
            }
        }

        periodic_candidates.sort_by_key(|(bmc_ip, _, last_reported)| (*last_reported, *bmc_ip));
        for (bmc_ip, warning, _) in periodic_candidates {
            if !reported_endpoints.contains(&bmc_ip)
                && reported_endpoints.len()
                    >= EXPLORATION_WARNING_MAX_LOGGED_ENDPOINTS_PER_ITERATION
            {
                continue;
            }
            reported_endpoints.insert(bmc_ip);
            self.reporter.report(
                ExplorationWarningReportReason::PeriodicUpdate,
                bmc_ip,
                &warning,
            );
            if let Some(status) = self.last_reported.get_mut(&bmc_ip) {
                status.insert(warning, now);
            }
        }

        self.last_reported
            .retain(|bmc_ip, _| seen_endpoints.contains(bmc_ip));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use config_version::ConfigVersion;
    use model::site_explorer::{
        ComputerSystem, EndpointExplorationReport, EthernetInterface, Manager, PreingestionState,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingReporter {
        events: Arc<Mutex<Vec<ExplorationWarningReport>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ExplorationWarningReport {
        reason: ExplorationWarningReportReason,
        bmc_ip: IpAddr,
        warning: ExplorationReportWarning,
    }

    impl RecordingReporter {
        fn take(&self) -> Vec<ExplorationWarningReport> {
            self.events
                .lock()
                .expect("recording reporter poisoned")
                .drain(..)
                .collect()
        }
    }

    impl ExplorationWarningReporter for RecordingReporter {
        fn report(
            &self,
            reason: ExplorationWarningReportReason,
            bmc_ip: IpAddr,
            warning: &ExplorationReportWarning,
        ) {
            self.events
                .lock()
                .expect("recording reporter poisoned")
                .push(ExplorationWarningReport {
                    reason,
                    bmc_ip,
                    warning: warning.clone(),
                });
        }
    }

    fn missing_oob_report() -> EndpointExplorationReport {
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn locally_administered_mac_report(mac_address: &str) -> EndpointExplorationReport {
        EndpointExplorationReport {
            managers: vec![Manager {
                id: "BMC_0".to_string(),
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("eth0".to_string()),
                    mac_address: Some(mac_address.parse().expect("valid MAC")),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn explored_endpoint(address: IpAddr, report: EndpointExplorationReport) -> ExploredEndpoint {
        ExploredEndpoint {
            address,
            report,
            report_version: ConfigVersion::initial(),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        }
    }

    #[test]
    fn reports_new_and_periodically_but_not_before_interval() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let bmc_ip = IpAddr::from([192, 0, 2, 1]);
        let reports = vec![explored_endpoint(bmc_ip, missing_oob_report())];

        tracker.track_reports(now, &reports);
        assert_eq!(
            reporter.take(),
            vec![ExplorationWarningReport {
                reason: ExplorationWarningReportReason::NewlyDetected,
                bmc_ip,
                warning: ExplorationReportWarning::MissingDpuOobInterface,
            }]
        );

        tracker.track_reports(now + Duration::from_secs(60), &reports);
        assert!(reporter.take().is_empty());

        tracker.track_reports(
            now + EXPLORATION_WARNING_REPORT_INTERVAL + Duration::from_secs(1),
            &reports,
        );
        assert_eq!(
            reporter.take(),
            vec![ExplorationWarningReport {
                reason: ExplorationWarningReportReason::PeriodicUpdate,
                bmc_ip,
                warning: ExplorationReportWarning::MissingDpuOobInterface,
            }]
        );
    }

    #[test]
    fn reports_warning_again_after_it_disappears() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let bmc_ip = IpAddr::from([192, 0, 2, 2]);
        let warning_report = vec![explored_endpoint(bmc_ip, missing_oob_report())];
        let clean_report = vec![explored_endpoint(
            bmc_ip,
            EndpointExplorationReport::default(),
        )];

        tracker.track_reports(now, &warning_report);
        reporter.take();
        tracker.track_reports(now + Duration::from_secs(1), &clean_report);
        tracker.track_reports(now + Duration::from_secs(2), &warning_report);

        assert_eq!(
            reporter.take(),
            vec![ExplorationWarningReport {
                reason: ExplorationWarningReportReason::NewlyDetected,
                bmc_ip,
                warning: ExplorationReportWarning::MissingDpuOobInterface,
            }]
        );
    }

    #[test]
    fn reports_warning_again_after_endpoint_disappears() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let bmc_ip = IpAddr::from([192, 0, 2, 4]);
        let reports = vec![explored_endpoint(bmc_ip, missing_oob_report())];

        tracker.track_reports(now, &reports);
        reporter.take();
        tracker.track_reports(now + Duration::from_secs(1), &[]);
        tracker.track_reports(now + Duration::from_secs(2), &reports);

        assert_eq!(
            reporter.take(),
            vec![ExplorationWarningReport {
                reason: ExplorationWarningReportReason::NewlyDetected,
                bmc_ip,
                warning: ExplorationReportWarning::MissingDpuOobInterface,
            }]
        );
    }

    #[test]
    fn reports_changed_warning_payload_immediately() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let bmc_ip = IpAddr::from([192, 0, 2, 3]);
        let first = vec![explored_endpoint(
            bmc_ip,
            locally_administered_mac_report("02:00:00:00:00:01"),
        )];
        let changed = vec![explored_endpoint(
            bmc_ip,
            locally_administered_mac_report("02:00:00:00:00:02"),
        )];

        tracker.track_reports(now, &first);
        reporter.take();
        tracker.track_reports(now + Duration::from_secs(1), &changed);

        assert_eq!(
            reporter.take(),
            vec![ExplorationWarningReport {
                reason: ExplorationWarningReportReason::NewlyDetected,
                bmc_ip,
                warning: ExplorationReportWarning::LocallyAdministeredManagerMac {
                    manager_id: "BMC_0".to_string(),
                    mac_address: "02:00:00:00:00:02".parse().unwrap(),
                },
            }]
        );
    }

    #[test]
    fn prioritizes_new_warnings_over_periodic_updates() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let periodic_endpoints: Vec<_> = (1..=12)
            .map(|last_octet| {
                explored_endpoint(IpAddr::from([192, 0, 2, last_octet]), missing_oob_report())
            })
            .collect();

        tracker.track_reports(now, &periodic_endpoints);
        reporter.take();

        let mut current = periodic_endpoints;
        current.extend([
            explored_endpoint(IpAddr::from([192, 0, 2, 100]), missing_oob_report()),
            explored_endpoint(IpAddr::from([192, 0, 2, 101]), missing_oob_report()),
        ]);
        tracker.track_reports(
            now + EXPLORATION_WARNING_REPORT_INTERVAL + Duration::from_secs(1),
            &current,
        );

        let events = reporter.take();
        assert_eq!(
            events.len(),
            EXPLORATION_WARNING_MAX_LOGGED_ENDPOINTS_PER_ITERATION
        );
        assert!(
            events[..2]
                .iter()
                .all(|event| event.reason == ExplorationWarningReportReason::NewlyDetected)
        );
        assert!(
            events[2..]
                .iter()
                .all(|event| event.reason == ExplorationWarningReportReason::PeriodicUpdate)
        );
    }

    #[test]
    fn selects_oldest_endpoints_for_periodic_updates() {
        let reporter = RecordingReporter::default();
        let mut tracker = ExplorationWarningTracker::new(reporter.clone());
        let now = Instant::now();
        let mut endpoints: Vec<_> = (1..=12)
            .map(|last_octet| {
                explored_endpoint(IpAddr::from([192, 0, 2, last_octet]), missing_oob_report())
            })
            .collect();

        tracker.track_reports(now, &endpoints);
        reporter.take();
        for (index, endpoint) in endpoints.iter().enumerate() {
            let last_reported = now
                .checked_sub(Duration::from_secs(index as u64))
                .unwrap_or(now);
            tracker
                .last_reported
                .get_mut(&endpoint.address)
                .expect("tracked endpoint should have warning status")
                .insert(
                    ExplorationReportWarning::MissingDpuOobInterface,
                    last_reported,
                );
        }
        endpoints.reverse();

        tracker.track_reports(
            now + EXPLORATION_WARNING_REPORT_INTERVAL + Duration::from_secs(1),
            &endpoints,
        );

        let reported_ips = reporter
            .take()
            .into_iter()
            .map(|event| event.bmc_ip)
            .collect::<Vec<_>>();
        let expected_ips = (3..=12)
            .rev()
            .map(|last_octet| IpAddr::from([192, 0, 2, last_octet]))
            .collect::<Vec<_>>();
        assert_eq!(reported_ips, expected_ips);
    }
}
