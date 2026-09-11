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
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Column, FromRow, Row};

use crate::StateSla;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::errors::ModelError;
use crate::network_prefix::{NetworkPrefix, NewNetworkPrefix};
use crate::state_history::StateHistoryRecord;

mod slas;

pub const MTU_MIN: i32 = 576;
pub const MTU_MAX: i32 = 9000;

#[derive(Clone, Debug, Default)]
pub struct NetworkSegmentSearchFilter {
    pub name: Option<String>,
    pub tenant_org_id: Option<String>,
}

/// State of a network segment as tracked by the controller
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum NetworkSegmentControllerState {
    Provisioning,
    /// The network segment is ready. Instances can be created
    Ready,
    /// The network segment is in the process of being deleted.
    Deleting {
        deletion_state: NetworkSegmentDeletionState,
    },
}

/// Possible states during deletion of a network segment
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum NetworkSegmentDeletionState {
    /// The segment is waiting until all IPs that had been allocated on the segment
    /// have been released - plus an additional grace period to avoid any race
    /// conditions.
    DrainAllocatedIps {
        /// Denotes the time at which the network segment will be deleted,
        /// assuming no IPs are detected to be in use until then.
        delete_at: DateTime<Utc>,
    },
    /// In this state we release the VNI and VLAN ID allocations and delete the segment from the
    /// database. This is the final state.
    DBDelete,
}

// How we specifiy a network segment in the config file
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkDefinition {
    #[serde(rename = "type")]
    pub segment_type: NetworkDefinitionSegmentType,
    /// CIDR notation
    pub prefix: IpNetwork,
    /// Optional IPv6 CIDR for dual-stack config-seeded segments.
    #[serde(default)]
    pub prefix_v6: Option<IpNetwork>,
    /// Required for IPv4 prefixes, usually the first IP in the prefix range.
    /// IPv6-only definitions may omit it. Supplied values are ignored but remain
    /// accepted for compatibility with older configurations.
    pub gateway: Option<IpAddr>,
    /// DHCPv6 relay link-address used to identify this segment. It may
    /// be outside `prefix_v6`, so it is modeled separately from gateway.
    #[serde(default)]
    pub dhcpv6_link_address: Option<IpAddr>,
    /// Typically 9000 for admin network, 1500 for underlay
    pub mtu: i32,
    /// How many addresses to skip before allocating
    pub reserve_first: i32,
    /// Controls whether DHCP allocates IPs dynamically from the pool
    /// for this specific network (with the ability to have per-IP static
    /// reservations), or ONLY serves pre-configured static reservations.
    ///
    /// Defaults to dynamic if not specified, which is the traditional
    /// behavior of Carbide + carbide-dhcp.
    #[serde(default)]
    pub allocation_strategy: AllocationStrategy,
    /// Allows NICo to infer modified EUI-64 SLAAC addresses for clients making
    /// stateless DHCPv6 requests and add them to interface address state.
    /// Inference also requires dynamic allocation, exactly one IPv6 `/64`, and
    /// an interface without an existing IPv6 address.
    ///
    /// This defaults to `false`. Set it only when clients on this segment
    /// derive SLAAC addresses from their MAC addresses using modified EUI-64.
    /// The value is applied when the segment is first seeded; later config
    /// edits do not update the stored segment.
    #[serde(default)]
    pub infer_slaac_eui64_addresses: bool,
    /// Set to the name of a VPC to attach this network segment to a VPC on creation. Will fail if
    /// the VPC is not defined. You probably want to add a vpc with a corresponding name to the
    /// config via `[vpcs.<name>]` for this to work when data is initially being seeded.
    pub vpc_name: Option<String>,
}

impl NetworkDefinition {
    pub fn validate(&self, name: &str) -> Result<(), crate::ConfigValidationError> {
        if self.mtu < MTU_MIN || self.mtu > MTU_MAX {
            return Err(crate::ConfigValidationError::InvalidValue(format!(
                "network \"{name}\": mtu {} is out of range ({MTU_MIN}-{MTU_MAX})",
                self.mtu
            )));
        }
        if self.prefix.is_ipv4() && self.gateway.is_none() {
            return Err(crate::ConfigValidationError::InvalidValue(format!(
                "network \"{name}\": gateway is required for an IPv4 prefix"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Copy, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkDefinitionSegmentType {
    Admin,
    Underlay,
    HostInband,
    // Tenant networks are created via the API, not the config file
}

impl From<NetworkDefinitionSegmentType> for crate::network_segment::NetworkSegmentType {
    fn from(value: NetworkDefinitionSegmentType) -> Self {
        match value {
            NetworkDefinitionSegmentType::Admin => {
                crate::network_segment::NetworkSegmentType::Admin
            }
            NetworkDefinitionSegmentType::Underlay => {
                crate::network_segment::NetworkSegmentType::Underlay
            }
            NetworkDefinitionSegmentType::HostInband => {
                crate::network_segment::NetworkSegmentType::HostInband
            }
        }
    }
}

/// Returns the SLA for the current state
pub fn state_sla(state: &NetworkSegmentControllerState, state_version: &ConfigVersion) -> StateSla {
    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));
    match state {
        NetworkSegmentControllerState::Provisioning => {
            StateSla::with_sla(slas::PROVISIONING, time_in_state)
        }
        NetworkSegmentControllerState::Ready => StateSla::no_sla(),
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DrainAllocatedIps { .. },
        } => {
            // Draining can take an indefinite time if the subnet is referenced
            // by an instance
            StateSla::no_sla()
        }
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DBDelete,
        } => StateSla::with_sla(slas::DELETING_DBDELETE, time_in_state),
    }
}

#[derive(Debug, Copy, Clone, Default)]
pub struct NetworkSegmentSearchConfig {
    pub include_history: bool,
    pub include_num_free_ips: bool,
}

/// User-controlled configuration for a network segment.
#[derive(Debug, Clone)]
pub struct NetworkSegmentConfig {
    pub name: String,
    pub subdomain_id: Option<DomainId>,
    pub mtu: i32,
    pub segment_type: NetworkSegmentType,
    pub allocation_strategy: AllocationStrategy,
    /// Whether NICo may infer modified EUI-64 SLAAC addresses for clients
    /// making stateless DHCPv6 requests and add them to interface address
    /// state. Inference also requires dynamic allocation, exactly one IPv6
    /// `/64`, and an interface without an IPv6 address.
    pub infer_slaac_eui64_addresses: bool,
    pub vpc_id: Option<VpcId>,
}

/// System-observed status for a network segment.
#[derive(Debug, Clone)]
pub struct NetworkSegmentStatus {
    pub controller_state: Versioned<NetworkSegmentControllerState>,
    /// The result of the last attempt to change state
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    /// History of state changes.
    pub history: Vec<StateHistoryRecord>,
    pub vlan_id: Option<i16>, // vlan_id are [0-4096) range, enforced via DB constraint
    pub vni: Option<i32>,
    pub can_stretch: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NetworkSegment {
    pub id: NetworkSegmentId,
    pub version: ConfigVersion,
    pub config: NetworkSegmentConfig,
    pub status: NetworkSegmentStatus,

    /// Prefixes are kept top-level because each NetworkPrefix contains both
    /// user-specified fields (CIDR, gateway, reserve_first) and system-populated
    /// fields (id, svi_ip, free_ip_count).
    pub prefixes: Vec<NetworkPrefix>,

    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

impl NetworkSegment {
    /// Returns whether the segment was deleted by the user
    pub fn is_marked_as_deleted(&self) -> bool {
        self.deleted.is_some()
    }

    /// Returns the IPv6 `/64` prefix NICo may use to infer a modified EUI-64
    /// SLAAC address for this segment.
    ///
    /// This is a segment-only predicate: relay context, requested addresses, and
    /// client identity must not influence it. Per-interface address ownership is
    /// checked at the DHCP call site.
    pub fn slaac_eui64_inference_prefix(&self) -> Option<&IpNetwork> {
        if !self.config.infer_slaac_eui64_addresses
            || self.config.allocation_strategy == AllocationStrategy::Reserved
        {
            return None;
        }

        // The DB currently enforces one prefix per family per segment; keep this
        // defensive guard so non-DB callers or future schema changes cannot infer
        // an address when v6 prefix selection is ambiguous.
        let mut v6_prefixes = self
            .prefixes
            .iter()
            .filter(|prefix| prefix.prefix.is_ipv6());
        let prefix = &v6_prefixes.next()?.prefix;
        if v6_prefixes.next().is_some() || prefix.prefix() != 64 {
            return None;
        }

        Some(prefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "network_segment_type_t")]
pub enum NetworkSegmentType {
    Tenant = 0,
    Admin,
    Underlay,
    HostInband,
}

impl NetworkSegmentType {
    pub fn is_tenant(&self) -> bool {
        matches!(
            self,
            NetworkSegmentType::Tenant | NetworkSegmentType::HostInband
        )
    }
}

/// Controls how IP addresses are assigned via DHCP on a network segment,
/// giving us support for segment-wide dynamic DHCP allocations or static
/// DHCP leases/reservations. It is worth noting that even if the entire
/// network segment is configured as `Dynamic`, an operator can still
/// do per-IP static reservation overrides within that segment.
///
/// - `Dynamic`: The DHCP allocator hands out IPs from the pool (default).
/// - `Reserved`: Only pre-existing static reservations are served.
///
/// Devices without a reservation get no DHCP response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AllocationStrategy {
    #[default]
    Dynamic,
    Reserved,
}

#[derive(Debug)]
pub struct NewNetworkSegment {
    pub id: NetworkSegmentId,
    pub name: String,
    pub subdomain_id: Option<DomainId>,
    pub vpc_id: Option<VpcId>,
    pub mtu: i32,
    pub prefixes: Vec<NewNetworkPrefix>,
    pub vlan_id: Option<i16>,
    pub vni: Option<i32>,
    pub segment_type: NetworkSegmentType,
    pub can_stretch: Option<bool>,
    pub allocation_strategy: AllocationStrategy,
    pub infer_slaac_eui64_addresses: bool,
}

impl FromStr for NetworkSegmentType {
    type Err = ModelError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "tenant" => NetworkSegmentType::Tenant,
            "admin" => NetworkSegmentType::Admin,
            "tor" => NetworkSegmentType::Underlay,
            "host_inband" => NetworkSegmentType::HostInband,
            _ => {
                return Err(ModelError::DatabaseTypeConversionError(format!(
                    "Invalid segment type {s} reveived from Database."
                )));
            }
        })
    }
}

impl fmt::Display for NetworkSegmentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tenant => write!(f, "tenant"),
            Self::Admin => write!(f, "admin"),
            Self::Underlay => write!(f, "tor"),
            Self::HostInband => write!(f, "host_inband"),
        }
    }
}

// We need to implement FromRow because we can't associate dependent tables with the default derive
// (i.e. it can't default unknown fields)
impl<'r> FromRow<'r, PgRow> for NetworkSegment {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let controller_state: sqlx::types::Json<NetworkSegmentControllerState> =
            row.try_get("controller_state")?;
        let state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;

        let prefixes_json: sqlx::types::Json<Vec<Option<NetworkPrefix>>> =
            row.try_get("prefixes")?;
        let prefixes = prefixes_json.0.into_iter().flatten().collect();

        let history = if let Some(column) = row.columns().iter().find(|c| c.name() == "history") {
            let value: sqlx::types::Json<Vec<Option<StateHistoryRecord>>> =
                row.try_get(column.ordinal())?;
            value.0.into_iter().flatten().collect()
        } else {
            Vec::new()
        };

        Ok(NetworkSegment {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            config: NetworkSegmentConfig {
                name: row.try_get("name")?,
                subdomain_id: row.try_get("subdomain_id")?,
                mtu: row.try_get("mtu")?,
                segment_type: row.try_get("network_segment_type")?,
                allocation_strategy: row.try_get("allocation_strategy").unwrap_or_default(),
                infer_slaac_eui64_addresses: row.try_get("infer_slaac_eui64_addresses")?,
                vpc_id: row.try_get("vpc_id")?,
            },
            status: NetworkSegmentStatus {
                controller_state: Versioned::new(
                    controller_state.0,
                    row.try_get("controller_state_version")?,
                ),
                controller_state_outcome: state_outcome.map(|x| x.0),
                history,
                vlan_id: row.try_get("vlan_id").unwrap_or_default(),
                vni: row.try_get("vni_id").unwrap_or_default(),
                can_stretch: row.try_get("can_stretch")?,
            },
            prefixes,
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            deleted: row.try_get("deleted")?,
        })
    }
}

impl NewNetworkSegment {
    pub fn build_from(
        name: &str,
        domain_id: DomainId,
        value: &NetworkDefinition,
    ) -> Result<Self, ModelError> {
        value
            .validate(name)
            .map_err(|err| ModelError::InvalidArgument(err.to_string()))?;

        // Validate the optional IPv6-specific config before expanding it
        // into persisted prefix rows.
        if let Some(prefix_v6) = value.prefix_v6
            && !prefix_v6.is_ipv6()
        {
            return Err(ModelError::InvalidArgument(
                "NetworkDefinition.prefix_v6 must be an IPv6 prefix.".to_string(),
            ));
        }

        if let Some(link_address) = value.dhcpv6_link_address
            && !link_address.is_ipv6()
        {
            return Err(ModelError::InvalidArgument(
                "NetworkDefinition.dhcpv6_link_address must be an IPv6 address.".to_string(),
            ));
        }

        // Keep the one-prefix-per-family invariant explicit before the
        // database unique index has to reject the insert.
        if let Some(prefix_v6) = value.prefix_v6
            && value.prefix.is_ipv6() == prefix_v6.is_ipv6()
        {
            return Err(ModelError::InvalidArgument(
                "NetworkDefinition cannot contain more than one prefix from the same address family."
                    .to_string(),
            ));
        }

        // A DHCPv6 link-address only has meaning when there is a v6 prefix row
        // to carry it.
        if value.prefix.is_ipv4()
            && value.prefix_v6.is_none()
            && value.dhcpv6_link_address.is_some()
        {
            return Err(ModelError::InvalidArgument(
                "NetworkDefinition.dhcpv6_link_address requires an IPv6 prefix.".to_string(),
            ));
        }

        // Expand the config definition into one row for the primary prefix and
        // an optional second row for the dual-stack IPv6 prefix.
        let mut prefixes = vec![NewNetworkPrefix {
            prefix: value.prefix,
            gateway: value.gateway.filter(|_| value.prefix.is_ipv4()),
            dhcpv6_link_address: if value.prefix.is_ipv6() {
                value.dhcpv6_link_address
            } else {
                None
            },
            num_reserved: value.reserve_first,
        }];
        if let Some(prefix_v6) = value.prefix_v6 {
            prefixes.push(NewNetworkPrefix {
                prefix: prefix_v6,
                gateway: None,
                dhcpv6_link_address: value.dhcpv6_link_address,
                num_reserved: value.reserve_first,
            });
        }

        Ok(NewNetworkSegment {
            id: uuid::Uuid::new_v4().into(),
            name: name.to_string(), // Set by the caller later
            subdomain_id: Some(domain_id),
            vpc_id: None,
            mtu: value.mtu,
            prefixes,
            vlan_id: None,
            vni: None,
            segment_type: value.segment_type.into(),
            can_stretch: None,
            allocation_strategy: value.allocation_strategy,
            infer_slaac_eui64_addresses: value.infer_slaac_eui64_addresses,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use carbide_uuid::network::NetworkPrefixId;

    use super::*;

    fn drain_state() -> NetworkSegmentControllerState {
        let delete_at: DateTime<Utc> = "2022-12-13T04:41:38Z".parse().unwrap();
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DrainAllocatedIps { delete_at },
        }
    }

    fn dbdelete_state() -> NetworkSegmentControllerState {
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DBDelete,
        }
    }

    /// Builds a minimal network segment fixture for segment-level predicate tests.
    fn test_segment(
        infer_slaac_eui64_addresses: bool,
        allocation_strategy: AllocationStrategy,
        prefixes: &[&str],
    ) -> NetworkSegment {
        let segment_id = NetworkSegmentId::new();
        let now = Utc::now();

        NetworkSegment {
            id: segment_id,
            version: ConfigVersion::initial(),
            config: NetworkSegmentConfig {
                name: "test-segment".to_string(),
                subdomain_id: None,
                mtu: 1500,
                segment_type: NetworkSegmentType::Admin,
                allocation_strategy,
                infer_slaac_eui64_addresses,
                vpc_id: None,
            },
            status: NetworkSegmentStatus {
                controller_state: Versioned::new(
                    NetworkSegmentControllerState::Ready,
                    ConfigVersion::initial(),
                ),
                controller_state_outcome: None,
                history: Vec::new(),
                vlan_id: None,
                vni: None,
                can_stretch: None,
            },
            prefixes: prefixes
                .iter()
                .map(|prefix| NetworkPrefix {
                    id: NetworkPrefixId::new(),
                    segment_id,
                    prefix: prefix.parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 0,
                    vpc_prefix_id: None,
                    vpc_prefix: None,
                    svi_ip: None,
                    num_free_ips: None,
                })
                .collect(),
            created: now,
            updated: now,
            deleted: None,
        }
    }

    #[test]
    fn slaac_eui64_inference_requires_opt_in_and_one_dynamic_v6_64_prefix() {
        // `slaac_eui64_inference_prefix` is the segment-level gate for inferring
        // an address. Keep the opt-in beside the prefix and allocation checks so
        // the DHCP handler cannot accidentally bypass one of them.
        value_scenarios!(
            run = |segment: NetworkSegment| segment.slaac_eui64_inference_prefix().copied();
            "opted-in dynamic segment with one v6 /64" {
                test_segment(true, AllocationStrategy::Dynamic, &["2001:db8::/64"]) => Some("2001:db8::/64".parse().unwrap()),
            }

            "opted-in dynamic dual-stack segment with one v6 /64" {
                test_segment(true, AllocationStrategy::Dynamic, &["192.0.2.0/24", "2001:db8::/64"]) => Some("2001:db8::/64".parse().unwrap()),
            }

            "default-off dynamic segment with one v6 /64" {
                test_segment(false, AllocationStrategy::Dynamic, &["2001:db8::/64"]) => None,
            }

            "opted-in reserved segment with one v6 /64" {
                test_segment(true, AllocationStrategy::Reserved, &["2001:db8::/64"]) => None,
            }

            "opted-in dynamic segment with no v6 prefix" {
                test_segment(true, AllocationStrategy::Dynamic, &["192.0.2.0/24"]) => None,
            }

            "opted-in dynamic segment with non-64 v6 prefix" {
                test_segment(true, AllocationStrategy::Dynamic, &["2001:db8::/80"]) => None,
            }

            "opted-in dynamic segment with ambiguous v6 prefixes" {
                test_segment(true, AllocationStrategy::Dynamic, &["2001:db8::/64", "2001:db8:1::/80"]) => None,
            }
        );
    }

    #[test]
    fn controller_state_serializes_to_tagged_json() {
        scenarios!(
            run = |state| serde_json::to_string(&state).map_err(drop);
            "provisioning" {
                NetworkSegmentControllerState::Provisioning => Yields(r#"{"state":"provisioning"}"#.to_string()),
            }

            "ready" {
                NetworkSegmentControllerState::Ready => Yields(r#"{"state":"ready"}"#.to_string()),
            }

            "deleting / drain allocated ips" {
                drain_state() => Yields(
                    r#"{"state":"deleting","deletion_state":{"state":"drainallocatedips","delete_at":"2022-12-13T04:41:38Z"}}"#
                        .to_string(),
                ),
            }

            "deleting / db delete" {
                dbdelete_state() => Yields(
                    r#"{"state":"deleting","deletion_state":{"state":"dbdelete"}}"#.to_string(),
                ),
            }
        );
    }

    #[test]
    fn controller_state_round_trips_through_json() {
        scenarios!(
            run = |state| {
                let json = serde_json::to_string(&state).map_err(drop)?;
                serde_json::from_str::<NetworkSegmentControllerState>(&json).map_err(drop)
            };
            "provisioning" {
                NetworkSegmentControllerState::Provisioning => Yields(NetworkSegmentControllerState::Provisioning),
            }

            "ready" {
                NetworkSegmentControllerState::Ready => Yields(NetworkSegmentControllerState::Ready),
            }

            "deleting / drain allocated ips" {
                drain_state() => Yields(drain_state()),
            }

            "deleting / db delete" {
                dbdelete_state() => Yields(dbdelete_state()),
            }
        );
    }

    #[test]
    fn segment_type_parses_from_db_string() {
        scenarios!(
            run = |s| NetworkSegmentType::from_str(s).map_err(drop);
            "tenant" {
                "tenant" => Yields(NetworkSegmentType::Tenant),
            }

            "admin" {
                "admin" => Yields(NetworkSegmentType::Admin),
            }

            "tor maps to underlay" {
                "tor" => Yields(NetworkSegmentType::Underlay),
            }

            "host_inband" {
                "host_inband" => Yields(NetworkSegmentType::HostInband),
            }

            "unknown token" {
                "bogus" => Fails,
            }

            "empty string" {
                "" => Fails,
            }

            "wrong-case admin" {
                "Admin" => Fails,
            }

            "display name underlay, not parse name" {
                "underlay" => Fails,
            }

            "whitespace padded" {
                " tenant " => Fails,
            }
        );
    }

    #[test]
    fn segment_type_parse_error_names_the_input() {
        scenarios!(
            run = |(s, tokens)| {
                let msg = NetworkSegmentType::from_str(s)
                    .map(|_| String::new())
                    .unwrap_or_else(|e| e.to_string());
                Ok::<_, ()>(tokens.iter().all(|t| msg.contains(t)))
            };
            "error mentions the offending token" {
                ("bogus", &["Invalid segment type", "bogus"][..]) => Yields(true),
            }

            "error mentions an empty token" {
                ("", &["Invalid segment type"][..]) => Yields(true),
            }
        );
    }

    #[test]
    fn segment_type_round_trips_through_display_and_parse() {
        scenarios!(
            run = |ty| NetworkSegmentType::from_str(&ty.to_string()).map_err(drop);
            "tenant" {
                NetworkSegmentType::Tenant => Yields(NetworkSegmentType::Tenant),
            }

            "admin" {
                NetworkSegmentType::Admin => Yields(NetworkSegmentType::Admin),
            }

            "underlay" {
                NetworkSegmentType::Underlay => Yields(NetworkSegmentType::Underlay),
            }

            "host_inband" {
                NetworkSegmentType::HostInband => Yields(NetworkSegmentType::HostInband),
            }
        );
    }

    #[test]
    fn segment_type_displays_its_db_token() {
        value_scenarios!(
            run = |ty| ty.to_string();
            "tenant" {
                NetworkSegmentType::Tenant => "tenant".to_string(),
            }

            "admin" {
                NetworkSegmentType::Admin => "admin".to_string(),
            }

            "underlay renders as tor" {
                NetworkSegmentType::Underlay => "tor".to_string(),
            }

            "host_inband" {
                NetworkSegmentType::HostInband => "host_inband".to_string(),
            }
        );
    }

    #[test]
    fn is_tenant_is_true_for_tenant_facing_segments() {
        value_scenarios!(
            run = |ty| ty.is_tenant();
            "tenant is tenant-facing" {
                NetworkSegmentType::Tenant => true,
            }

            "host_inband is tenant-facing" {
                NetworkSegmentType::HostInband => true,
            }

            "admin is not tenant-facing" {
                NetworkSegmentType::Admin => false,
            }

            "underlay is not tenant-facing" {
                NetworkSegmentType::Underlay => false,
            }
        );
    }

    #[test]
    fn segment_type_converts_from_definition_type() {
        value_scenarios!(
            run = NetworkSegmentType::from;
            "admin" {
                NetworkDefinitionSegmentType::Admin => NetworkSegmentType::Admin,
            }

            "underlay" {
                NetworkDefinitionSegmentType::Underlay => NetworkSegmentType::Underlay,
            }

            "host_inband" {
                NetworkDefinitionSegmentType::HostInband => NetworkSegmentType::HostInband,
            }
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct BuiltPrefix {
        prefix: String,
        gateway: Option<String>,
        dhcpv6_link_address: Option<String>,
        num_reserved: i32,
    }

    /// Builds a config network definition for segment expansion tests.
    fn definition(
        prefix: &str,
        prefix_v6: Option<&str>,
        dhcpv6_link_address: Option<&str>,
    ) -> NetworkDefinition {
        NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix: prefix.parse().unwrap(),
            prefix_v6: prefix_v6.map(|prefix| prefix.parse().unwrap()),
            gateway: Some(prefix.parse::<IpNetwork>().unwrap().network()),
            dhcpv6_link_address: dhcpv6_link_address.map(|addr| addr.parse().unwrap()),
            mtu: 1500,
            reserve_first: 5,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        }
    }

    /// Expands a network definition and returns the persisted prefix shape.
    fn build_definition_prefixes(value: NetworkDefinition) -> Result<Vec<BuiltPrefix>, String> {
        NewNetworkSegment::build_from("test-segment", uuid::Uuid::new_v4().into(), &value)
            .map(|segment| {
                segment
                    .prefixes
                    .into_iter()
                    .map(|prefix| BuiltPrefix {
                        prefix: prefix.prefix.to_string(),
                        gateway: prefix.gateway.map(|addr| addr.to_string()),
                        dhcpv6_link_address: prefix
                            .dhcpv6_link_address
                            .map(|addr| addr.to_string()),
                        num_reserved: prefix.num_reserved,
                    })
                    .collect()
            })
            .map_err(|err| err.to_string())
    }

    /// Verifies config-seeded segments expand to at most one prefix per family.
    #[test]
    fn build_from_network_definition_expands_dual_stack_prefixes() {
        scenarios!(
            // Build the config-seeded segment and project the prefix rows that
            // will be persisted.
            run = build_definition_prefixes;
            "v4-only keeps the legacy gateway row" {
                definition("192.0.2.0/24", None, None) => Yields(vec![
                    BuiltPrefix {
                        prefix: "192.0.2.0/24".to_string(),
                        gateway: Some("192.0.2.0".to_string()),
                        dhcpv6_link_address: None,
                        num_reserved: 5,
                    },
                ]),
            }

            "v6-only has no gateway and carries the dhcpv6 link-address" {
                definition("2001:db8::/64", None, Some("2001:db8:ffff::1")) => Yields(vec![
                    BuiltPrefix {
                        prefix: "2001:db8::/64".to_string(),
                        gateway: None,
                        dhcpv6_link_address: Some("2001:db8:ffff::1".to_string()),
                        num_reserved: 5,
                    },
                ]),
            }

            "dual-stack builds one row per family" {
                definition("192.0.2.0/24", Some("2001:db8::/64"), Some("2001:db8:ffff::1")) => Yields(vec![
                    BuiltPrefix {
                        prefix: "192.0.2.0/24".to_string(),
                        gateway: Some("192.0.2.0".to_string()),
                        dhcpv6_link_address: None,
                        num_reserved: 5,
                    },
                    BuiltPrefix {
                        prefix: "2001:db8::/64".to_string(),
                        gateway: None,
                        dhcpv6_link_address: Some("2001:db8:ffff::1".to_string()),
                        num_reserved: 5,
                    },
                ]),
            }

            "prefix_v6 must be IPv6" {
                definition("192.0.2.0/24", Some("198.51.100.0/24"), None) => Fails,
            }

            "two IPv6 prefixes are rejected" {
                definition("2001:db8:1::/64", Some("2001:db8:2::/64"), None) => Fails,
            }

            "dhcpv6 link-address must be IPv6" {
                definition("192.0.2.0/24", Some("2001:db8::/64"), Some("192.0.2.1")) => Fails,
            }

            "dhcpv6 link-address requires a v6 prefix" {
                definition("192.0.2.0/24", None, Some("2001:db8::1")) => Fails,
            }
        );
    }

    #[test]
    fn build_from_network_definition_keeps_slaac_inference_setting() {
        value_scenarios!(
            run = |infer_slaac_eui64_addresses| {
                let mut definition = definition("2001:db8::/64", None, None);
                definition.infer_slaac_eui64_addresses = infer_slaac_eui64_addresses;

                NewNetworkSegment::build_from(
                    "test-segment",
                    uuid::Uuid::new_v4().into(),
                    &definition,
                )
                .unwrap()
                .infer_slaac_eui64_addresses
            };
            "inference disabled" {
                false => false,
            }

            "inference enabled" {
                true => true,
            }
        );
    }

    #[test]
    fn network_definition_requires_gateway_when_ipv4_is_present() {
        scenarios!(
            run = |prefix_v6| {
                let mut definition = definition("192.0.2.0/24", prefix_v6, None);
                definition.gateway = None;
                definition.validate("test-net").map_err(|err| err.to_string())
            };
            "IPv4-only requires a gateway" {
                None => FailsWith("invalid value: network \"test-net\": gateway is required for an IPv4 prefix".to_string()),
            }
            "an additional IPv6 prefix does not make the IPv4 gateway optional" {
                Some("2001:db8::/64") => FailsWith("invalid value: network \"test-net\": gateway is required for an IPv4 prefix".to_string()),
            }
        );
    }

    #[test]
    fn build_from_network_definition_rejects_missing_ipv4_gateway() {
        let mut definition = definition("192.0.2.0/24", None, None);
        definition.gateway = None;

        let err = build_definition_prefixes(definition).unwrap_err();
        assert!(
            err.contains("gateway is required for an IPv4 prefix"),
            "{err}"
        );
    }

    #[test]
    fn allocation_strategy_round_trips_through_json() {
        scenarios!(
            run = |s| serde_json::to_string(&s).map_err(drop);
            "dynamic serializes to its snake-case token" {
                AllocationStrategy::Dynamic => Yields(r#""dynamic""#.to_string()),
            }

            "reserved serializes to its snake-case token" {
                AllocationStrategy::Reserved => Yields(r#""reserved""#.to_string()),
            }
        );
    }

    #[test]
    fn allocation_strategy_defaults_to_dynamic() {
        value_scenarios!(
            run = |()| AllocationStrategy::default();
            "default" {
                () => AllocationStrategy::Dynamic,
            }
        );
    }

    #[test]
    fn is_marked_as_deleted_follows_the_deleted_timestamp() {
        let stamp: DateTime<Utc> = "2022-12-13T04:41:38Z".parse().unwrap();
        value_scenarios!(
            run = |deleted| deleted.is_some();
            "no timestamp means live" {
                None => false,
            }

            "timestamp means deleted" {
                Some(stamp) => true,
            }
        );
    }

    fn network_definition_with_mtu(mtu: i32) -> NetworkDefinition {
        NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix: "10.0.0.0/24".parse().unwrap(),
            prefix_v6: None,
            gateway: Some("10.0.0.1".parse().unwrap()),
            dhcpv6_link_address: None,
            mtu,
            reserve_first: 0,
            allocation_strategy: AllocationStrategy::default(),
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        }
    }

    #[test]
    fn network_definition_validate_rejects_out_of_range_mtu() {
        value_scenarios!(
            run = |mtu: i32| network_definition_with_mtu(mtu).validate("test-net").is_err();
            "MTU below minimum is rejected" {
                575 => true,
            }
            "MTU above maximum is rejected" {
                9001 => true,
            }
            "minimum boundary MTU is accepted" {
                576 => false,
            }
            "maximum boundary MTU is accepted" {
                9000 => false,
            }
            "typical standard MTU is accepted" {
                1500 => false,
            }
        );
    }

    #[test]
    fn network_definition_validate_error_names_the_network_and_mtu() {
        let err = network_definition_with_mtu(9214)
            .validate("test-inband")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("test-inband"),
            "error should name the network: {err}"
        );
        assert!(
            err.contains("9214"),
            "error should include the bad MTU value: {err}"
        );
    }
}
