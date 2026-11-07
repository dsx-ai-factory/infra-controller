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

use std::net::IpAddr;
use std::str::FromStr;

use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

/// One NVOS ethernet port of a switch: its MAC and whether it is the primary
/// (eth0) port NICo talks to. Parsed from a `--nvos-interface <MAC>[,primary]`
/// flag value, or deserialized from the structured `nvos_interfaces` entries of
/// a bulk `replace-all` JSON file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NvosInterfaceArg {
    pub(crate) mac_address: MacAddress,
    #[serde(default)]
    pub(crate) is_primary: bool,
}

impl FromStr for NvosInterfaceArg {
    type Err = String;

    /// Parse `<MAC>[,primary]`: a MAC address optionally followed by the literal
    /// `primary` marker, separated by a comma (MACs use `:` so there is no
    /// ambiguity). Whitespace around each part is ignored.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(',').map(str::trim);
        let mac = parts.next().unwrap_or_default();
        let mac_address = mac
            .parse::<MacAddress>()
            .map_err(|_| format!("invalid NVOS MAC address: '{mac}'"))?;
        let mut is_primary = false;
        for modifier in parts {
            match modifier {
                "primary" => is_primary = true,
                "" => {}
                other => {
                    return Err(format!(
                        "unknown --nvos-interface modifier: '{other}'; expected 'primary'"
                    ));
                }
            }
        }
        Ok(Self {
            mac_address,
            is_primary,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExpectedSwitchJson {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) bmc_username: String,
    pub(crate) bmc_password: String,
    pub(crate) switch_serial_number: String,
    /// The switch's NVOS ethernet ports. Mark one entry `"is_primary": true` to
    /// choose eth0; a single-port switch is primary implicitly and omitting the
    /// flag on a multi-port switch defaults the first entry to primary.
    pub(crate) nvos_interfaces: Vec<NvosInterfaceArg>,
    pub(crate) nvos_username: Option<String>,
    pub(crate) nvos_password: Option<String>,
    #[serde(default)]
    pub(crate) metadata: Option<rpc::forge::Metadata>,
    pub(crate) rack_id: Option<RackId>,
    pub(crate) bmc_ip_address: Option<IpAddr>,
    #[serde(default)]
    pub(crate) nvos_ip_address: Option<IpAddr>,
    #[serde(default)]
    pub(crate) bmc_retain_credentials: Option<bool>,
}

/// Builds the structured `nvos_interfaces` wire list from the per-port CLI or
/// JSON inputs. When no port is marked primary, the first is flagged; a
/// single-port switch is primary implicitly. Core validates the resulting set. Call
/// [`ensure_single_primary`] first to reject more than one primary.
pub(crate) fn nvos_interfaces_wire(
    interfaces: &[NvosInterfaceArg],
) -> Vec<rpc::forge::ExpectedNvosInterface> {
    let any_primary = interfaces.iter().any(|interface| interface.is_primary);
    interfaces
        .iter()
        .enumerate()
        .map(|(index, interface)| rpc::forge::ExpectedNvosInterface {
            mac_address: interface.mac_address.to_string(),
            is_primary: if any_primary {
                interface.is_primary
            } else {
                index == 0
            },
        })
        .collect()
}

/// Reject a port set that marks more than one port primary. The server also
/// enforces a single primary, but checking here fails fast without a round trip.
pub(crate) fn ensure_single_primary(interfaces: &[NvosInterfaceArg]) -> Result<(), String> {
    if interfaces
        .iter()
        .filter(|interface| interface.is_primary)
        .count()
        > 1
    {
        return Err("at most one --nvos-interface may be marked 'primary'".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_json_requires_the_structured_port_field() {
        for (field, ports, accepted) in [
            (
                "nvos_interfaces",
                serde_json::json!([{"mac_address": "02:00:00:00:00:01", "is_primary": true}]),
                true,
            ),
            (
                "nvos_mac_addresses",
                serde_json::json!(["02:00:00:00:00:01"]),
                false,
            ),
        ] {
            let mut value = serde_json::json!({
                "bmc_mac_address": "02:00:00:00:00:02",
                "bmc_username": "admin",
                "bmc_password": "test-password",
                "switch_serial_number": "JSON-PORTS",
                "expected_switch_id": "12345678-1234-5678-90ab-cdef01234567"
            });
            value[field] = ports;
            let result = serde_json::from_value::<ExpectedSwitchJson>(value);
            if accepted {
                assert!(result.is_ok(), "{field}: {result:?}");
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("missing field `nvos_interfaces`")
                );
            }
        }
    }

    fn wire(values: &[&str]) -> Vec<(String, bool)> {
        let interfaces: Vec<NvosInterfaceArg> = values
            .iter()
            .map(|value| value.parse().expect("value should parse"))
            .collect();
        nvos_interfaces_wire(&interfaces)
            .into_iter()
            .map(|interface| (interface.mac_address, interface.is_primary))
            .collect()
    }

    #[test]
    fn parses_mac_with_optional_primary_marker() {
        let plain: NvosInterfaceArg = "aa:bb:cc:dd:ee:ff".parse().unwrap();
        assert_eq!(plain.mac_address.to_string(), "AA:BB:CC:DD:EE:FF");
        assert!(!plain.is_primary);

        let primary: NvosInterfaceArg = "aa:bb:cc:dd:ee:ff,primary".parse().unwrap();
        assert!(primary.is_primary);

        // Whitespace around the parts is tolerated.
        let spaced: NvosInterfaceArg = " aa:bb:cc:dd:ee:ff , primary ".parse().unwrap();
        assert!(spaced.is_primary);
    }

    #[test]
    fn rejects_bad_mac_and_unknown_modifier() {
        assert!("not-a-mac".parse::<NvosInterfaceArg>().is_err());
        let unknown = "aa:bb:cc:dd:ee:ff,secondary"
            .parse::<NvosInterfaceArg>()
            .unwrap_err();
        assert!(unknown.contains("secondary"), "{unknown}");
    }

    #[test]
    fn wire_defaults_first_port_primary_when_none_marked() {
        assert_eq!(
            wire(&["aa:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:00"]),
            vec![
                ("AA:BB:CC:DD:EE:FF".to_string(), true),
                ("AA:BB:CC:DD:EE:00".to_string(), false),
            ]
        );
    }

    #[test]
    fn wire_honors_explicit_primary_marker() {
        assert_eq!(
            wire(&["aa:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:00,primary"]),
            vec![
                ("AA:BB:CC:DD:EE:FF".to_string(), false),
                ("AA:BB:CC:DD:EE:00".to_string(), true),
            ]
        );
    }

    #[test]
    fn ensure_single_primary_rejects_two_primaries() {
        let two: Vec<NvosInterfaceArg> = ["aa:bb:cc:dd:ee:ff,primary", "aa:bb:cc:dd:ee:00,primary"]
            .iter()
            .map(|value| value.parse().unwrap())
            .collect();
        assert!(ensure_single_primary(&two).is_err());

        let one: Vec<NvosInterfaceArg> = ["aa:bb:cc:dd:ee:ff,primary", "aa:bb:cc:dd:ee:00"]
            .iter()
            .map(|value| value.parse().unwrap())
            .collect();
        assert!(ensure_single_primary(&one).is_ok());
    }
}
