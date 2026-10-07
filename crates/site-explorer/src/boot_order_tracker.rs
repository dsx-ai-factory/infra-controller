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

use carbide_uuid::machine::MachineId;
use model::site_explorer::{
    BootOrder, ComputerSystem, EndpointExplorationReport, ExploredManagedHost,
};

pub(super) trait BootOrderReporter: Send + Sync {
    fn report(
        &self,
        reason: BootOrderReportReason,
        bmc_ip: IpAddr,
        machine: &Option<MachineId>,
        systems: &[ComputerSystem],
    );
}

/// Time between boot order tracker logging messages if nothing has
/// changed. This is needed for hosts where nothing is changing for
/// long period of time.
const BOOT_ORDER_TRACKER_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
/// Tentative maximum number of hosts reported per iteration. This
/// value can be exceeded when boot order change is detected.
const BOOT_ORDER_MAX_LOGGED_HOST_PER_ITERATION: u32 = 10;

/// Object that tracks and logs boot order on each explored host.
/// Boot order is logged each BOOT_ORDER_TRACKER_INTERVAL and anytime
/// when change in orde has been detected.
///
/// In future object can be part of BootOrderController that
/// reconciliates boot order expected by managed host state machine
/// and H/W.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BootOrderReportReason {
    ChangeDetected,
    PeriodicUpdate,
    NewHost,
}

impl fmt::Display for BootOrderReportReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BootOrderReportReason::ChangeDetected => "change detected",
            BootOrderReportReason::PeriodicUpdate => "periodic update",
            BootOrderReportReason::NewHost => "new host",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TracingBootOrderReporter;

impl BootOrderReporter for TracingBootOrderReporter {
    fn report(
        &self,
        reason: BootOrderReportReason,
        bmc_ip: IpAddr,
        machine: &Option<MachineId>,
        systems: &[ComputerSystem],
    ) {
        let machine_id = machine.as_ref().map(ToString::to_string);
        let boot_orders = BootOrderDisplayProxy { systems };

        tracing::info!(
            bmc_ip_address = %bmc_ip,
            machine_id = ?machine_id,
            ?boot_orders,
            reason = %reason,
            "Boot order tracker report"
        );
    }
}

pub(super) struct BootOrderTracker<R = TracingBootOrderReporter> {
    cached_boot_order: HashMap<IpAddr, BootOrderStatus>,
    reporter: R,
}

impl<R: Default> Default for BootOrderTracker<R> {
    fn default() -> Self {
        Self {
            cached_boot_order: HashMap::new(),
            reporter: R::default(),
        }
    }
}

#[cfg(test)]
impl<R> BootOrderTracker<R> {
    fn new(reporter: R) -> Self {
        Self {
            cached_boot_order: HashMap::new(),
            reporter,
        }
    }
}

impl<R: BootOrderReporter> BootOrderTracker<R> {
    pub(super) fn track_hosts(
        &mut self,
        now: Instant,
        reports: &[(ExploredManagedHost, EndpointExplorationReport)],
    ) {
        let mut num_reported = 0;
        let mut seen_hosts = HashSet::with_capacity(reports.len());
        let mut indices: Vec<usize> = (0..reports.len()).collect();
        // Sort indices in reports array prioritizing oldest report first.
        indices.sort_by_key(|&i| {
            let (host, _) = &reports[i];
            self.cached_boot_order
                .get(&host.host_bmc_ip)
                .map(|s| s.report_time)
        });

        for idx in indices {
            let (h, r) = &reports[idx];
            seen_hosts.insert(h.host_bmc_ip);
            if let Some(status) = self.cached_boot_order.get_mut(&h.host_bmc_ip) {
                if status.boot_order_updated(&r.systems) {
                    self.reporter.report(
                        BootOrderReportReason::ChangeDetected,
                        h.host_bmc_ip,
                        &r.machine_id,
                        &r.systems,
                    );
                    num_reported += 1;
                    status.report_time = now;
                } else if now - status.report_time > BOOT_ORDER_TRACKER_INTERVAL
                    && num_reported < BOOT_ORDER_MAX_LOGGED_HOST_PER_ITERATION
                {
                    self.reporter.report(
                        BootOrderReportReason::PeriodicUpdate,
                        h.host_bmc_ip,
                        &r.machine_id,
                        &r.systems,
                    );
                    num_reported += 1;
                    status.report_time = now;
                }
            } else if Self::is_eligible_for_tracking(&r.systems) {
                self.reporter.report(
                    BootOrderReportReason::NewHost,
                    h.host_bmc_ip,
                    &r.machine_id,
                    &r.systems,
                );
                num_reported += 1;
                let status = BootOrderStatus::new(now, &r.systems);
                self.cached_boot_order.insert(h.host_bmc_ip, status);
            }
        }

        // Remove cached entries for hosts that are no longer reported.
        self.cached_boot_order
            .retain(|ip, _| seen_hosts.contains(ip));
    }

    // In some cases site explorer cannot find boot order for the
    // machine. We prevent from tracking these machines and create
    // additional noise in logs. Eligible for tracking are machines
    // that have at least one system with at least one boot option in
    // boot order.
    fn is_eligible_for_tracking(systems: &[ComputerSystem]) -> bool {
        systems.iter().any(|s| {
            s.boot_order
                .as_ref()
                .is_some_and(|order| !order.boot_order.is_empty())
        })
    }
}

struct BootOrderStatus {
    report_time: std::time::Instant,
    boot_order: HashMap<String, Option<BootOrder>>,
}

impl BootOrderStatus {
    fn new(report_time: std::time::Instant, systems: &[ComputerSystem]) -> Self {
        BootOrderStatus {
            report_time,
            boot_order: Self::collect_boot_order(systems),
        }
    }

    fn boot_order_updated(&mut self, systems: &[ComputerSystem]) -> bool {
        if systems.len() != self.boot_order.len() {
            self.boot_order = Self::collect_boot_order(systems);
            return true;
        }

        let mut seen_ids = HashSet::with_capacity(systems.len());
        for system in systems {
            // Duplicate IDs mean we cannot reliably compare entries; treat as change.
            if !seen_ids.insert(system.id.as_str()) {
                self.boot_order = Self::collect_boot_order(systems);
                return true;
            }

            match self.boot_order.get(&system.id) {
                Some(cached) if cached.as_ref() == system.boot_order.as_ref() => {}
                _ => {
                    self.boot_order = Self::collect_boot_order(systems);
                    return true;
                }
            }
        }

        false
    }

    fn collect_boot_order(systems: &[ComputerSystem]) -> HashMap<String, Option<BootOrder>> {
        systems
            .iter()
            .map(|system| (system.id.clone(), system.boot_order.clone()))
            .collect()
    }
}

/// Proxy type used solely for logging/printing boot order without exposing full structs.
struct BootOrderDisplayProxy<'a> {
    systems: &'a [ComputerSystem],
}

impl fmt::Debug for BootOrderDisplayProxy<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut per_system: Vec<(&str, Vec<String>)> = Vec::with_capacity(self.systems.len());

        for system in self.systems {
            let mut entries = Vec::new();
            if let Some(order) = &system.boot_order {
                for option in &order.boot_order {
                    let state = match option.boot_option_enabled {
                        Some(true) => "On",
                        Some(false) => "Off",
                        None => "?",
                    };
                    entries.push(format!("{}:{}:{}", option.id, option.display_name, state));
                }
            }
            per_system.push((system.id.as_str(), entries));
        }

        f.debug_map()
            .entries(per_system.iter().map(|(id, entries)| (id, entries)))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};

    use model::site_explorer::BootOption;

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingReporter {
        events: Arc<Mutex<Vec<BootOrderReport>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct BootOrderReport {
        reason: BootOrderReportReason,
        bmc_ip: IpAddr,
        machine_id: Option<MachineId>,
    }

    impl RecordingReporter {
        fn take(&self) -> Vec<BootOrderReport> {
            let mut guard = self.events.lock().expect("recording reporter poisoned");
            guard.drain(..).collect()
        }
    }

    impl BootOrderReporter for RecordingReporter {
        fn report(
            &self,
            reason: BootOrderReportReason,
            bmc_ip: IpAddr,
            machine: &Option<MachineId>,
            _systems: &[ComputerSystem],
        ) {
            self.events.lock().unwrap().push(BootOrderReport {
                reason,
                bmc_ip,
                machine_id: *machine,
            });
        }
    }

    fn sample_system(id: &str, enabled: bool) -> ComputerSystem {
        ComputerSystem {
            id: id.to_string(),
            boot_order: Some(BootOrder {
                boot_order: vec![BootOption {
                    display_name: "PXE".to_string(),
                    id: "1".to_string(),
                    boot_option_enabled: Some(enabled),
                    uefi_device_path: None,
                }],
            }),
            ..Default::default()
        }
    }

    fn make_report(
        ip: IpAddr,
        system: ComputerSystem,
    ) -> (ExploredManagedHost, EndpointExplorationReport) {
        (
            ExploredManagedHost {
                host_bmc_ip: ip,
                dpus: Vec::new(),
            },
            EndpointExplorationReport {
                systems: vec![system],
                ..Default::default()
            },
        )
    }

    #[test]
    fn eligibility_requires_boot_order_entries() {
        let system_without_boot_order = ComputerSystem {
            id: "sys0".to_string(),
            boot_order: None,
            ..Default::default()
        };
        let system_with_empty_boot_order = ComputerSystem {
            id: "sys1".to_string(),
            boot_order: Some(BootOrder {
                boot_order: Vec::new(),
            }),
            ..Default::default()
        };
        let valid_system = sample_system("sys2", true);

        assert!(
            !BootOrderTracker::<RecordingReporter>::is_eligible_for_tracking(&[
                system_without_boot_order
            ])
        );
        assert!(
            !BootOrderTracker::<RecordingReporter>::is_eligible_for_tracking(std::slice::from_ref(
                &system_with_empty_boot_order
            ))
        );

        // Presence of at least one system with boot options makes the host eligible.
        assert!(
            BootOrderTracker::<RecordingReporter>::is_eligible_for_tracking(&[
                system_with_empty_boot_order,
                valid_system.clone()
            ])
        );
        assert!(BootOrderTracker::<RecordingReporter>::is_eligible_for_tracking(&[valid_system]));
    }

    #[test]
    fn skips_ineligible_hosts() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();

        let ineligible_none = ComputerSystem {
            id: "none".to_string(),
            boot_order: None,
            ..Default::default()
        };
        let ineligible_empty = ComputerSystem {
            id: "empty".to_string(),
            boot_order: Some(BootOrder {
                boot_order: Vec::new(),
            }),
            ..Default::default()
        };
        let eligible = sample_system("ok", true);

        let report_ineligible_none =
            make_report(IpAddr::from_str("192.0.2.60").unwrap(), ineligible_none);
        let report_ineligible_empty =
            make_report(IpAddr::from_str("192.0.2.61").unwrap(), ineligible_empty);
        let report_eligible = make_report(IpAddr::from_str("192.0.2.62").unwrap(), eligible);

        tracker.track_hosts(
            now,
            &[
                report_ineligible_none,
                report_ineligible_empty,
                report_eligible.clone(),
            ],
        );

        let events = reporter.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bmc_ip, report_eligible.0.host_bmc_ip);
        assert_eq!(events[0].reason, BootOrderReportReason::NewHost);

        // Cache should only contain the eligible host.
        assert_eq!(tracker.cached_boot_order.len(), 1);
        assert!(
            tracker
                .cached_boot_order
                .contains_key(&report_eligible.0.host_bmc_ip)
        );
    }

    #[test]
    fn reports_on_new_host() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();
        let report = make_report(
            IpAddr::from_str("192.0.2.1").unwrap(),
            sample_system("sys", true),
        );

        tracker.track_hosts(now, &[report]);

        let events = reporter.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, BootOrderReportReason::NewHost);
    }

    #[test]
    fn reports_on_change_detection() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();
        let ip = IpAddr::from_str("192.0.2.2").unwrap();
        let (host, report) = make_report(ip, sample_system("sys", true));

        tracker.track_hosts(now, &[(host.clone(), report)]);
        reporter.take(); // clear initial new host report

        let changed = make_report(ip, sample_system("sys", false));
        tracker.track_hosts(now + Duration::from_secs(1), &[(host, changed.1)]);

        let events = reporter.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, BootOrderReportReason::ChangeDetected);
    }

    #[test]
    fn reports_periodically_after_interval() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();
        let ip = IpAddr::from_str("192.0.2.3").unwrap();
        let report = make_report(ip, sample_system("sys", true));

        tracker.track_hosts(now, std::slice::from_ref(&report));
        reporter.take(); // initial new host

        tracker.track_hosts(now + Duration::from_secs(60), std::slice::from_ref(&report));
        assert!(
            reporter.take().is_empty(),
            "should not report before interval"
        );

        tracker.track_hosts(
            now + BOOT_ORDER_TRACKER_INTERVAL + Duration::from_secs(1),
            &[report],
        );
        let events = reporter.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, BootOrderReportReason::PeriodicUpdate);
    }

    #[test]
    fn caps_periodic_reports_per_iteration() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();

        let hosts: Vec<_> = (0..12)
            .map(|i| {
                let ip = IpAddr::from_str(&format!("192.0.2.{}", i + 10)).unwrap();
                make_report(ip, sample_system(&format!("sys{i}"), true))
            })
            .collect();

        tracker.track_hosts(now, &hosts);
        reporter.take(); // clear new host reports

        // Make some hosts "older" than others to ensure prioritization respects report_time.
        let mut ips: Vec<IpAddr> = tracker.cached_boot_order.keys().copied().collect();
        ips.sort();
        for (idx, ip) in ips.iter().enumerate() {
            if let Some(status) = tracker.cached_boot_order.get_mut(ip) {
                status.report_time = now
                    .checked_sub(Duration::from_secs((idx as u64) * 10))
                    .unwrap_or(now);
            }
        }

        tracker.track_hosts(
            now + BOOT_ORDER_TRACKER_INTERVAL + Duration::from_secs(1),
            &hosts,
        );

        let events = reporter.take();
        let limit = BOOT_ORDER_MAX_LOGGED_HOST_PER_ITERATION as usize;
        assert_eq!(events.len(), limit);
        assert!(
            events
                .iter()
                .all(|e| e.reason == BootOrderReportReason::PeriodicUpdate)
        );

        // Ensure the reported hosts are the ones with the oldest report_time.
        let expected: std::collections::HashSet<IpAddr> =
            ips.into_iter().rev().take(limit).collect();
        let reported: std::collections::HashSet<IpAddr> = events.iter().map(|e| e.bmc_ip).collect();
        assert_eq!(reported, expected);
    }

    #[test]
    fn removes_hosts_not_reported_anymore() {
        let reporter = RecordingReporter::default();
        let mut tracker = BootOrderTracker::new(reporter.clone());
        let now = Instant::now();
        let ip = IpAddr::from_str("192.0.2.50").unwrap();
        let report = make_report(ip, sample_system("sys", true));

        tracker.track_hosts(now, std::slice::from_ref(&report));
        reporter.take(); // clear initial new host report

        // No reports for this iteration: host should be evicted from cache.
        tracker.track_hosts(now + Duration::from_secs(1), &[]);
        assert!(reporter.take().is_empty());

        // When the host reappears it should be treated as a new host again.
        tracker.track_hosts(now + Duration::from_secs(2), &[report]);
        let events = reporter.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, BootOrderReportReason::NewHost);
    }
}
