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

use health_report::HealthAlertClassification;
use serde::Deserialize;

use super::{failed, make_alert, make_classified_alert, passed, probe_ids};
use crate::{HBNDeviceNames, hbn};

/// `BgpHealthCheckParams` contains the configured peers, uplinks, and address
/// families that the BGP health check should require.
pub(super) struct BgpHealthCheckParams<'a> {
    pub(super) host_routes: &'a [&'a str],
    pub(super) min_healthy_links: usize,
    pub(super) route_servers: &'a [String],
    pub(super) should_check_ipv6_unicast: bool,
    pub(super) hbn_device_names: &'a HBNDeviceNames,
}

/// Check HBN BGP stats
pub(super) async fn check_bgp_stats(
    hr: &mut health_report::HealthReport,
    container_id: &str,
    params: BgpHealthCheckParams<'_>,
) {
    // If BGP daemon is not enabled, we will get a bunch of bogus alerts shown
    // that are not helpful to anyone. Since showing `BgpDaemonEnabled` already
    // covers the core problem - don't bother with the remaining checks.
    if hr
        .alerts
        .iter()
        .any(|alert| alert.id == *probe_ids::BgpDaemonEnabled)
    {
        return;
    }

    let mut health_data = BgpHealthData::default();

    // `vtysh` is the Free Range Routing (FRR) shell.
    let bgp_summary_parsed = match hbn::run_in_container(
        container_id,
        &["vtysh", "-c", "show bgp summary json"],
        true,
    )
    .await
    {
        Ok(bgp_json) => verify_bgp_summary(&mut health_data, &bgp_json, &params),
        Err(err) => {
            tracing::warn!(error = %err, "Failed to fetch BGP summary");
            health_data.other_errors.push(err.to_string());
            false
        }
    };

    if params.should_check_ipv6_unicast && bgp_summary_parsed && params.min_healthy_links > 0 {
        match hbn::run_in_container(
            container_id,
            &["vtysh", "-c", "show bgp ipv6 unicast summary failed json"],
            true,
        )
        .await
        {
            Ok(failed_peers_json) => verify_failed_ipv6_unicast_peers(
                &mut health_data,
                &failed_peers_json,
                params.min_healthy_links,
                params.hbn_device_names,
            ),
            Err(err) => {
                tracing::warn!(error = %err, "Failed to fetch failed IPv6-unicast BGP peers");
                health_data.other_errors.push(format!(
                    "failed to fetch failed IPv6-unicast BGP peers: {err}"
                ));
            }
        }
    }

    health_data.into_health_report(hr, params.min_healthy_links, params.hbn_device_names);
}

pub(super) fn check_daemon_enabled(hr: &mut health_report::HealthReport, hbn_daemons_file: &str) {
    let daemons = match std::fs::read_to_string(hbn_daemons_file) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to read BGP daemon configuration");
            failed(
                hr,
                probe_ids::BgpDaemonEnabled.clone(),
                None,
                format!("Trying to open and read {hbn_daemons_file}: {err}"),
            );
            return;
        }
    };

    if daemons.contains("bgpd=no") {
        failed(
            hr,
            probe_ids::BgpDaemonEnabled.clone(),
            None,
            format!("BGP daemon is disabled - {hbn_daemons_file} contains 'bgpd=no'"),
        );
        return;
    }
    if !daemons.contains("bgpd=yes") {
        failed(
            hr,
            probe_ids::BgpDaemonEnabled.clone(),
            None,
            format!("BGP daemon is not enabled - {hbn_daemons_file} does not contain 'bgpd=yes'"),
        );
        return;
    }

    passed(hr, probe_ids::BgpDaemonEnabled.clone(), None);
}

/// `verify_bgp_summary` records health failures from a parsed FRR summary.
///
/// The return value only says whether deserialization succeeded. Individual
/// BGP failures stay in `health_data` and still return `true`.
fn verify_bgp_summary(
    health_data: &mut BgpHealthData,
    bgp_json: &str,
    params: &BgpHealthCheckParams<'_>,
) -> bool {
    let networks: BgpNetworks = match serde_json::from_str(bgp_json) {
        Ok(networks) => networks,
        Err(error) => {
            health_data
                .other_errors
                .push(format!("failed to deserialize BGP summary: {error}"));
            return false;
        }
    };

    check_bgp_stats_ipv4_unicast(
        "ipv4_unicast",
        &networks.ipv4_unicast,
        health_data,
        params.host_routes,
        params.min_healthy_links,
        params.hbn_device_names,
    );
    if params.should_check_ipv6_unicast {
        match &networks.ipv6_unicast {
            Some(ipv6_unicast) => check_bgp_stats_ipv6_unicast(
                "ipv6_unicast",
                ipv6_unicast,
                health_data,
                params.min_healthy_links,
                params.hbn_device_names,
            ),
            None => health_data
                .other_errors
                .push("ipv6_unicast BGP summary was not reported".to_string()),
        }
    }
    check_bgp_stats_l2_vpn_evpn(
        "l2_vpn_evpn",
        &networks.l2_vpn_evpn,
        health_data,
        params.route_servers,
        params.min_healthy_links,
        params.hbn_device_names,
    );
    true
}

fn check_bgp_tor_routes(
    s: &BgpStats,
    health_data: &mut BgpHealthData,
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    if min_healthy_links > hbn_device_names.uplinks.len() {
        let error = format!(
            "The number of min healthy links: {min_healthy_links} \
            was bigger than the number of uplinks defined by the hbn device names: {}",
            hbn_device_names.uplinks.len()
        );
        if !health_data.other_errors.contains(&error) {
            health_data.other_errors.push(error);
        }
    }

    if min_healthy_links == 0 {
        return;
    }

    // A positive minimum is a health threshold, not a positional limit on
    // which physical sessions should be inspected.
    for &port_name in &hbn_device_names.uplinks {
        let mut message = None;

        let session_data = s.peers.get(port_name);
        match session_data {
            Some(session) => {
                if session.state != "Established" {
                    message = Some(format!(
                        "Session {port_name} is not Established, but in state {}",
                        session.state
                    ));
                }
            }
            None => {
                message = Some(format!(
                    "Expected session for {port_name} was not found in BGP peer data"
                ));
            }
        }

        if let Some(message) = message {
            health_data
                .tor_session_failures
                .insert(port_name.to_string());
            health_data
                .unhealthy_tor_peers
                .insert(port_name.to_string(), message);
        }
    }
}

/// Applies the ToR-session and dynamic-peer invariants shared by both underlay
/// unicast address families.
fn check_bgp_stats_unicast(
    name: &str,
    s: &BgpStats,
    health_data: &mut BgpHealthData,
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    check_bgp_tor_routes(s, health_data, min_healthy_links, hbn_device_names);

    if s.dynamic_peers != 0 {
        health_data.other_errors.push(format!(
            "{name}.dynamic_peers is {} should be 0",
            s.dynamic_peers
        ));
    }
}

/// Checks the default-VRF IPv6 underlay, where every non-ToR peer is
/// unexpected.
///
/// Tenant peers live in per-VPC VRFs, and route-server IPv6 is disabled. That
/// leaves the configured HBN uplinks as the only expected peers here.
fn check_bgp_stats_ipv6_unicast(
    name: &str,
    s: &BgpStats,
    health_data: &mut BgpHealthData,
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    check_bgp_stats_unicast(name, s, health_data, min_healthy_links, hbn_device_names);

    for peer_name in s.peers.keys() {
        if !hbn_device_names.uplinks.contains(&peer_name.as_str()) {
            health_data
                .unexpected_peers
                .push((name.to_string(), peer_name.clone()));
        }
    }
}

/// Maps FRR's failed peer view for one address family back to the required uplinks.
///
/// The regular summary's peer state describes the shared BGP transport. The
/// filtered view also includes established peers whose remote did not
/// negotiate IPv6 unicast. The configured minimum limits this check to the
/// required uplinks, so a minimum of one requires only the primary uplink to
/// negotiate IPv6 unicast. Transport health is evaluated separately for both
/// sessions, so this limit does not hide a total uplink outage.
fn verify_failed_ipv6_unicast_peers(
    health_data: &mut BgpHealthData,
    failed_peers_json: &str,
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    let failed_summary: BgpFailedSummary = match serde_json::from_str(failed_peers_json) {
        Ok(summary) => summary,
        Err(error) => {
            health_data.other_errors.push(format!(
                "failed to deserialize failed IPv6-unicast BGP summary: {error}"
            ));
            return;
        }
    };

    for &port_name in hbn_device_names.uplinks.iter().take(min_healthy_links) {
        if failed_summary.peers.contains_key(port_name) {
            health_data
                .unhealthy_tor_peers
                .entry(port_name.to_string())
                .or_insert_with(|| format!("Session {port_name} did not negotiate IPv6-unicast"));
        }
    }
}

fn check_bgp_stats_ipv4_unicast(
    name: &str,
    s: &BgpStats,
    health_data: &mut BgpHealthData,
    host_routes: &[&str],
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    check_bgp_stats_unicast(name, s, health_data, min_healthy_links, hbn_device_names);

    // Tenant sessions are optional because tenants choose whether to use them.
    // No other non-ToR sessions belong in the IPv4-unicast summary.
    for (peer_name, _peer) in s.other_peers() {
        if !host_routes.contains(&peer_name.as_str()) {
            health_data
                .unexpected_peers
                .push((name.to_string(), peer_name.clone()));
        }
    }
}

fn check_bgp_stats_l2_vpn_evpn(
    name: &str,
    s: &BgpStats,
    health_data: &mut BgpHealthData,
    route_servers: &[String],
    min_healthy_links: usize,
    hbn_device_names: &HBNDeviceNames,
) {
    // In case Route servers are not specified, the peer list should contain only
    // TORs. Otherwise we expect it to contain the route servers.
    if route_servers.is_empty() {
        check_bgp_tor_routes(s, health_data, min_healthy_links, hbn_device_names);

        for (peer_name, _peer) in s.other_peers() {
            health_data
                .unexpected_peers
                .push((name.to_string(), peer_name.clone()));
        }
    } else {
        let mut other_peers: HashMap<&String, &BgpPeer> = s.other_peers().collect();
        for route_server in route_servers {
            let session_data = other_peers.remove(route_server);
            let mut message = None;
            match session_data {
                Some(session) => {
                    if session.state != "Established" {
                        message = Some(format!(
                            "Session {route_server} is not Established, but in state {}",
                            session.state
                        ));
                    }
                }
                None => {
                    message = Some(format!(
                        "Expected session for {route_server} was not found in BGP peer data"
                    ));
                }
            }

            if let Some(message) = message {
                health_data
                    .unhealthy_route_server_peers
                    .push((route_server.to_string(), message));
            }
        }

        for (peer_name, _peer) in other_peers {
            health_data
                .unexpected_peers
                .push((name.to_string(), peer_name.clone()));
        }
    }

    if s.dynamic_peers != 0 {
        health_data.other_errors.push(format!(
            "{name}.dynamic_peers is {} should be 0",
            s.dynamic_peers
        ));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BgpHealthData {
    // ToR state appears in summaries for multiple address families. Keying by port
    // emits one alert for each physical link.
    pub unhealthy_tor_peers: HashMap<String, String>,
    // Only transport/session failures should affect allocation policy for the
    // primary uplink. Failures to negotiate an address family reuse the same
    // probe ID but do not mean the ToR session itself is unavailable.
    pub tor_session_failures: HashSet<String>,
    pub unhealthy_route_server_peers: Vec<(String, String)>,
    pub unexpected_peers: Vec<(String, String)>,
    pub other_errors: Vec<String>,
}

impl BgpHealthData {
    fn into_health_report(
        mut self,
        hr: &mut health_report::HealthReport,
        min_healthy_links: usize,
        hbn_device_names: &HBNDeviceNames,
    ) {
        if self.other_errors.is_empty() {
            passed(hr, probe_ids::BgpStats.clone(), None);
        } else {
            self.other_errors
                .insert(0, "Failures while gathering BGP health data:".to_string());
            let err_msg = self.other_errors.join("\n");
            failed(hr, probe_ids::BgpStats.clone(), None, err_msg);
        }

        let healthy_uplink_count = hbn_device_names
            .uplinks
            .len()
            .saturating_sub(self.tor_session_failures.len());
        let minimum_satisfied = healthy_uplink_count >= min_healthy_links;
        let [primary_uplink, _] = hbn_device_names.uplinks;

        // Apply the configured redundancy suppression before preserving the
        // legacy policy that two unhealthy physical uplinks are blocking. The
        // effective set includes address family negotiation warnings as well as
        // transport failures.
        let mut effective_unhealthy_tors = self.unhealthy_tor_peers;
        if min_healthy_links == 0 {
            effective_unhealthy_tors.clear();
        } else if minimum_satisfied {
            for secondary_uplink in hbn_device_names.uplinks.iter().skip(1) {
                if self.tor_session_failures.contains(*secondary_uplink) {
                    effective_unhealthy_tors.remove(*secondary_uplink);
                }
            }
        }
        let multiple_unhealthy_tors = effective_unhealthy_tors.len() > 1;

        // Emit the unhealthy uplinks that remain after applying the configured
        // redundancy policy, with classifications based on their role and count.
        for (port_name, message) in effective_unhealthy_tors {
            let is_primary_session_failure =
                port_name == primary_uplink && self.tor_session_failures.contains(&port_name);

            let health_classifications = if multiple_unhealthy_tors {
                vec![
                    HealthAlertClassification::prevent_allocations(),
                    HealthAlertClassification::prevent_host_state_changes(),
                ]
            } else if is_primary_session_failure {
                vec![HealthAlertClassification::prevent_allocations()]
            } else {
                vec![]
            };
            hr.alerts.push(make_classified_alert(
                probe_ids::BgpPeeringTor.clone(),
                Some(port_name),
                message,
                health_classifications,
            ));
        }

        for (route_server, message) in self.unhealthy_route_server_peers.into_iter() {
            hr.alerts.push(make_alert(
                probe_ids::BgpPeeringRouteServer.clone(),
                Some(route_server.to_string()),
                message,
                true,
            ));
        }

        for (group, peer_name) in self.unexpected_peers.into_iter() {
            hr.alerts.push(make_alert(
                probe_ids::UnexpectedBgpPeer.clone(),
                Some(peer_name.clone()),
                format!("Unexpected BGP session referencing peer {peer_name} was found in {group}"),
                true,
            ));
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct BgpNetworks {
    ipv4_unicast: BgpStats,
    /// FRR can omit or null inactive address families; the configuration gate
    /// decides when this summary becomes required.
    ipv6_unicast: Option<BgpStats>,
    l2_vpn_evpn: BgpStats,
}

#[derive(Deserialize, Debug)]
#[serde(try_from = "BgpFailedSummaryWire")]
struct BgpFailedSummary {
    /// FRR omits this map when no peers fail the address-family filter.
    peers: HashMap<String, serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BgpFailedSummaryWire {
    #[serde(default)]
    peers: HashMap<String, serde::de::IgnoredAny>,
    failed_peers: Option<u32>,
    failed_peers_count: Option<u32>,
}

impl TryFrom<BgpFailedSummaryWire> for BgpFailedSummary {
    type Error = &'static str;

    fn try_from(summary: BgpFailedSummaryWire) -> Result<Self, Self::Error> {
        if summary.failed_peers.is_none() && summary.failed_peers_count.is_none() {
            return Err("missing failedPeers or failedPeersCount");
        }

        Ok(Self {
            peers: summary.peers,
        })
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct BgpStats {
    dynamic_peers: u32,
    peers: HashMap<String, BgpPeer>,
}

impl BgpStats {
    /// Returns the list of peers that are not connected to TORs
    fn other_peers(&self) -> impl Iterator<Item = (&String, &BgpPeer)> {
        lazy_static::lazy_static! {
            static ref TOR_SESSION_RE: regex::Regex = regex::Regex::new(r"^p[0-9]+_[si]f$").unwrap();
        }

        self.peers
            .iter()
            .filter(|(name, _peer)| !TOR_SESSION_RE.is_match(name))
    }
}

// We don't currently check the two pfx values because they depend on how many correctly
// configured instances we have right now, and dpu-agent doesn't know that.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct BgpPeer {
    state: String,
    // pfx_rcd: Option<u32>, // unused
    // pfx_snt: Option<u32>, // unused
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    const BGP_SUMMARY_JSON_NO_ROUTE_SERVER_SUCCESS: &str =
        include_str!("../hbn_bgp_summary_no_route_server_success.json");
    const BGP_SUMMARY_JSON_NO_ROUTE_SERVER_FAILED_TOR_PEERS: &str =
        include_str!("../hbn_bgp_summary_no_route_server_failed_tor_peers.json");
    const BGP_SUMMARY_JSON_NO_ROUTE_SERVER_SINGLE_FAILED_TOR_PEER: &str =
        include_str!("../hbn_bgp_summary_no_route_server_single_failed_tor_peer.json");
    const BGP_SUMMARY_JSON_NO_ROUTE_SERVER_WITH_TENANT_ROUTES: &str =
        include_str!("../hbn_bgp_summary_no_route_server_with_tenant_routes.json");
    const BGP_SUMMARY_JSON_WITH_ROUTE_SERVER_AND_TENANT_ROUTES: &str =
        include_str!("../hbn_bgp_summary_with_route_server_and_tenant_routes.json");
    const BGP_SUMMARY_JSON_WITH_ROUTE_SERVER_FAILED_ALL_PEERS: &str =
        include_str!("../hbn_bgp_summary_with_route_server_failed_all_peers.json");
    const BGP_IPV6_UNICAST_SUMMARY_JSON_NO_FAILED_PEERS: &str =
        include_str!("../hbn_bgp_ipv6_unicast_summary_no_failed_peers.json");

    /// One `verify_bgp_summary` scenario: a BGP summary JSON plus the host routes
    /// and route servers that frame what's expected, and the exact alerts the
    /// resulting health report should hold (sorted by `(id, target)`).
    struct Row {
        scenario: &'static str,
        json: &'static str,
        host_routes: &'static [&'static str],
        route_servers: &'static [&'static str],
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

    /// Builds a ToR BGP peering alert for `port`, the most common alert these
    /// scenarios produce.
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

    #[test]
    fn verify_bgp_summary_emits_expected_alerts() {
        check_values(
            [
                Row {
                    scenario: "all sessions established, no alerts",
                    json: BGP_SUMMARY_JSON_NO_ROUTE_SERVER_SUCCESS,
                    host_routes: &[],
                    route_servers: &[],
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "both ToR peers block allocations and state changes",
                    json: BGP_SUMMARY_JSON_NO_ROUTE_SERVER_FAILED_TOR_PEERS,
                    host_routes: &[],
                    route_servers: &[],
                    expected_alerts: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Row {
                    scenario: "failed primary prevents allocations but not state changes",
                    json: BGP_SUMMARY_JSON_NO_ROUTE_SERVER_SINGLE_FAILED_TOR_PEER,
                    host_routes: &[],
                    route_servers: &[],
                    expected_alerts: vec![tor_alert(
                        "p0_if",
                        "Session p0_if is not Established, but in state Idle",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Row {
                    scenario: "tenant route in host_routes is expected, no alerts",
                    json: BGP_SUMMARY_JSON_NO_ROUTE_SERVER_WITH_TENANT_ROUTES,
                    host_routes: &["10.217.4.78"],
                    route_servers: &[],
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "tenant route absent from host_routes is unexpected",
                    json: BGP_SUMMARY_JSON_NO_ROUTE_SERVER_WITH_TENANT_ROUTES,
                    host_routes: &[],
                    route_servers: &[],
                    expected_alerts: vec![make_alert(
                        probe_ids::UnexpectedBgpPeer.clone(),
                        Some("10.217.4.78".to_string()),
                        "Unexpected BGP session referencing peer 10.217.4.78 was found in ipv4_unicast"
                            .to_string(),
                        true,
                    )],
                },
                Row {
                    scenario: "route server present but not in route_servers list",
                    json: BGP_SUMMARY_JSON_WITH_ROUTE_SERVER_AND_TENANT_ROUTES,
                    host_routes: &["10.217.19.211"],
                    route_servers: &[],
                    expected_alerts: vec![
                        tor_alert(
                            "p0_if",
                            "Expected session for p0_if was not found in BGP peer data",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Expected session for p1_if was not found in BGP peer data",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        make_alert(
                            probe_ids::UnexpectedBgpPeer.clone(),
                            Some("10.217.126.67".to_string()),
                            "Unexpected BGP session referencing peer 10.217.126.67 was found in l2_vpn_evpn"
                                .to_string(),
                            true,
                        ),
                    ],
                },
                Row {
                    scenario: "route server and tenant routes both expected, no alerts",
                    json: BGP_SUMMARY_JSON_WITH_ROUTE_SERVER_AND_TENANT_ROUTES,
                    host_routes: &["10.217.19.211"],
                    route_servers: &["10.217.126.67"],
                    expected_alerts: vec![],
                },
                Row {
                    scenario: "route server and both TOR peers down",
                    json: BGP_SUMMARY_JSON_WITH_ROUTE_SERVER_FAILED_ALL_PEERS,
                    host_routes: &[],
                    route_servers: &["10.217.126.67"],
                    expected_alerts: vec![
                        make_alert(
                            probe_ids::BgpPeeringRouteServer.clone(),
                            Some("10.217.126.67".to_string()),
                            "Session 10.217.126.67 is not Established, but in state Active"
                                .to_string(),
                            true,
                        ),
                        tor_alert(
                            "p0_if",
                            "Session p0_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
            ]
            .map(|mut row| {
                // Sort the expected alerts the same way the run closure sorts the
                // produced ones, so rows can list alerts in any readable order.
                sort_alerts(&mut row.expected_alerts);
                let expect = row.expected_alerts.clone();
                Check {
                    scenario: row.scenario,
                    input: row,
                    expect,
                }
            }),
            |row: Row| {
                // Exercise the full ContainerExec/FRR path by collecting
                // failures, then converting them into the public health alerts.
                let route_servers: Vec<String> =
                    row.route_servers.iter().map(|s| s.to_string()).collect();
                let hbn_device_names = HBNDeviceNames::hbn_23();
                let mut report =
                    health_report::HealthReport::empty("forge-dpu-agent".to_string());
                let mut health_data = BgpHealthData::default();
                verify_bgp_summary(
                    &mut health_data,
                    row.json,
                    &BgpHealthCheckParams {
                        host_routes: row.host_routes,
                        min_healthy_links: 2,
                        route_servers: &route_servers,
                        should_check_ipv6_unicast: false,
                        hbn_device_names: &hbn_device_names,
                    },
                );
                health_data.into_health_report(&mut report, 2, &hbn_device_names);
                let mut alerts = report.alerts;
                sort_alerts(&mut alerts);
                alerts
            },
        );
    }

    /// Inputs for one ContainerExec/FRR ToR health policy scenario.
    struct TorHealthInput {
        /// Configured minimum passed to failure collection and alert classification.
        min_healthy_links: usize,
        /// FRR BGP transport state reported for the primary uplink.
        p0_state: &'static str,
        /// Compact test encoding for the secondary uplink state.
        ///
        /// Real FRR transport states pass through unchanged.
        /// `ESTABLISHED_WITHOUT_IPV6_UNICAST` instead represents an established
        /// session that appears in FRR's separate failed IPv6 unicast summary.
        p1_state: &'static str,
        /// Whether the primary appears in FRR's failed IPv6 unicast summary.
        p0_ipv6_unicast_failed: bool,
    }

    /// Marks an established p1 session whose IPv6 unicast negotiation failed.
    const ESTABLISHED_WITHOUT_IPV6_UNICAST: &str = "EstablishedWithoutIpv6Unicast";

    #[test]
    fn bgp_health_data_classifies_redundant_uplink_failures() {
        check_values(
            [
                Check {
                    scenario: "minimum zero ignores two established sessions",
                    input: TorHealthInput {
                        min_healthy_links: 0,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum zero ignores a failed primary session",
                    input: TorHealthInput {
                        min_healthy_links: 0,
                        p0_state: "Idle",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum zero ignores a failed secondary session",
                    input: TorHealthInput {
                        min_healthy_links: 0,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum zero ignores two failed sessions",
                    input: TorHealthInput {
                        min_healthy_links: 0,
                        p0_state: "Idle",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum zero ignores session and negotiation failures",
                    input: TorHealthInput {
                        min_healthy_links: 0,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum one accepts two established sessions",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum one keeps a failed primary as an allocation blocker",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Idle",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![tor_alert(
                        "p0_if",
                        "Session p0_if is not Established, but in state Idle",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Check {
                    scenario: "minimum one suppresses a failed redundant secondary",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum one blocks when both sessions fail",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Idle",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Check {
                    scenario: "minimum two accepts two established sessions",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![],
                },
                Check {
                    scenario: "minimum two keeps a failed primary as an allocation blocker",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Idle",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![tor_alert(
                        "p0_if",
                        "Session p0_if is not Established, but in state Idle",
                        ExpectedClassifications::PreventAllocations,
                    )],
                },
                Check {
                    scenario: "minimum two keeps a failed secondary visible",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![tor_alert(
                        "p1_if",
                        "Session p1_if is not Established, but in state Idle",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Check {
                    scenario: "minimum two blocks when both sessions fail",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Idle",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Check {
                    scenario: "minimum one retains a primary negotiation warning",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![tor_alert(
                        "p0_if",
                        "Session p0_if did not negotiate IPv6-unicast",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Check {
                    scenario: "minimum one retains primary negotiation warning and suppresses failed secondary",
                    input: TorHealthInput {
                        min_healthy_links: 1,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![tor_alert(
                        "p0_if",
                        "Session p0_if did not negotiate IPv6-unicast",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Check {
                    scenario: "minimum two keeps a lone primary negotiation warning unclassified",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![tor_alert(
                        "p0_if",
                        "Session p0_if did not negotiate IPv6-unicast",
                        ExpectedClassifications::Unclassified,
                    )],
                },
                Check {
                    scenario: "minimum two blocks a failed primary session with a secondary negotiation warning",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Idle",
                        p1_state: ESTABLISHED_WITHOUT_IPV6_UNICAST,
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if did not negotiate IPv6-unicast",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Check {
                    scenario: "minimum two blocks a primary negotiation warning with a failed secondary session",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Established",
                        p1_state: "Idle",
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if did not negotiate IPv6-unicast",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if is not Established, but in state Idle",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Check {
                    scenario: "minimum two blocks negotiation warnings on both uplinks",
                    input: TorHealthInput {
                        min_healthy_links: 2,
                        p0_state: "Established",
                        p1_state: ESTABLISHED_WITHOUT_IPV6_UNICAST,
                        p0_ipv6_unicast_failed: true,
                    },
                    expect: vec![
                        tor_alert(
                            "p0_if",
                            "Session p0_if did not negotiate IPv6-unicast",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                        tor_alert(
                            "p1_if",
                            "Session p1_if did not negotiate IPv6-unicast",
                            ExpectedClassifications::PreventAllocationsAndHostStateChanges,
                        ),
                    ],
                },
                Check {
                    scenario: "minimum above configured uplink count remains a configuration error",
                    input: TorHealthInput {
                        min_healthy_links: 3,
                        p0_state: "Established",
                        p1_state: "Established",
                        p0_ipv6_unicast_failed: false,
                    },
                    expect: vec![make_alert(
                        probe_ids::BgpStats.clone(),
                        None,
                        "Failures while gathering BGP health data:\nThe number of min healthy links: 3 was bigger than the number of uplinks defined by the hbn device names: 2".to_string(),
                        true,
                    )],
                },
            ],
            |input| {
                // FRR reports transport state and IPv6 unicast negotiation
                // failures separately. Build both inputs before combining them.
                let hbn_device_names = HBNDeviceNames::hbn_23();
                let (p1_session_state, p1_ipv6_unicast_failed) = match input.p1_state {
                    ESTABLISHED_WITHOUT_IPV6_UNICAST => ("Established", true),
                    state => (state, false),
                };
                let stats = BgpStats {
                    dynamic_peers: 0,
                    peers: HashMap::from([
                        (
                            "p0_if".to_string(),
                            BgpPeer {
                                state: input.p0_state.to_string(),
                            },
                        ),
                        (
                            "p1_if".to_string(),
                            BgpPeer {
                                state: p1_session_state.to_string(),
                            },
                        ),
                    ]),
                };
                let mut health_data = BgpHealthData::default();
                check_bgp_tor_routes(
                    &stats,
                    &mut health_data,
                    input.min_healthy_links,
                    &hbn_device_names,
                );

                // Model the separate FRR summary only when an uplink failed
                // IPv6 unicast negotiation.
                let failed_ipv6_unicast_json =
                    match (input.p0_ipv6_unicast_failed, p1_ipv6_unicast_failed) {
                        (false, false) => None,
                        (true, false) => {
                            Some(r#"{"peers":{"p0_if":{}},"failedPeers":1}"#)
                        }
                        (false, true) => {
                            Some(r#"{"peers":{"p1_if":{}},"failedPeers":1}"#)
                        }
                        (true, true) => Some(
                            r#"{"peers":{"p0_if":{},"p1_if":{}},"failedPeers":2}"#,
                        ),
                    };
                if let Some(failed_ipv6_unicast_json) = failed_ipv6_unicast_json {
                    verify_failed_ipv6_unicast_peers(
                        &mut health_data,
                        failed_ipv6_unicast_json,
                        input.min_healthy_links,
                        &hbn_device_names,
                    );
                }

                // Convert the combined FRR results into the exact public alert
                // set asserted by each row.
                let mut report = health_report::HealthReport::empty("forge-dpu-agent".to_string());
                health_data.into_health_report(
                    &mut report,
                    input.min_healthy_links,
                    &hbn_device_names,
                );
                sort_alerts(&mut report.alerts);
                report.alerts
            },
        );
    }

    #[test]
    fn malformed_bgp_summary_is_not_verified() {
        let mut health_data = BgpHealthData::default();

        let verified = verify_bgp_summary(
            &mut health_data,
            r#"{"ipv4Unicast":"#,
            &BgpHealthCheckParams {
                host_routes: &[],
                min_healthy_links: 2,
                route_servers: &[],
                should_check_ipv6_unicast: true,
                hbn_device_names: &HBNDeviceNames::hbn_23(),
            },
        );

        assert!(!verified);
        assert_eq!(health_data.other_errors.len(), 1);
        assert!(
            health_data.other_errors[0].starts_with("failed to deserialize BGP summary"),
            "unexpected error: {}",
            health_data.other_errors[0]
        );
    }

    enum Ipv6UnicastSummary {
        Missing,
        Null,
        Healthy,
        P0Idle,
        DynamicPeer,
        UnexpectedPeer,
        UnexpectedTorLikePeer,
        Unhealthy,
    }

    struct Ipv6UnicastInput {
        should_check_ipv6_unicast: bool,
        summary: Ipv6UnicastSummary,
    }

    #[test]
    fn ipv6_unicast_summary_checks_enabled_underlay_sessions() {
        check_values(
            [
                Check {
                    scenario: "disabled health ignores an unhealthy IPv6 summary",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: false,
                        summary: Ipv6UnicastSummary::Unhealthy,
                    },
                    expect: BgpHealthData::default(),
                },
                Check {
                    scenario: "missing enabled address family is unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::Missing,
                    },
                    expect: BgpHealthData {
                        other_errors: vec!["ipv6_unicast BGP summary was not reported".to_string()],
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "null enabled address family is unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::Null,
                    },
                    expect: BgpHealthData {
                        other_errors: vec!["ipv6_unicast BGP summary was not reported".to_string()],
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "established underlay sessions are healthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::Healthy,
                    },
                    expect: BgpHealthData::default(),
                },
                Check {
                    scenario: "idle underlay session is unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::P0Idle,
                    },
                    expect: BgpHealthData {
                        unhealthy_tor_peers: HashMap::from([(
                            "p0_if".to_string(),
                            "Session p0_if is not Established, but in state Idle".to_string(),
                        )]),
                        tor_session_failures: HashSet::from(["p0_if".to_string()]),
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "dynamic peers are unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::DynamicPeer,
                    },
                    expect: BgpHealthData {
                        other_errors: vec![
                            "ipv6_unicast.dynamic_peers is 1 should be 0".to_string(),
                        ],
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "unexpected default-VRF peer is unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::UnexpectedPeer,
                    },
                    expect: BgpHealthData {
                        unexpected_peers: vec![(
                            "ipv6_unicast".to_string(),
                            "2001:db8::2".to_string(),
                        )],
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "unconfigured ToR-like peer is unhealthy",
                    input: Ipv6UnicastInput {
                        should_check_ipv6_unicast: true,
                        summary: Ipv6UnicastSummary::UnexpectedTorLikePeer,
                    },
                    expect: BgpHealthData {
                        unexpected_peers: vec![("ipv6_unicast".to_string(), "p2_if".to_string())],
                        ..Default::default()
                    },
                },
            ],
            |input: Ipv6UnicastInput| {
                let mut summary: serde_json::Value =
                    serde_json::from_str(BGP_SUMMARY_JSON_NO_ROUTE_SERVER_SUCCESS)
                        .expect("parse known-good BGP summary fixture");
                match input.summary {
                    Ipv6UnicastSummary::Missing => {}
                    Ipv6UnicastSummary::Null => {
                        summary["ipv6Unicast"] = serde_json::Value::Null;
                    }
                    Ipv6UnicastSummary::Healthy => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                    }
                    Ipv6UnicastSummary::P0Idle => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                        summary["ipv6Unicast"]["peers"]["p0_if"]["state"] = "Idle".into();
                    }
                    Ipv6UnicastSummary::DynamicPeer => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                        summary["ipv6Unicast"]["dynamicPeers"] = 1.into();
                    }
                    Ipv6UnicastSummary::UnexpectedPeer => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                        summary["ipv6Unicast"]["peers"]["2001:db8::2"] =
                            summary["ipv4Unicast"]["peers"]["p0_if"].clone();
                    }
                    Ipv6UnicastSummary::UnexpectedTorLikePeer => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                        summary["ipv6Unicast"]["peers"]["p2_if"] =
                            summary["ipv4Unicast"]["peers"]["p0_if"].clone();
                    }
                    Ipv6UnicastSummary::Unhealthy => {
                        summary["ipv6Unicast"] = summary["ipv4Unicast"].clone();
                        summary["ipv6Unicast"]["peers"]["p0_if"]["state"] = "Idle".into();
                        summary["ipv6Unicast"]["dynamicPeers"] = 1.into();
                        summary["ipv6Unicast"]["peers"]["2001:db8::2"] =
                            summary["ipv4Unicast"]["peers"]["p0_if"].clone();
                    }
                }

                let mut health_data = BgpHealthData::default();
                verify_bgp_summary(
                    &mut health_data,
                    &summary.to_string(),
                    &BgpHealthCheckParams {
                        host_routes: &[],
                        min_healthy_links: 2,
                        route_servers: &[],
                        should_check_ipv6_unicast: input.should_check_ipv6_unicast,
                        hbn_device_names: &HBNDeviceNames::hbn_23(),
                    },
                );
                health_data
            },
        );
    }

    struct FailedIpv6SummaryInput {
        json: &'static str,
        min_healthy_links: usize,
        existing_p0_error: Option<&'static str>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FailedIpv6SummaryOutcome {
        unhealthy_tor_peers: HashMap<String, String>,
        tor_session_failures: HashSet<String>,
        other_error_count: usize,
        has_deserialization_error: bool,
    }

    #[test]
    fn failed_ipv6_unicast_summary_checks_only_required_uplinks() {
        check_values(
            [
                Check {
                    scenario: "no failed peers is healthy",
                    input: FailedIpv6SummaryInput {
                        json: BGP_IPV6_UNICAST_SUMMARY_JSON_NO_FAILED_PEERS,
                        min_healthy_links: 2,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::new(),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 0,
                        has_deserialization_error: false,
                    },
                },
                Check {
                    scenario: "required p0 negotiation failure is unhealthy",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"peers":{"p0_if":{}},"failedPeers":1}"#,
                        min_healthy_links: 1,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::from([(
                            "p0_if".to_string(),
                            "Session p0_if did not negotiate IPv6-unicast".to_string(),
                        )]),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 0,
                        has_deserialization_error: false,
                    },
                },
                Check {
                    scenario: "optional p1 negotiation failure is healthy",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"peers":{"p1_if":{}},"failedPeers":1}"#,
                        min_healthy_links: 1,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::new(),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 0,
                        has_deserialization_error: false,
                    },
                },
                Check {
                    scenario: "both required negotiation failures are unhealthy",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"peers":{"p0_if":{},"p1_if":{}},"failedPeers":2}"#,
                        min_healthy_links: 2,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::from([
                            (
                                "p0_if".to_string(),
                                "Session p0_if did not negotiate IPv6-unicast".to_string(),
                            ),
                            (
                                "p1_if".to_string(),
                                "Session p1_if did not negotiate IPv6-unicast".to_string(),
                            ),
                        ]),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 0,
                        has_deserialization_error: false,
                    },
                },
                Check {
                    scenario: "transport failure remains the primary diagnosis",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"peers":{"p0_if":{}},"failedPeers":1}"#,
                        min_healthy_links: 1,
                        existing_p0_error: Some("Session p0_if is in state Idle"),
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::from([(
                            "p0_if".to_string(),
                            "Session p0_if is in state Idle".to_string(),
                        )]),
                        tor_session_failures: HashSet::from(["p0_if".to_string()]),
                        other_error_count: 0,
                        has_deserialization_error: false,
                    },
                },
                Check {
                    scenario: "unrelated JSON payload is unhealthy",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"error":"BGP instance unavailable"}"#,
                        min_healthy_links: 2,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::new(),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 1,
                        has_deserialization_error: true,
                    },
                },
                Check {
                    scenario: "malformed failed-peer summary is unhealthy",
                    input: FailedIpv6SummaryInput {
                        json: r#"{"peers":"#,
                        min_healthy_links: 2,
                        existing_p0_error: None,
                    },
                    expect: FailedIpv6SummaryOutcome {
                        unhealthy_tor_peers: HashMap::new(),
                        tor_session_failures: HashSet::new(),
                        other_error_count: 1,
                        has_deserialization_error: true,
                    },
                },
            ],
            |input: FailedIpv6SummaryInput| {
                let mut health_data = BgpHealthData::default();
                if let Some(error) = input.existing_p0_error {
                    health_data
                        .unhealthy_tor_peers
                        .insert("p0_if".to_string(), error.to_string());
                    health_data.tor_session_failures.insert("p0_if".to_string());
                }

                verify_failed_ipv6_unicast_peers(
                    &mut health_data,
                    input.json,
                    input.min_healthy_links,
                    &HBNDeviceNames::hbn_23(),
                );

                let has_deserialization_error = health_data.other_errors.iter().any(|error| {
                    error.starts_with("failed to deserialize failed IPv6-unicast BGP summary")
                });
                FailedIpv6SummaryOutcome {
                    unhealthy_tor_peers: health_data.unhealthy_tor_peers,
                    tor_session_failures: health_data.tor_session_failures,
                    other_error_count: health_data.other_errors.len(),
                    has_deserialization_error,
                }
            },
        );
    }

    /// Orders alerts by `(id, target)` so a produced report and its expected
    /// report compare element-by-element regardless of insertion order.
    fn sort_alerts(alerts: &mut [health_report::HealthProbeAlert]) {
        alerts.sort_by(|a, b| (&a.id, &a.target).cmp(&(&b.id, &b.target)));
    }
}
