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
use std::net::IpAddr;

use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use serde::Deserialize;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};
use uuid::Uuid;

use crate::metadata::{Metadata, default_metadata_for_deserializer};

/// A single NVOS ethernet port of a switch. Both of a switch's ports reach the
/// same NVOS instance; `is_primary` marks the one NICo talks to. Exactly one port
/// per switch is primary (auto-set when a switch has a single port).
#[derive(Default, Clone, Debug, Deserialize, PartialEq, Eq, FromRow)]
pub struct ExpectedNvosInterface {
    pub mac_address: MacAddress,
    #[serde(default)]
    pub is_primary: bool,
}

/// Why a set of [`ExpectedNvosInterface`]s is not a valid switch NVOS port set.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NvosInterfaceError {
    #[error("a switch must declare at least one NVOS ethernet port")]
    NoEthernetPorts,
    #[error("duplicate NVOS ethernet port MAC address: {0}")]
    DuplicateMac(MacAddress),
    #[error(
        "exactly one NVOS ethernet port must be marked primary when more than one port is defined"
    )]
    PrimaryCount,
}

/// Validate and canonicalize a switch's NVOS ports.
///
/// Enforces the switch NVOS port contract: at least one port, no duplicate MACs,
/// and a single primary. A lone port is always the primary NICo uses, so its
/// `is_primary` flag is forced to `true` regardless of the caller's input; with
/// more than one port exactly one must already be flagged primary.
pub fn normalize_nvos_interfaces(
    interfaces: &[ExpectedNvosInterface],
) -> Result<Vec<ExpectedNvosInterface>, NvosInterfaceError> {
    if interfaces.is_empty() {
        return Err(NvosInterfaceError::NoEthernetPorts);
    }

    let mut seen = std::collections::HashSet::new();
    for interface in interfaces {
        if !seen.insert(interface.mac_address) {
            return Err(NvosInterfaceError::DuplicateMac(interface.mac_address));
        }
    }

    if let [only] = interfaces {
        return Ok(vec![ExpectedNvosInterface {
            mac_address: only.mac_address,
            is_primary: true,
        }]);
    }

    if interfaces.iter().filter(|i| i.is_primary).count() != 1 {
        return Err(NvosInterfaceError::PrimaryCount);
    }

    Ok(interfaces.to_vec())
}

#[derive(Default, Clone, Deserialize)] // Do not add debug here, it contains passwords.
#[serde(default)]
pub struct ExpectedSwitch {
    #[serde(default)]
    pub expected_switch_id: Option<Uuid>,
    pub bmc_mac_address: MacAddress,
    /// The switch's NVOS ethernet ports, in no particular order. Persisted in the
    /// `expected_switch_nvos_interfaces` table, not on the `expected_switches` row.
    #[serde(default)]
    pub nvos_interfaces: Vec<ExpectedNvosInterface>,
    pub bmc_username: String,
    pub serial_number: String,
    pub bmc_password: String,
    pub nvos_username: Option<String>,
    pub nvos_password: Option<String>,
    #[serde(default)]
    pub bmc_ip_address: Option<IpAddr>,
    /// Static IP reservation for the single wired NVOS port. Only meaningful
    /// when `nvos_interfaces` has exactly one entry; handlers reject this
    /// being set otherwise so the (mac, ip) pairing stays unambiguous.
    #[serde(default)]
    pub nvos_ip_address: Option<IpAddr>,
    #[serde(default = "default_metadata_for_deserializer")]
    pub metadata: Metadata,
    pub rack_id: Option<RackId>,
    /// When true, site-explorer skips BMC password rotation and stores the
    /// factory-default credentials in Vault as-is.
    #[serde(default)]
    pub bmc_retain_credentials: Option<bool>,
}

impl ExpectedSwitch {
    /// The MAC addresses of every NVOS port, primary first.
    pub fn nvos_mac_addresses(&self) -> Vec<MacAddress> {
        let mut interfaces: Vec<&ExpectedNvosInterface> = self.nvos_interfaces.iter().collect();
        interfaces.sort_by_key(|i| (!i.is_primary, i.mac_address.to_string()));
        interfaces.into_iter().map(|i| i.mac_address).collect()
    }

    /// The MAC of the primary NVOS port NICo talks to. A switch with a single
    /// port uses it implicitly even when no port is flagged primary.
    pub fn primary_nvos_mac_address(&self) -> Option<MacAddress> {
        if let Some(primary) = self.nvos_interfaces.iter().find(|i| i.is_primary) {
            return Some(primary.mac_address);
        }
        if let [only] = self.nvos_interfaces.as_slice() {
            return Some(only.mac_address);
        }
        None
    }
}

impl<'r> FromRow<'r, PgRow> for ExpectedSwitch {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("metadata_labels")?;
        let metadata = Metadata {
            name: row.try_get("metadata_name")?,
            description: row.try_get("metadata_description")?,
            labels: labels.0,
        };

        // Queries that need the NVOS ports select them from the
        // `expected_switch_nvos_interfaces` table as a JSON aggregate aliased
        // `nvos_interfaces`. Queries that do not include that column (and legacy
        // callers) default to no ports.
        let nvos_interfaces: Vec<ExpectedNvosInterface> = match row
            .try_get::<sqlx::types::Json<Vec<ExpectedNvosInterface>>, _>("nvos_interfaces")
        {
            Ok(json) => json.0,
            // Only a missing column is a legitimate "no ports" default;
            // a malformed JSON value must surface rather than masquerade
            // as a switch with no NVOS ports.
            Err(sqlx::Error::ColumnNotFound(_)) => Vec::new(),
            Err(e) => return Err(e),
        };

        Ok(ExpectedSwitch {
            expected_switch_id: row.try_get("expected_switch_id")?,
            bmc_mac_address: row.try_get("bmc_mac_address")?,
            nvos_interfaces,
            bmc_username: row.try_get("bmc_username")?,
            serial_number: row.try_get("serial_number")?,
            bmc_password: row.try_get("bmc_password")?,
            nvos_username: row.try_get("nvos_username")?,
            nvos_password: row.try_get("nvos_password")?,
            bmc_ip_address: row.try_get("bmc_ip_address")?,
            nvos_ip_address: row.try_get("nvos_ip_address")?,
            metadata,
            rack_id: row.try_get("rack_id")?,
            bmc_retain_credentials: row.try_get("bmc_retain_credentials")?,
        })
    }
}

#[derive(FromRow)]
pub struct LinkedExpectedSwitch {
    pub serial_number: String,
    pub bmc_mac_address: MacAddress, // from expected_switches table
    pub switch_id: Option<SwitchId>, // The switch
    pub expected_switch_id: Option<Uuid>, // The expected switch ID
    pub address: Option<IpAddr>,     // The explored BMC endpoint IP
    pub rack_id: Option<RackId>,     // The rack this switch belongs to
}

/// A request to identify an ExpectedSwitch by either ID or MAC address.
#[derive(Debug, Clone)]
pub struct ExpectedSwitchRequest {
    pub expected_switch_id: Option<Uuid>,
    pub bmc_mac_address: Option<MacAddress>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(mac: &str, is_primary: bool) -> ExpectedNvosInterface {
        ExpectedNvosInterface {
            mac_address: mac.parse().expect("valid MAC"),
            is_primary,
        }
    }

    #[test]
    fn normalize_rejects_an_empty_port_set() {
        assert_eq!(
            normalize_nvos_interfaces(&[]),
            Err(NvosInterfaceError::NoEthernetPorts)
        );
    }

    #[test]
    fn normalize_rejects_a_duplicate_mac() {
        let ports = [
            iface("aa:bb:cc:dd:ee:01", true),
            iface("aa:bb:cc:dd:ee:01", false),
        ];
        assert_eq!(
            normalize_nvos_interfaces(&ports),
            Err(NvosInterfaceError::DuplicateMac(
                "aa:bb:cc:dd:ee:01".parse().unwrap()
            ))
        );
    }

    #[test]
    fn normalize_forces_a_lone_port_primary() {
        let got = normalize_nvos_interfaces(&[iface("aa:bb:cc:dd:ee:01", false)])
            .expect("a single port is valid");
        assert_eq!(got, vec![iface("aa:bb:cc:dd:ee:01", true)]);
    }

    #[test]
    fn normalize_requires_exactly_one_primary_across_multiple_ports() {
        let none = [
            iface("aa:bb:cc:dd:ee:01", false),
            iface("aa:bb:cc:dd:ee:02", false),
        ];
        assert_eq!(
            normalize_nvos_interfaces(&none),
            Err(NvosInterfaceError::PrimaryCount)
        );

        let two = [
            iface("aa:bb:cc:dd:ee:01", true),
            iface("aa:bb:cc:dd:ee:02", true),
        ];
        assert_eq!(
            normalize_nvos_interfaces(&two),
            Err(NvosInterfaceError::PrimaryCount)
        );

        let one = [
            iface("aa:bb:cc:dd:ee:01", false),
            iface("aa:bb:cc:dd:ee:02", true),
        ];
        assert_eq!(normalize_nvos_interfaces(&one).unwrap(), one.to_vec());
    }

    #[test]
    fn nvos_mac_addresses_lists_the_primary_first_then_by_mac() {
        let switch = ExpectedSwitch {
            nvos_interfaces: vec![
                iface("aa:bb:cc:dd:ee:03", false),
                iface("aa:bb:cc:dd:ee:01", true),
                iface("aa:bb:cc:dd:ee:02", false),
            ],
            ..Default::default()
        };

        let macs: Vec<String> = switch
            .nvos_mac_addresses()
            .iter()
            .map(|mac| mac.to_string())
            .collect();

        // Primary leads regardless of MAC order; the rest follow by MAC.
        assert_eq!(
            macs,
            vec![
                "AA:BB:CC:DD:EE:01".to_string(),
                "AA:BB:CC:DD:EE:02".to_string(),
                "AA:BB:CC:DD:EE:03".to_string(),
            ]
        );
    }

    #[test]
    fn primary_nvos_mac_address_prefers_the_flag_then_a_lone_port() {
        let flagged = ExpectedSwitch {
            nvos_interfaces: vec![
                iface("aa:bb:cc:dd:ee:03", false),
                iface("aa:bb:cc:dd:ee:01", true),
            ],
            ..Default::default()
        };
        assert_eq!(
            flagged.primary_nvos_mac_address(),
            Some("aa:bb:cc:dd:ee:01".parse().unwrap())
        );

        let lone = ExpectedSwitch {
            nvos_interfaces: vec![iface("aa:bb:cc:dd:ee:09", false)],
            ..Default::default()
        };
        assert_eq!(
            lone.primary_nvos_mac_address(),
            Some("aa:bb:cc:dd:ee:09".parse().unwrap())
        );

        let unflagged = ExpectedSwitch {
            nvos_interfaces: vec![
                iface("aa:bb:cc:dd:ee:01", false),
                iface("aa:bb:cc:dd:ee:02", false),
            ],
            ..Default::default()
        };
        assert_eq!(unflagged.primary_nvos_mac_address(), None);
    }
}
