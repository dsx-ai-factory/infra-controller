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
pub mod capability;
pub mod routing_profile;

use std::collections::HashMap;
use std::net::IpAddr;

pub use capability::{
    ALL_VPC_VIRTUALIZATION_TYPES, DataPlaneKind, FabricInterfaceType, VpcCapabilities,
    VpcCapabilityError, VpcVirtualizationTypeCapabilities,
};
use carbide_network::ip::IpAddressFamily;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use carbide_uuid::vpc::VpcId;
use carbide_uuid::vpc_peering::VpcPeeringId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
pub use routing_profile::{PrefixFilterPolicyEntry, RouteTargetConfig, VpcRoutingProfileOverrides};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::metadata::{LabelFilter, Metadata};

/// Returns the prefix length allocated to one instance interface.
///
/// IPv4 always uses a point-to-point `/31`. IPv6 uses a `/64` when SLAAC is
/// enabled and a point-to-point `/127` otherwise.
pub const fn instance_prefix_len(address_family: IpAddressFamily, slaac_enabled: bool) -> u8 {
    match (address_family, slaac_enabled) {
        (IpAddressFamily::Ipv4, _) => 31,
        (IpAddressFamily::Ipv6, true) => 64,
        (IpAddressFamily::Ipv6, false) => 127,
    }
}

/// Returns whether a VPC prefix can contain an instance interface prefix.
///
/// Interface prefixes must be narrower than their parent. An IPv4 `/31` is
/// also allowed to back one interface directly for compatibility with the
/// existing point-to-point allocation model. IPv6 keeps the strict parent
/// rule, so an exact `/64` or `/127` parent has no allocatable capacity.
pub const fn vpc_prefix_can_allocate_interface_prefix(
    address_family: IpAddressFamily,
    parent_prefix_len: u8,
    interface_prefix_len: u8,
) -> bool {
    parent_prefix_len < interface_prefix_len
        || matches!(
            (address_family, parent_prefix_len, interface_prefix_len),
            (IpAddressFamily::Ipv4, 31, 31)
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpcConfig {
    pub tenant_organization_id: String,
    pub tenant_keyset_id: Option<String>,
    pub network_virtualization_type: VpcVirtualizationType,
    pub network_security_group_id: Option<NetworkSecurityGroupId>,
    pub default_nvlink_logical_partition_id: Option<NvLinkLogicalPartitionId>,
    pub vni: Option<i32>,
    pub routing_profile_type: Option<String>,
    pub routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
    pub power_resource_group: Option<String>,
    /// Whether the VPC uses SLAAC allocation mode for instance IPv6 interfaces.
    pub slaac_enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct VpcStatus {
    /// Allocated VNI.
    pub vni: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vpc {
    pub id: VpcId,
    pub version: ConfigVersion,
    pub config: VpcConfig,
    pub status: VpcStatus,
    pub metadata: Metadata,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VpcDefinition {
    pub organization_id: Option<String>,
    pub network_virtualization_type: VpcVirtualizationType,
    pub routing_profile_type: Option<String>,
    pub routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
    pub vni: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct VpcSearchFilter {
    pub name: Option<String>,
    pub tenant_org_id: Option<String>,
    pub label: Option<LabelFilter>,
}

#[derive(Clone, Debug)]
pub struct NewVpc {
    pub id: VpcId,
    pub tenant_organization_id: String,
    pub network_virtualization_type: VpcVirtualizationType,
    pub metadata: Metadata,
    pub network_security_group_id: Option<NetworkSecurityGroupId>,
    pub routing_profile_type: Option<String>,
    pub routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
    pub power_resource_group: Option<String>,
    pub vni: Option<i32>,
    /// Whether the VPC uses SLAAC allocation mode for instance IPv6 interfaces.
    pub slaac_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct UpdateVpc {
    pub id: VpcId,
    pub network_security_group_id: Option<NetworkSecurityGroupId>,
    pub routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
    pub power_resource_group: Option<PowerResourceGroupUpdate>,
    pub if_version_match: Option<ConfigVersion>,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerResourceGroupUpdate {
    Set(String),
    Clear,
}

/// UpdateVpcVirtualization exists as a mechanism to translate
/// an incoming VpcUpdateVirtualizationRequest and turn it
/// into something we can `update()` to the database.
#[derive(Clone, Debug)]
pub struct UpdateVpcVirtualization {
    pub id: VpcId,
    pub if_version_match: Option<ConfigVersion>,
    pub network_virtualization_type: VpcVirtualizationType,
}

impl<'r> sqlx::FromRow<'r, PgRow> for Vpc {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let vpc_labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;

        let metadata = Metadata {
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            labels: vpc_labels.0,
        };

        let status: sqlx::types::Json<VpcStatus> = row.try_get("status")?;

        Ok(Vpc {
            id: row.try_get("id")?,
            version: row.try_get("version")?,
            config: VpcConfig {
                tenant_organization_id: row.try_get("organization_id")?,
                tenant_keyset_id: None, // TODO: fix this once DB gets updated
                network_security_group_id: row.try_get("network_security_group_id")?,
                network_virtualization_type: row.try_get("network_virtualization_type")?,
                routing_profile_type: row.try_get("routing_profile_type")?,
                routing_profile_overrides: row
                    .try_get::<Option<sqlx::types::Json<VpcRoutingProfileOverrides>>, _>(
                        "routing_profile_overrides",
                    )?
                    .map(|profile| profile.0),
                power_resource_group: row.try_get("power_resource_group")?,
                slaac_enabled: row.try_get("slaac_enabled")?,
                vni: row.try_get("vni")?,
                default_nvlink_logical_partition_id: None,
            },
            status: status.0,
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            deleted: row.try_get("deleted")?,
            metadata,
        })
    }
}

#[derive(Clone, Debug, FromRow)]
pub struct VpcDpuLoopback {
    pub dpu_id: DpuMachineId,
    pub vpc_id: VpcId,
    pub loopback_ip: IpAddr,
}

impl VpcDpuLoopback {
    pub fn new(dpu_id: DpuMachineId, vpc_id: VpcId, loopback_ip: IpAddr) -> Self {
        Self {
            dpu_id,
            vpc_id,
            loopback_ip,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VpcPeering {
    pub id: VpcPeeringId,
    pub vpc_id: VpcId,
    pub peer_vpc_id: VpcId,
}

impl<'r> FromRow<'r, PgRow> for VpcPeering {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(VpcPeering {
            id: row.try_get("id")?,
            vpc_id: row.try_get("vpc1_id")?,
            peer_vpc_id: row.try_get("vpc2_id")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_network::ip::IpAddressFamily;
    use carbide_test_support::value_scenarios;

    use super::{instance_prefix_len, vpc_prefix_can_allocate_interface_prefix};

    #[test]
    fn instance_prefix_length_follows_address_family_and_slaac_policy() {
        value_scenarios!(
            run = |(address_family, slaac_enabled)| {
                instance_prefix_len(address_family, slaac_enabled)
            };
            "stateful allocation" {
                (IpAddressFamily::Ipv4, false) => 31,
                (IpAddressFamily::Ipv6, false) => 127,
            }

            "SLAAC allocation" {
                (IpAddressFamily::Ipv4, true) => 31,
                (IpAddressFamily::Ipv6, true) => 64,
            }
        );
    }

    #[test]
    fn vpc_prefix_eligibility_preserves_the_ipv4_31_exception() {
        value_scenarios!(
            run = |(address_family, parent_prefix_len, interface_prefix_len)| {
                vpc_prefix_can_allocate_interface_prefix(
                    address_family,
                    parent_prefix_len,
                    interface_prefix_len,
                )
            };
            "eligible parents" {
                (IpAddressFamily::Ipv4, 30, 31) => true,
                (IpAddressFamily::Ipv4, 31, 31) => true,
                (IpAddressFamily::Ipv6, 63, 64) => true,
                (IpAddressFamily::Ipv6, 126, 127) => true,
            }

            "ineligible parents" {
                (IpAddressFamily::Ipv4, 24, 24) => false,
                (IpAddressFamily::Ipv4, 32, 31) => false,
                (IpAddressFamily::Ipv6, 64, 64) => false,
                (IpAddressFamily::Ipv6, 127, 127) => false,
            }
        );
    }
}
