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
use std::net::{IpAddr, Ipv6Addr};

use carbide_uuid::machine::DpuMachineId;
use chrono::{DateTime, Duration, Utc};
use config_version::ConfigVersion;
use health_report::HealthReport;
use serde::{Deserialize, Serialize};

use crate::instance::status::network::InstanceNetworkStatusObservation;

/// The fabric interface status last reported by a DPU agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpuFabricInterfaceStatusObservation {
    pub interface_name: String,
    pub link_data: Option<DpuLinkStatusObservation>,
}

/// The persisted subset of link attributes reported for a DPU fabric interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpuLinkStatusObservation {
    pub link_type: Option<String>,
    pub state: Option<String>,
    pub carrier_up: Option<bool>,
    pub mtu: Option<u32>,
    pub carrier_up_count: Option<u32>,
    pub carrier_down_count: Option<u32>,
}

/// The network status that was last reported by the networking subsystem
/// Stored in a Postgres JSON field so new fields have to be Option until fully deployed
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineNetworkStatusObservation {
    pub machine_id: DpuMachineId,
    pub agent_version: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub network_config_version: Option<ConfigVersion>,
    pub client_certificate_expiry: Option<i64>,
    pub agent_version_superseded_at: Option<DateTime<Utc>>,
    pub instance_network_observation: Option<InstanceNetworkStatusObservation>,
    #[serde(default)]
    pub fabric_interfaces: Vec<DpuFabricInterfaceStatusObservation>,
}

impl MachineNetworkStatusObservation {
    pub fn any_observed_version_changed(&self, other: &Self) -> bool {
        if self.network_config_version != other.network_config_version {
            return true;
        }

        if match (
            &self.instance_network_observation,
            &other.instance_network_observation,
        ) {
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
            (Some(a), Some(b)) => a.any_observed_version_changed(b),
        } {
            return true;
        }

        false
    }

    pub fn expired_version_health_report(
        &self,
        staleness_threshold: Duration,
        prevent_allocations: bool,
    ) -> Option<HealthReport> {
        let Some(agent_version) = self.agent_version.as_ref() else {
            return Some(health_report::HealthReport::stale_agent_version(
                "forge-dpu-agent".to_string(),
                self.machine_id.to_string(),
                "Agent version is not known".to_string(),
                prevent_allocations,
            ));
        };

        if agent_version == carbide_version::v!(build_version) {
            // Same version as the server, all good.
            return None;
        }

        match self.agent_version_superseded_at {
            Some(superseded_at) => {
                let staleness = Utc::now().signed_duration_since(superseded_at);
                if staleness > staleness_threshold {
                    Some(health_report::HealthReport::stale_agent_version(
                        "forge-dpu-agent".to_string(),
                        self.machine_id.to_string(),
                        format!(
                            "Agent version is {}, which is out of date since {}",
                            agent_version,
                            superseded_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        ),
                        prevent_allocations,
                    ))
                } else {
                    None
                }
            }
            None => {
                tracing::debug!(
                        machine_id = %self.machine_id,
                        agent_version = %agent_version,
                        "DPU is on a stale agent version which we don't know about. Cannot know how stale it is, will not prevent allocations");
                None
            }
        }
    }
}

/// Desired network configuration for an instance.
/// This is persisted to a Postgres JSON column, so only use Option
/// fields for easier migrations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedHostNetworkConfig {
    pub loopback_ip: Option<IpAddr>,
    /// IPv6 loopback reserved for the FNN underlay. `None` keeps the existing
    /// IPv4-only behavior for sites without `lo-ip-v6`.
    pub loopback_ip_v6: Option<Ipv6Addr>,
    /// This is a host-level field of the "consolidated" network
    /// config served to all [DPU] agents within host machine group.
    /// This is set in the config for the host-specific row in the
    /// database, and we use it as a base layer of sorts for then
    /// merging in DPU-specific configs.
    pub use_admin_network: Option<bool>,
    pub quarantine_state: Option<ManagedHostQuarantineState>,
    pub use_admin_network_changed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedHostQuarantineState {
    pub reason: Option<String>,
    pub mode: ManagedHostQuarantineMode,
}

impl ManagedHostQuarantineState {
    pub fn reason_str(&self) -> &str {
        self.reason.as_deref().unwrap_or("")
    }

    pub fn mode_str(&self) -> &str {
        self.mode.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedHostQuarantineMode {
    BlockAllTraffic,
}

impl ManagedHostQuarantineMode {
    fn as_str(&self) -> &'static str {
        match self {
            ManagedHostQuarantineMode::BlockAllTraffic => "BlockAllTraffic",
        }
    }
}

impl Default for ManagedHostNetworkConfig {
    fn default() -> Self {
        ManagedHostNetworkConfig {
            loopback_ip: None,
            loopback_ip_v6: None,
            use_admin_network: Some(true),
            quarantine_state: None,
            use_admin_network_changed: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, scenarios, value_scenarios};
    use chrono::TimeZone;
    use config_version::ConfigVersion;

    use super::*;
    use crate::test_support::machine_snapshot::config_version;

    // A stable MachineId for status observations; `any_observed_version_changed`
    // never inspects it, so any valid id does.
    fn machine_id() -> DpuMachineId {
        DpuMachineId::from_str("fm100dt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg")
            .unwrap()
    }

    // A fixed timestamp so observations built in tests compare deterministically.
    fn observed_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    // A MachineNetworkStatusObservation carrying only the fields
    // `any_observed_version_changed` reads; everything else is a fixed default.
    fn network_status(
        network_config_version: Option<ConfigVersion>,
        instance_network_observation: Option<InstanceNetworkStatusObservation>,
    ) -> MachineNetworkStatusObservation {
        MachineNetworkStatusObservation {
            machine_id: machine_id(),
            agent_version: None,
            observed_at: observed_at(),
            network_config_version,
            client_certificate_expiry: None,
            agent_version_superseded_at: None,
            instance_network_observation,
            fabric_interfaces: Vec::new(),
        }
    }

    fn network_observation(
        config_version: ConfigVersion,
        instance_config_version: Option<ConfigVersion>,
    ) -> InstanceNetworkStatusObservation {
        InstanceNetworkStatusObservation {
            config_version,
            instance_config_version,
            interfaces: Vec::new(),
            observed_at: observed_at(),
        }
    }

    // JSON round-trips: serialize a config to JSON and deserialize it back; the
    // config must survive intact. The error type (serde_json::Error) is not
    // PartialEq, so failing rows would use `Fails`; all rows here round-trip
    // cleanly.
    #[test]
    fn test_managed_host_network_config_json_roundtrip() {
        scenarios!(
            run = |config| {
                let json = serde_json::to_string(&config).map_err(drop)?;
                serde_json::from_str::<ManagedHostNetworkConfig>(&json).map_err(drop)
            };
            "ipv4 round-trip" {
                ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    loopback_ip_v6: None,
                    use_admin_network: Some(true),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                } => Yields(ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    loopback_ip_v6: None,
                    use_admin_network: Some(true),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                }),
            }

            "generic IPv6 addresses round-trip" {
                ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V6(Ipv6Addr::new(
                        0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
                    ))),
                    loopback_ip_v6: None,
                    use_admin_network: Some(false),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                } => Yields(ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V6(Ipv6Addr::new(
                        0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
                    ))),
                    loopback_ip_v6: None,
                    use_admin_network: Some(false),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                }),
            }

            "dedicated IPv6 loopback round-trip" {
                ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    loopback_ip_v6: Some(Ipv6Addr::new(
                        0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
                    )),
                    use_admin_network: Some(false),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                } => Yields(ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    loopback_ip_v6: Some(Ipv6Addr::new(
                        0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
                    )),
                    use_admin_network: Some(false),
                    quarantine_state: None,
                    use_admin_network_changed: None,
                }),
            }
        );
    }

    // `loopback_ip_v6` is optional in persisted machine JSON, so rows written
    // before this field existed keep loading it as `None`. Once populated, the
    // `Ipv6Addr` type rejects an IPv4 value before it can reach FNN rendering.
    #[test]
    fn test_managed_host_network_config_deserialize_json() {
        scenarios!(
            run = |json| {
                serde_json::from_str::<ManagedHostNetworkConfig>(json)
                    .map(|c| (c.loopback_ip, c.loopback_ip_v6))
                    .map_err(drop)
            };
            "legacy ipv4 json" {
                r#"{
                            "loopback_ip": "10.0.0.1",
                            "secondary_overlay_vtep_ip": "172.16.0.5",
                            "use_admin_network": true,
                            "quarantine_state": null
                }"# => Yields((
                    Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    None,
                )),
            }

            "generic IPv6 address json" {
                r#"{
                            "loopback_ip": "2001:db8::1",
                            "use_admin_network": true,
                            "quarantine_state": null
                }"# => Yields((
                    Some(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))),
                    None,
                )),
            }

            "dedicated IPv6 loopback json" {
                r#"{
                            "loopback_ip": "10.0.0.1",
                            "loopback_ip_v6": "2001:db8::1",
                            "use_admin_network": true,
                            "quarantine_state": null
                }"# => Yields((
                    Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    Some(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
                )),
            }

            "IPv4 rejected for dedicated IPv6 loopback" {
                r#"{"loopback_ip_v6": "192.0.2.1"}"# => Fails,
            }
        );
    }

    // Ensure that the JSON representation of an IPv4 address under IpAddr is
    // identical to what Ipv4Addr would have produced. It should be, but better
    // safe than sorry, and backwards compatibility is key here, even though
    // it's not really backwards.
    #[test]
    fn test_managed_host_network_config_ipv4_json_format_unchanged() {
        let config = ManagedHostNetworkConfig {
            loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            loopback_ip_v6: None,
            use_admin_network: Some(true),
            quarantine_state: None,
            use_admin_network_changed: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        // Ensure IpAddr serializes IPv4 same as Ipv4Addr.
        assert!(json.contains(r#""loopback_ip":"10.0.0.1""#), "json: {json}");
        assert!(!json.contains("secondary_overlay_vtep_ip"), "json: {json}");
    }

    // Ensure default ManagedHostNetworkConfig is still all-None/Some(true),
    // etc etc, and unaffected by the type change to IpAddr for v6 support.
    // Folded from four hand-written asserts into a table that projects each
    // default field; equality of the whole default config is exercised by the
    // round-trip test.
    #[test]
    fn test_managed_host_network_config_default() {
        let default = ManagedHostNetworkConfig::default();
        assert_eq!(default.loopback_ip_v6, None);

        value_scenarios!(
            run = |ip| ip;
            "loopback_ip defaults to None" {
                default.loopback_ip => None,
            }
        );
        value_scenarios!(
            run = |flag| flag;
            "use_admin_network defaults to Some(true)" {
                default.use_admin_network => Some(true),
            }

            "use_admin_network_changed defaults to None" {
                default.use_admin_network_changed => None,
            }
        );
        Check {
            scenario: "quarantine_state defaults to None",
            input: default.quarantine_state,
            expect: None,
        }
        .check(|qs| qs);
    }

    // ManagedHostQuarantineState::reason_str() returns the reason or an empty
    // string when None. ManagedHostQuarantineMode has a single variant today;
    // its as_str()/mode_str() must render it exactly so the persisted form is
    // stable.
    #[test]
    fn test_quarantine_reason_str() {
        value_scenarios!(
            run = |state| state.reason_str().to_string();
            "present reason passes through" {
                ManagedHostQuarantineState {
                    reason: Some("flooding the fabric".to_string()),
                    mode: ManagedHostQuarantineMode::BlockAllTraffic,
                } => "flooding the fabric".to_string(),
            }

            "empty reason stays empty" {
                ManagedHostQuarantineState {
                    reason: Some(String::new()),
                    mode: ManagedHostQuarantineMode::BlockAllTraffic,
                } => String::new(),
            }

            "missing reason yields empty string" {
                ManagedHostQuarantineState {
                    reason: None,
                    mode: ManagedHostQuarantineMode::BlockAllTraffic,
                } => String::new(),
            }
        );
    }

    #[test]
    fn test_quarantine_mode_str() {
        value_scenarios!(
            run = |mode| {
                ManagedHostQuarantineState { reason: None, mode }
                    .mode_str()
                    .to_string()
            };
            "block-all-traffic renders its name" {
                ManagedHostQuarantineMode::BlockAllTraffic => "BlockAllTraffic".to_string(),
            }
        );
    }

    // any_observed_version_changed compares the network config version and then
    // the nested instance-network observation, via a None/Some pairing that
    // delegates to its own comparison. Enumerate every arm: matching versions,
    // differing versions, each None/Some transition, and a deeper change inside
    // the Some/Some pair.
    #[test]
    fn test_any_observed_version_changed() {
        let v1 = config_version(1);
        let v2 = config_version(2);

        value_scenarios!(
            run = |(a, b)| a.any_observed_version_changed(&b);
            "identical observations -> unchanged" {
                (
                    network_status(Some(v1), None),
                    network_status(Some(v1), None),
                ) => false,
            }

            "both network config versions None -> unchanged" {
                (
                    network_status(None, None),
                    network_status(None, None),
                ) => false,
            }

            "network config version differs -> changed" {
                (
                    network_status(Some(v1), None),
                    network_status(Some(v2), None),
                ) => true,
            }

            "network config version None vs Some -> changed" {
                (
                    network_status(None, None),
                    network_status(Some(v1), None),
                ) => true,
            }

            "network config version Some vs None -> changed" {
                (
                    network_status(Some(v1), None),
                    network_status(None, None),
                ) => true,
            }

            "instance observation None vs Some -> changed" {
                (
                    network_status(Some(v1), None),
                    network_status(Some(v1), Some(network_observation(v1, None))),
                ) => true,
            }

            "instance observation Some vs None -> changed" {
                (
                    network_status(Some(v1), Some(network_observation(v1, None))),
                    network_status(Some(v1), None),
                ) => true,
            }

            "instance observation Some/Some identical -> unchanged" {
                (
                    network_status(Some(v1), Some(network_observation(v1, Some(v1)))),
                    network_status(Some(v1), Some(network_observation(v1, Some(v1)))),
                ) => false,
            }

            "instance observation inner config version differs -> changed" {
                (
                    network_status(Some(v1), Some(network_observation(v1, None))),
                    network_status(Some(v1), Some(network_observation(v2, None))),
                ) => true,
            }

            "instance observation inner instance-config version differs -> changed" {
                (
                    network_status(Some(v1), Some(network_observation(v1, Some(v1)))),
                    network_status(Some(v1), Some(network_observation(v1, Some(v2)))),
                ) => true,
            }

        );
    }

    // Deserialize raw JSON whose shape is malformed or whose IP strings are
    // invalid; serde_json::Error is not PartialEq, so rejected rows use `Fails`.
    // Also covers a populated quarantine_state, which the earlier deserialize
    // table left null.
    #[test]
    fn test_managed_host_network_config_deserialize_errors() {
        scenarios!(
            run = |json| serde_json::from_str::<ManagedHostNetworkConfig>(json).map_err(drop);
            "well-formed with quarantine state" {
                r#"{
                            "loopback_ip": "10.0.0.1",
                            "use_admin_network": false,
                            "quarantine_state": {
                                "reason": "noisy",
                                "mode": "BlockAllTraffic"
                            }
                        }"# => Yields(ManagedHostNetworkConfig {
                    loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                    loopback_ip_v6: None,
                    use_admin_network: Some(false),
                    quarantine_state: Some(ManagedHostQuarantineState {
                        reason: Some("noisy".to_string()),
                        mode: ManagedHostQuarantineMode::BlockAllTraffic,
                    }),
                    use_admin_network_changed: None,
                }),
            }

            "empty object defaults all optional fields" {
                "{}" => Yields(ManagedHostNetworkConfig {
                    loopback_ip: None,
                    loopback_ip_v6: None,
                    use_admin_network: None,
                    quarantine_state: None,
                    use_admin_network_changed: None,
                }),
            }

            "invalid loopback ip string is rejected" {
                r#"{"loopback_ip": "not-an-ip"}"# => Fails,
            }

            "unknown quarantine mode is rejected" {
                r#"{"quarantine_state": {"mode": "Nope"}}"# => Fails,
            }

            "wrong type for use_admin_network is rejected" {
                r#"{"use_admin_network": "true"}"# => Fails,
            }

            "truncated json is rejected" {
                r#"{"loopback_ip": "10.0.0.1""# => Fails,
            }

            "non-object json is rejected" {
                "[]" => Fails,
            }
        );
    }
}
