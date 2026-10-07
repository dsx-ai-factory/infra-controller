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

use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeSuccess, HealthReport,
};
use nvue_client::types::bgp::{BgpNeighbors, BgpPeerState};
use nvue_client::{FieldFilter, NvueClient};

use super::{add_post_config_wait_alert, failed, make_alert, make_classified_alert, probe_ids};
use crate::HBNDeviceNames;

/// The VRF we'll look for our BGP uplinks in.
const BGP_VRF_UPLINKS: &str = "default";

/// Health check configuration for NVUE API targets.
pub(crate) struct NvueHealthCheck<'a> {
    /// NVUE client used for API availability checks and BGP queries.
    pub(crate) nvue_client: &'a NvueClient,
    /// Minimum number of configured ToR uplink sessions that must be established.
    pub(crate) min_healthy_links: usize,
    /// HBN interface names used to identify expected ToR uplink sessions.
    pub(crate) hbn_device_names: &'a HBNDeviceNames,
    /// Whether this iteration applied a changed HBN configuration through NVUE.
    pub(crate) has_changed_hbn_config: bool,
}

impl NvueHealthCheck<'_> {
    /// Performs all health checks against the NVUE API.
    pub(crate) async fn health_check(&self) -> HealthReport {
        let mut report = HealthReport::empty("forge-dpu-agent".into());

        match self.nvue_api_health().await {
            Ok(success) => report.successes.push(success),
            Err(alert) => {
                // If the NVUE API wasn't healthy, we can't use it to check
                // anything else.
                report.alerts.push(alert);
                return report;
            }
        }

        self.check_bgp_uplinks(&mut report).await;
        add_post_config_wait_alert(&mut report, self.has_changed_hbn_config);
        report
    }

    /// Checks BGP uplink session health through the configured NVUE API target.
    async fn check_bgp_uplinks(&self, report: &mut HealthReport) {
        const BGP_NEIGHBOR_STATE_FIELD: &str = "/*/state";

        if self.min_healthy_links == 0 {
            return;
        }

        let bgp_neighbors = self
            .nvue_client
            .get_bgp_neighbors_filtered(
                BGP_VRF_UPLINKS,
                FieldFilter::with_includes([BGP_NEIGHBOR_STATE_FIELD]),
            )
            .await;
        match bgp_neighbors.as_ref() {
            Ok(bgp_neighbors) => check_bgp_uplink_sessions(
                report,
                bgp_neighbors.as_ref(),
                self.min_healthy_links,
                self.hbn_device_names,
            ),
            Err(error) => failed(
                report,
                probe_ids::BgpPeeringTor.clone(),
                None,
                format!("Error fetching NVUE BGP neighbor data for VRF {BGP_VRF_UPLINKS}: {error}"),
            ),
        }
    }

    /// Checks whether the NVUE API can answer a basic system-information request.
    async fn nvue_api_health(&self) -> Result<HealthProbeSuccess, HealthProbeAlert> {
        match self.nvue_client.system_info().await {
            Ok(_) => Ok(HealthProbeSuccess {
                id: probe_ids::NvueApiRunning.clone(),
                target: None,
            }),
            Err(e) => Err(make_alert(
                probe_ids::NvueApiRunning.clone(),
                None,
                format!("Error communicating with NVUE API: {e}"),
                true,
            )),
        }
    }
}

/// One configured NVUE uplink whose BGP session is not established.
struct UnhealthyUplink {
    /// Whether this is the first configured uplink, which PXE boot requires.
    is_primary: bool,
    /// HBN interface name used as the health alert target.
    uplink_name: String,
    /// Diagnostic reason that the session check failed.
    message: String,
}

/// Checks configured ToR BGP sessions using an NVUE response fetched by the caller.
///
/// All configured HBN uplinks are evaluated, and the check passes when at least
/// `min_healthy_links` of them are present and established. If too few uplinks
/// are healthy, each missing peer, missing state, or non-established state emits
/// a `BgpPeeringTor` alert targeted at that uplink. While a redundant session
/// remains established, an unavailable primary (the first configured uplink)
/// prevents allocations but not host state changes; an unavailable secondary
/// has no classifications. With a minimum of one, either established session
/// satisfies the configured minimum, so only a failed primary remains visible
/// for the PXE readiness gate. A minimum of zero disables the NVUE neighbor
/// request and all uplink alerts, including the primary readiness signal. If no
/// expected uplink is established, each alert prevents allocations and host
/// state changes. A `BgpPeeringTor` alert is emitted when `min_healthy_links`
/// asks for more uplinks than the configured device names provide. The helper
/// does not emit success entries.
fn check_bgp_uplink_sessions(
    report: &mut HealthReport,
    neighbors: Option<&BgpNeighbors>,
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    let mut unhealthy_uplinks = Vec::new();
    let mut healthy_uplink_count = 0;
    let expected_hbn_uplinks = hbn_device_names.uplinks.iter().copied().enumerate();

    for (uplink_index, expected_uplink) in expected_hbn_uplinks {
        match check_expected_peer_established(neighbors, expected_uplink) {
            Ok(()) => healthy_uplink_count += 1,
            Err(message) => unhealthy_uplinks.push(UnhealthyUplink {
                is_primary: uplink_index == 0,
                uplink_name: expected_uplink.to_string(),
                message,
            }),
        }
    }

    if min_healthy_links == 0 {
        return;
    }

    if healthy_uplink_count >= min_healthy_links {
        // A healthy p1 can satisfy the configured minimum, but PXE still
        // depends on p0, so preserve a failed primary as an allocation blocker.
        let failed_primary_uplink = unhealthy_uplinks
            .into_iter()
            .find(|unhealthy_uplink| unhealthy_uplink.is_primary);
        if let Some(failed_primary_uplink) = failed_primary_uplink {
            report.alerts.push(make_classified_alert(
                probe_ids::BgpPeeringTor.clone(),
                Some(failed_primary_uplink.uplink_name),
                failed_primary_uplink.message,
                bgp_uplink_classifications(true, false),
            ));
        }
        return;
    }

    if min_healthy_links > hbn_device_names.uplinks.len() {
        failed(
            report,
            probe_ids::BgpPeeringTor.clone(),
            None,
            format!(
                "Site configuration requires a minimum of {min_healthy_links} \
                healthy uplinks, but this is greater than the number of \
                expected uplink interface names ({hbn_uplink_names})",
                hbn_uplink_names = hbn_device_names.uplinks.join(",")
            ),
        );
    }

    let no_established_uplinks = healthy_uplink_count == 0;
    for unhealthy_uplink in unhealthy_uplinks {
        report.alerts.push(make_classified_alert(
            probe_ids::BgpPeeringTor.clone(),
            Some(unhealthy_uplink.uplink_name),
            unhealthy_uplink.message,
            bgp_uplink_classifications(unhealthy_uplink.is_primary, no_established_uplinks),
        ));
    }
}

/// Selects the classifications for one unavailable ToR uplink.
fn bgp_uplink_classifications(
    is_primary: bool,
    no_established_uplinks: bool,
) -> Vec<HealthAlertClassification> {
    if no_established_uplinks {
        vec![
            HealthAlertClassification::prevent_allocations(),
            HealthAlertClassification::prevent_host_state_changes(),
        ]
    } else if is_primary {
        vec![HealthAlertClassification::prevent_allocations()]
    } else {
        vec![]
    }
}

/// Checks whether an expected peer has an established NVUE BGP session.
///
/// Returns `Ok(())` when the peer exists in the NVUE neighbor map and reports
/// `BgpPeerState::Established`. A missing NVUE neighbor map, missing peer,
/// missing state, or non-established state returns a descriptive error message
/// for conversion to a health alert by the caller.
fn check_expected_peer_established(
    neighbors: Option<&BgpNeighbors>,
    peer_name: &str,
) -> Result<(), String> {
    let Some(neighbors) = neighbors else {
        return Err(format!(
            "BGP neighbor data was not reported; expected session for {peer_name}"
        ));
    };
    let Some(peer) = neighbors.get(peer_name) else {
        return Err(format!(
            "expected session for {peer_name} was not found in BGP peer data"
        ));
    };
    let Some(peer_state) = peer.state.as_ref() else {
        return Err(format!("state field for {peer_name} peer is not present"));
    };
    let expected_state = BgpPeerState::Established;
    match peer_state {
        BgpPeerState::Established => Ok(()),
        state => Err(format!(
            "BGP session {peer_name} is not {expected_state}, but in state {state}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    /// One `check_bgp_uplink_sessions` scenario and its exact expected alerts.
    struct Row {
        scenario: &'static str,
        bgp_json: &'static str,
        min_healthy_links: usize,
        expected_alerts: Vec<health_report::HealthProbeAlert>,
    }

    /// Test-only names for the exact classification sets expected by table rows.
    ///
    /// This builds the vectors without production classification helpers, so
    /// the tests can catch policy regressions.
    #[derive(Clone, Copy)]
    enum ExpectedClassifications {
        /// Contains no classifications; the alert remains visible.
        Unclassified,
        /// Contains only `PreventAllocations`.
        PreventAllocations,
        /// Contains `PreventAllocations` and `PreventHostStateChanges`.
        PreventAllocationsAndHostStateChanges,
    }

    impl ExpectedClassifications {
        fn health_classifications(self) -> Vec<HealthAlertClassification> {
            match self {
                Self::Unclassified => vec![],
                Self::PreventAllocations => {
                    vec![HealthAlertClassification::prevent_allocations()]
                }
                Self::PreventAllocationsAndHostStateChanges => vec![
                    HealthAlertClassification::prevent_allocations(),
                    HealthAlertClassification::prevent_host_state_changes(),
                ],
            }
        }
    }

    /// Builds NVUE BGP neighbors from a compact scenario JSON fixture.
    fn bgp_neighbors(bgp_json: &str) -> Option<BgpNeighbors> {
        serde_json::from_str(bgp_json).expect("BGP neighbors should deserialize")
    }

    /// Builds an expected ToR peering alert independently of the production helper.
    fn tor_alert(
        port: &str,
        message: &str,
        expected_classifications: ExpectedClassifications,
    ) -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::bgp_peering_tor(),
            target: Some(port.to_string()),
            in_alert_since: None,
            message: message.to_string(),
            tenant_message: None,
            classifications: expected_classifications.health_classifications(),
        }
    }

    /// Builds the site-configuration alert for impossible uplink thresholds.
    fn config_mismatch_alert(min_healthy_links: usize) -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::bgp_peering_tor(),
            target: None,
            in_alert_since: None,
            message: format!(
                "Site configuration requires a minimum of {min_healthy_links} healthy uplinks, but this is greater than the number of expected uplink interface names (p0_if,p1_if)"
            ),
            tenant_message: None,
            classifications: ExpectedClassifications::PreventAllocationsAndHostStateChanges
                .health_classifications(),
        }
    }

    /// Orders alerts by `(id, target, message)` so table rows can stay readable.
    fn sort_alerts(alerts: &mut [health_report::HealthProbeAlert]) {
        alerts.sort_by(|a, b| (&a.id, &a.target, &a.message).cmp(&(&b.id, &b.target, &b.message)));
    }

    #[test]
    fn check_bgp_tor_sessions_emits_expected_alerts() {
        check_values(
            [
                Row {
                    scenario: "all configured uplinks established",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "missing neighbor data blocks allocations and host state changes",
                    bgp_json: r#"null"#,
                    min_healthy_links: 2,
                    expected_alerts: vec![
                        tor_alert(
                            "p0_if",
                            "BGP neighbor data was not reported; expected session for p0_if",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "BGP neighbor data was not reported; expected session for p1_if",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Row {
                    scenario: "missing secondary stays visible without classifications",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![tor_alert(
                        "p1_if",
                        "expected session for p1_if was not found in BGP peer data",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Row {
                    scenario: "failed primary prevents allocations but not state changes",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "idle" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![tor_alert(
                        "p0_if",
                        "BGP session p0_if is not established, but in state idle",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Row {
                    scenario: "failed secondary stays visible without classifications",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "idle" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![tor_alert(
                        "p1_if",
                        "BGP session p1_if is not established, but in state idle",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Row {
                    scenario: "two failed sessions block allocations and state changes",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "idle" },
                        "p1_if": { "state": "idle" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![
                        tor_alert(
                            "p0_if",
                            "BGP session p0_if is not established, but in state idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "BGP session p1_if is not established, but in state idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Row {
                    scenario: "active primary session prevents allocations",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "active" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![tor_alert(
                        "p0_if",
                        "BGP session p0_if is not established, but in state active",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Row {
                    scenario: "primary with missing state prevents allocations",
                    bgp_json: r#"
                    {
                        "p0_if": {},
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![tor_alert(
                        "p0_if",
                        "state field for p0_if peer is not present",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Row {
                    scenario: "extra non-ToR and route-server neighbors ignored",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "established" },
                        "tenant-vrf-peer": { "state": "idle" },
                        "10.217.126.67": { "state": "active" }
                    }
                    "#,
                    min_healthy_links: 2,
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "minimum one keeps failed primary as allocation blocker",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "idle" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 1,
                    expected_alerts: vec![tor_alert(
                        "p0_if",
                        "BGP session p0_if is not established, but in state idle",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Row {
                    scenario: "minimum one accepts two established sessions",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 1,
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "minimum one suppresses failed secondary after p0 is established",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "idle" }
                    }
                    "#,
                    min_healthy_links: 1,
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "minimum one still blocks when both sessions fail",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "idle" },
                        "p1_if": { "state": "idle" }
                    }
                    "#,
                    min_healthy_links: 1,
                    expected_alerts: vec![
                        tor_alert(
                            "p0_if",
                            "BGP session p0_if is not established, but in state idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "BGP session p1_if is not established, but in state idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Row {
                    scenario: "minimum zero disables every uplink alert",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "idle" },
                        "p1_if": { "state": "idle" }
                    }
                    "#,
                    min_healthy_links: 0,
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "min healthy links greater than configured uplinks alerts",
                    bgp_json: r#"
                    {
                        "p0_if": { "state": "established" },
                        "p1_if": { "state": "established" }
                    }
                    "#,
                    min_healthy_links: 3,
                    expected_alerts: vec![config_mismatch_alert(3)],
                },
            ]
            .map(|mut row| {
                // Alert emission order is not part of the policy under test, so
                // normalize each expected row before moving it into `Check`.
                sort_alerts(&mut row.expected_alerts);
                let expect = row.expected_alerts.clone();
                Check {
                    scenario: row.scenario,
                    input: row,
                    expect,
                }
            }),
            |row| {
                // Parse the NVUE response, apply the uplink policy, and compare
                // the complete alert set independent of emission order.
                let mut report = HealthReport::empty("forge-dpu-agent".to_string());
                let bgp_neighbors = bgp_neighbors(row.bgp_json);
                check_bgp_uplink_sessions(
                    &mut report,
                    bgp_neighbors.as_ref(),
                    row.min_healthy_links,
                    &HBNDeviceNames::hbn_23(),
                );
                sort_alerts(&mut report.alerts);
                report.alerts
            },
        );
    }
}
