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

//! Rack-level `NmxcUnreachable` health alerts.
//!
//! The partition monitor reaches each rack's NMX-C through one switch
//! endpoint. When that fails, the pass clears the rack's machine observations
//! and counts the failure, but nothing in aggregate health says the controller
//! itself is down. This module tracks consecutive failures per rack and tells
//! the monitor when to raise or clear a merge-mode report on the rack.
//!
//! A single failed pass is common on a loaded site (a keepalive timeout on one
//! slow pod, a Hello lost during a pod restart), so the alert is debounced: it
//! is raised only after `threshold` consecutive unusable passes and cleared on
//! the first pass that reaches NMX-C again. Whether the alert is currently
//! stored is read from the database each pass rather than remembered here, so
//! a monitor restart or an operator removing the report cannot leave the two
//! out of step.

use std::collections::{HashMap, HashSet};

use carbide_uuid::rack::RackId;
use chrono::{DateTime, Utc};

use crate::metrics::ChassisNmxCUnreachableReason;

/// What the monitor should do to a rack's `NmxcUnreachable` report after a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RackAlertAction {
    /// Leave the stored state as it is.
    Keep,
    /// Write the alert: the rack has been unusable for `threshold` passes
    /// without a stored report. Carries the reason of the pass that crossed
    /// the threshold and when the current run of failures began.
    Set {
        reason: ChassisNmxCUnreachableReason,
        since: DateTime<Utc>,
    },
    /// Remove the alert: NMX-C was reached this pass.
    Clear,
}

/// A run of consecutive passes in which a rack's NMX-C was unusable and no
/// report was stored for it.
#[derive(Debug, Clone, Copy)]
struct Streak {
    passes: u32,
    since: DateTime<Utc>,
}

/// Consecutive unusable passes per rack, held between iterations.
#[derive(Debug, Default)]
pub(super) struct RackUnreachableTracker {
    streaks: HashMap<RackId, Streak>,
    /// Racks whose `Clear` write failed. A reachable rack is skipped on later
    /// passes and yields no outcome, so the clear is carried here until it
    /// succeeds or the rack is checked again.
    pending_clears: HashSet<RackId>,
}

impl RackUnreachableTracker {
    /// Records this pass's outcome for `rack_id` and decides the report action.
    ///
    /// `unreachable` is the reason NMX-C could not be used, or `None` when the
    /// pass reached it. `alert_stored` is whether the rack currently carries
    /// this monitor's merge report. Only passes without a stored report count
    /// towards `threshold`, so a report that is cleared from elsewhere during
    /// an ongoing outage, by an operator or by another replica that reached
    /// NMX-C, is written again only after `threshold` further consecutive
    /// failures. The streak is kept until [`Self::confirm`] reports the write
    /// succeeded, so a failed `Set` is retried on the next failing pass.
    pub(super) fn observe(
        &mut self,
        rack_id: &RackId,
        unreachable: Option<ChassisNmxCUnreachableReason>,
        threshold: u32,
        alert_stored: bool,
    ) -> RackAlertAction {
        match unreachable {
            None => {
                self.streaks.remove(rack_id);
                if alert_stored {
                    RackAlertAction::Clear
                } else {
                    self.pending_clears.remove(rack_id);
                    RackAlertAction::Keep
                }
            }
            Some(_) if alert_stored => {
                // The stored report already says what this pass observed.
                self.streaks.remove(rack_id);
                self.pending_clears.remove(rack_id);
                RackAlertAction::Keep
            }
            Some(reason) => {
                self.pending_clears.remove(rack_id);
                let streak = self
                    .streaks
                    .entry(rack_id.clone())
                    .or_insert_with(|| Streak {
                        passes: 0,
                        since: Utc::now(),
                    });
                streak.passes = streak.passes.saturating_add(1);
                if streak.passes >= threshold {
                    RackAlertAction::Set {
                        reason,
                        since: streak.since,
                    }
                } else {
                    RackAlertAction::Keep
                }
            }
        }
    }

    /// Racks that still carry the report because an earlier `Clear` failed, so
    /// the clear can be retried for those this pass did not check.
    pub(super) fn pending_clears<'a>(
        &'a self,
        stored: &'a HashSet<RackId>,
    ) -> impl Iterator<Item = RackId> + 'a {
        self.pending_clears
            .iter()
            .filter(|rack_id| stored.contains(rack_id))
            .cloned()
    }

    /// Records whether the write for `action` succeeded. A confirmed `Set`
    /// ends the streak so the next report needs `threshold` passes again; a
    /// failed `Clear` is remembered and retried while the rack stays stored.
    pub(super) fn confirm(&mut self, rack_id: &RackId, action: RackAlertAction, written: bool) {
        match action {
            RackAlertAction::Keep => {}
            RackAlertAction::Set { .. } => {
                if written {
                    self.streaks.remove(rack_id);
                }
            }
            RackAlertAction::Clear => {
                if written {
                    self.pending_clears.remove(rack_id);
                } else {
                    self.pending_clears.insert(rack_id.clone());
                }
            }
        }
    }

    /// Drops state of racks no longer processed by the monitor.
    pub(super) fn retain(&mut self, keep: impl Fn(&RackId) -> bool) {
        self.streaks.retain(|rack_id, _| keep(rack_id));
        self.pending_clears.retain(|rack_id| keep(rack_id));
    }
}

/// Stable label for the failing step, used in the alert message.
///
/// `PartitionMonitorWorkFailed` is reported after NMX-C was reached, so the
/// monitor never passes it here; the arm exists only so the match is
/// exhaustive against the metric's reason set.
pub(super) fn unreachable_reason_label(reason: ChassisNmxCUnreachableReason) -> &'static str {
    match reason {
        ChassisNmxCUnreachableReason::NoEndpoint => "no NMX-C endpoint",
        ChassisNmxCUnreachableReason::InvalidEndpointUri => "invalid NMX-C endpoint URI",
        ChassisNmxCUnreachableReason::ClientCreateFailed => "NMX-C client could not be created",
        ChassisNmxCUnreachableReason::HelloFailed => "NMX-C hello failed",
        ChassisNmxCUnreachableReason::DomainUuidParseFailed => {
            "NMX-C hello returned no usable domain UUID"
        }
        ChassisNmxCUnreachableReason::PartitionMonitorWorkFailed => "NMX-C partition work failed",
    }
}

/// The merge report the monitor stores on a rack whose NMX-C stays unreachable.
/// `since` is when the run of failed passes began, so the alert's
/// `in_alert_since` reflects the outage rather than the debounce.
pub(super) fn nmxc_unreachable_report(
    source: &str,
    endpoint: Option<&str>,
    reason: ChassisNmxCUnreachableReason,
    since: DateTime<Utc>,
) -> health_report::HealthReport {
    health_report::HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert::nmxc_unreachable(
            endpoint.map(str::to_owned),
            unreachable_reason_label(reason),
            since,
        )],
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    const RACK: &str = "rack-1";

    /// One pass as the tracker sees it: the failure reason if any, and whether
    /// the rack carries the report before the pass.
    type Pass = (Option<ChassisNmxCUnreachableReason>, bool);

    /// Actions without the `since` timestamp, which varies per run.
    #[derive(Debug, PartialEq, Eq)]
    enum Action {
        Keep,
        Set(ChassisNmxCUnreachableReason),
        Clear,
    }

    /// Runs `passes` for one rack against `threshold` and returns each action.
    /// Runs `passes` for one rack against `threshold`, confirming every write
    /// as successful, and returns each action.
    fn actions(threshold: u32, passes: &[Pass]) -> Vec<Action> {
        let mut tracker = RackUnreachableTracker::default();
        let rack_id = RackId::new(RACK);
        passes
            .iter()
            .map(|(unreachable, stored)| {
                let action = tracker.observe(&rack_id, *unreachable, threshold, *stored);
                tracker.confirm(&rack_id, action, true);
                match action {
                    RackAlertAction::Keep => Action::Keep,
                    RackAlertAction::Set { reason, .. } => Action::Set(reason),
                    RackAlertAction::Clear => Action::Clear,
                }
            })
            .collect()
    }

    #[test]
    fn failed_writes_are_retried() {
        use ChassisNmxCUnreachableReason::HelloFailed;

        let rack_id = RackId::new(RACK);
        let stored = HashSet::from([rack_id.clone()]);

        // A failed Set keeps the streak, so the next failing pass sets again
        // without waiting for the threshold anew.
        let mut tracker = RackUnreachableTracker::default();
        let first = tracker.observe(&rack_id, Some(HelloFailed), 1, false);
        assert!(matches!(first, RackAlertAction::Set { .. }));
        tracker.confirm(&rack_id, first, false);
        let retry = tracker.observe(&rack_id, Some(HelloFailed), 1, false);
        assert!(matches!(retry, RackAlertAction::Set { .. }));
        tracker.confirm(&rack_id, retry, true);
        assert_eq!(
            tracker.observe(&rack_id, Some(HelloFailed), 2, false),
            RackAlertAction::Keep,
            "a confirmed Set starts the count over"
        );

        // A failed Clear stays pending while the rack is stored, and is
        // forgotten once a write succeeds.
        let mut tracker = RackUnreachableTracker::default();
        let clear = tracker.observe(&rack_id, None, 3, true);
        assert_eq!(clear, RackAlertAction::Clear);
        tracker.confirm(&rack_id, clear, false);
        assert_eq!(
            tracker.pending_clears(&stored).collect::<Vec<_>>(),
            vec![rack_id.clone()]
        );
        assert_eq!(
            tracker.pending_clears(&HashSet::new()).count(),
            0,
            "a rack no longer stored has nothing to clear"
        );
        tracker.confirm(&rack_id, RackAlertAction::Clear, true);
        assert_eq!(tracker.pending_clears(&stored).count(), 0);
    }

    #[test]
    fn alert_is_raised_only_after_threshold_consecutive_failures() {
        use Action::{Keep, Set};
        use ChassisNmxCUnreachableReason::HelloFailed;

        let fail = (Some(HelloFailed), false);
        check_values(
            [
                Check {
                    scenario: "one failure below threshold is kept quiet",
                    input: (3, vec![fail]),
                    expect: vec![Keep],
                },
                Check {
                    scenario: "the pass that reaches the threshold sets the alert",
                    input: (3, vec![fail, fail, fail]),
                    expect: vec![Keep, Keep, Set(HelloFailed)],
                },
                Check {
                    scenario: "threshold of one sets on the first failure",
                    input: (1, vec![fail]),
                    expect: vec![Set(HelloFailed)],
                },
            ],
            |(threshold, passes)| actions(threshold, &passes),
        );
    }

    #[test]
    fn success_resets_the_streak_and_clears_a_stored_alert() {
        use Action::{Clear, Keep, Set};
        use ChassisNmxCUnreachableReason::{HelloFailed, NoEndpoint};

        check_values(
            [
                Check {
                    scenario: "a reachable pass between failures restarts the count",
                    input: vec![
                        (Some(HelloFailed), false),
                        (None, false),
                        (Some(HelloFailed), false),
                        (Some(HelloFailed), false),
                    ],
                    expect: vec![Keep, Keep, Keep, Set(HelloFailed)],
                },
                Check {
                    scenario: "a reachable pass clears a stored alert exactly once",
                    input: vec![(None, true), (None, false)],
                    expect: vec![Clear, Keep],
                },
                Check {
                    scenario: "the reason that crosses the threshold is reported",
                    input: vec![(Some(HelloFailed), false), (Some(NoEndpoint), false)],
                    expect: vec![Keep, Set(NoEndpoint)],
                },
            ],
            |passes| actions(2, &passes),
        );
    }

    #[test]
    fn stored_alert_is_kept_and_a_cleared_one_is_restored_only_after_threshold() {
        use Action::{Keep, Set};
        use ChassisNmxCUnreachableReason::HelloFailed;

        let fail_stored = (Some(HelloFailed), true);
        let fail_unstored = (Some(HelloFailed), false);
        check_values(
            [
                Check {
                    scenario: "failures while the report is stored are not rewritten",
                    input: vec![fail_stored, fail_stored, fail_stored],
                    expect: vec![Keep, Keep, Keep],
                },
                Check {
                    scenario: "a report cleared elsewhere mid-outage needs threshold passes again",
                    input: vec![
                        fail_unstored,
                        fail_unstored,
                        fail_stored,
                        fail_unstored,
                        fail_unstored,
                    ],
                    expect: vec![Keep, Set(HelloFailed), Keep, Keep, Set(HelloFailed)],
                },
            ],
            |passes| actions(2, &passes),
        );
    }

    #[test]
    fn set_reports_when_the_run_of_failures_began() {
        let mut tracker = RackUnreachableTracker::default();
        let rack_id = RackId::new(RACK);
        let before = Utc::now();
        assert_eq!(
            tracker.observe(
                &rack_id,
                Some(ChassisNmxCUnreachableReason::HelloFailed),
                2,
                false
            ),
            RackAlertAction::Keep
        );
        let RackAlertAction::Set { since, .. } = tracker.observe(
            &rack_id,
            Some(ChassisNmxCUnreachableReason::HelloFailed),
            2,
            false,
        ) else {
            panic!("second failure should set the alert");
        };
        assert!(since >= before && since <= Utc::now());
    }

    #[test]
    fn report_names_the_endpoint_and_reason_without_blocking_allocation() {
        let since = Utc::now();
        let report = nmxc_unreachable_report(
            "nvlink-partition-monitor",
            Some("https://10.0.0.5:9370"),
            ChassisNmxCUnreachableReason::HelloFailed,
            since,
        );
        assert_eq!(report.source, "nvlink-partition-monitor");
        let [alert] = report.alerts.as_slice() else {
            panic!("expected one alert, got {:?}", report.alerts);
        };
        assert_eq!(alert.id, health_report::HealthProbeId::nmxc_unreachable());
        assert_eq!(alert.target.as_deref(), Some("https://10.0.0.5:9370"));
        assert_eq!(alert.in_alert_since, Some(since));
        assert!(alert.message.contains("NMX-C hello failed"));
        assert_eq!(
            alert.classifications,
            vec![health_report::HealthAlertClassification::hardware()]
        );

        let no_endpoint = nmxc_unreachable_report(
            "nvlink-partition-monitor",
            None,
            ChassisNmxCUnreachableReason::NoEndpoint,
            since,
        );
        assert_eq!(no_endpoint.alerts[0].target, None);
        assert!(no_endpoint.alerts[0].message.contains("No NMX-C endpoint"));
    }
}
