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

use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    AllocationStrategy, NetworkSegment, NetworkSegmentControllerState, NetworkSegmentSearchConfig,
    NetworkSegmentSearchFilter, NetworkSegmentType, NewNetworkSegment, state_sla,
};

use crate as rpc;
use crate::TenantState;
use crate::errors::RpcDataConversionError;
use crate::model::{RpcTryFrom, RpcTryInto};

impl From<rpc::forge::NetworkSegmentSearchFilter> for NetworkSegmentSearchFilter {
    fn from(filter: rpc::forge::NetworkSegmentSearchFilter) -> Self {
        NetworkSegmentSearchFilter {
            name: filter.name,
            tenant_org_id: filter.tenant_org_id,
        }
    }
}

const DEFAULT_MTU_TENANT: i32 = 9000;
const DEFAULT_MTU_OTHER: i32 = 1500;

impl From<rpc::forge::NetworkSegmentSearchConfig> for NetworkSegmentSearchConfig {
    fn from(value: rpc::forge::NetworkSegmentSearchConfig) -> Self {
        NetworkSegmentSearchConfig {
            include_history: value.include_history,
            include_num_free_ips: value.include_num_free_ips,
        }
    }
}

impl RpcTryFrom<i32> for NetworkSegmentType {
    type Error = RpcDataConversionError;
    fn rpc_try_from(value: i32) -> Result<Self, Self::Error> {
        Ok(match value {
            x if x == rpc::forge::NetworkSegmentType::Tenant as i32 => NetworkSegmentType::Tenant,
            x if x == rpc::forge::NetworkSegmentType::Admin as i32 => NetworkSegmentType::Admin,
            x if x == rpc::forge::NetworkSegmentType::Underlay as i32 => {
                NetworkSegmentType::Underlay
            }
            x if x == rpc::forge::NetworkSegmentType::HostInband as i32 => {
                NetworkSegmentType::HostInband
            }
            _ => {
                return Err(RpcDataConversionError::InvalidNetworkSegmentType(value));
            }
        })
    }
}

/// Converts from Protobuf NetworkSegmentCreationRequest into NewNetworkSegment
///
/// subdomain_id - Converting from Protobuf UUID(String) to Rust UUID type can fail.
/// Use try_from in order to return a Result where Result is an error if the conversion
/// from String -> UUID fails
impl TryFrom<rpc::forge::NetworkSegmentCreationRequest> for NewNetworkSegment {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::NetworkSegmentCreationRequest) -> Result<Self, Self::Error> {
        if value.prefixes.is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "Prefixes are empty.".to_string(),
            ));
        }

        let prefixes = value
            .prefixes
            .into_iter()
            .map(NewNetworkPrefix::try_from)
            .collect::<Result<Vec<NewNetworkPrefix>, RpcDataConversionError>>()?;

        let ipv4_prefix_count = prefixes
            .iter()
            .filter(|prefix| prefix.prefix.is_ipv4())
            .count();
        let ipv6_prefix_count = prefixes
            .iter()
            .filter(|prefix| prefix.prefix.is_ipv6())
            .count();
        if ipv4_prefix_count > 1 || ipv6_prefix_count > 1 {
            return Err(RpcDataConversionError::InvalidArgument(
                "Network segment cannot contain more than one prefix from the same address family."
                    .to_string(),
            ));
        }

        let id = value.id.unwrap_or_else(|| uuid::Uuid::new_v4().into());

        let segment_type: NetworkSegmentType = value.segment_type.rpc_try_into()?;
        if segment_type == NetworkSegmentType::Tenant
            && prefixes.iter().any(|ip| match ip.prefix {
                ipnetwork::IpNetwork::V4(v4) => v4.prefix() >= 31,
                ipnetwork::IpNetwork::V6(v6) => v6.prefix() >= 127,
            })
        {
            return Err(RpcDataConversionError::InvalidArgument(
                "IPv4 prefix /31 and /32 (or IPv6 /127 and /128) are not allowed for tenant segments.".to_string(),
            ));
        }

        // This TryFrom implementation is part of the API handler logic for
        // network segment creation, and is not used by FNN. Therefore, the only
        // type of tenant segment we could be creating is a stretchable one.
        let can_stretch = matches!(segment_type, NetworkSegmentType::Tenant).then_some(true);

        Ok(NewNetworkSegment {
            id,
            name: value.name,
            subdomain_id: value.subdomain_id,
            vpc_id: value.vpc_id,
            mtu: value.mtu.unwrap_or(match segment_type {
                NetworkSegmentType::Tenant => DEFAULT_MTU_TENANT,
                _ => DEFAULT_MTU_OTHER,
            }),
            prefixes,
            vlan_id: None,
            vni: None,
            segment_type,
            can_stretch,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: value.infer_slaac_eui64_addresses,
        })
    }
}

///
/// Marshal a Data Object (NetworkSegment) into an RPC NetworkSegment
///
/// subdomain_id - Rust UUID -> ProtoBuf UUID(String) cannot fail, so convert it or return None
impl From<NetworkSegment> for rpc::NetworkSegment {
    fn from(src: NetworkSegment) -> Self {
        // Coarse tenant-facing state derived from the full controller state.
        // Note that even though the segment might already be ready,
        // we only return `Ready` after the state machine also noticed that.
        // Otherwise we would need to allow address allocation before the
        // controller state is ready, which spreads out the state mismatch.
        let tenant_state = match &src.status.controller_state.value {
            NetworkSegmentControllerState::Provisioning => TenantState::Provisioning,
            NetworkSegmentControllerState::Ready => TenantState::Ready,
            NetworkSegmentControllerState::Deleting { .. } => TenantState::Terminating,
        };
        // If deletion is requested, immediately overwrite to terminating.
        // The state controller will eventually catch up.
        let tenant_state = if src.is_marked_as_deleted() {
            TenantState::Terminating
        } else {
            tenant_state
        };

        // lifecycle.state: full JSON serialization of the internal controller state.
        // Consistent with how Switch and PowerShelf populate LifecycleStatus.
        let lifecycle_state =
            serde_json::to_string(&src.status.controller_state.value).unwrap_or_default();

        let sla: rpc::forge::StateSla = state_sla(
            &src.status.controller_state.value,
            &src.status.controller_state.version,
        )
        .into();

        let state_reason: Option<rpc::forge::ControllerStateReason> =
            src.status.controller_state_outcome.map(Into::into);

        let flags: Vec<i32> = {
            use crate::forge::NetworkSegmentFlag::*;

            let mut flags = vec![];

            let can_stretch = src.status.can_stretch.unwrap_or_else(|| {
                // If the segment's can_stretch flag is NULL in the database,
                // we're going to have to go off of what an FNN-created
                // segment's prefixes would look like, and then assume any such
                // FNN segment is _not_ stretchable.
                src.prefixes.iter().all(|p| !p.smells_like_fnn())
            });

            if can_stretch {
                flags.push(CanStretch);
            }

            // Just so a gRPC client can tell the difference between a missing
            // `flags` field and an empty one.
            if flags.is_empty() {
                flags.push(NoOp);
            }

            flags.into_iter().map(|flag| flag as i32).collect()
        };

        let prefixes: Vec<rpc::forge::NetworkPrefix> =
            src.prefixes.into_iter().map(Into::into).collect();

        let version = src.version.version_string();

        rpc::NetworkSegment {
            id: Some(src.id),
            created: Some(src.created.into()),
            updated: Some(src.updated.into()),
            deleted: src.deleted.map(|t| t.into()),

            // Note: prefixes are placed under config in the proto even though they are top-level
            // in the Rust model. The Rust model keeps them top-level because each NetworkPrefix
            // contains mixed config fields (CIDR, gateway) and status fields (free_ip_count,
            // svi_ip). The proto puts them under config as the closest semantic fit for now.
            config: Some(rpc::forge::NetworkSegmentConfig {
                vpc_id: src.config.vpc_id,
                subdomain_id: src.config.subdomain_id,
                mtu: Some(src.config.mtu),
                prefixes,
                segment_type: src.config.segment_type as i32,
                infer_slaac_eui64_addresses: src.config.infer_slaac_eui64_addresses,
            }),
            status: Some(rpc::forge::NetworkSegmentStatus {
                flags,
                lifecycle: Some(rpc::forge::LifecycleStatus {
                    state: lifecycle_state,
                    version,
                    state_reason,
                    sla: Some(sla),
                }),
                tenant_state: tenant_state as i32,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: src.config.name,
                description: String::new(),
                labels: vec![],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    fn make_test_creation_request(
        prefixes: Vec<rpc::forge::NetworkPrefix>,
        segment_type: NetworkSegmentType,
    ) -> rpc::forge::NetworkSegmentCreationRequest {
        rpc::forge::NetworkSegmentCreationRequest {
            id: None,
            mtu: Some(1500),
            name: "TEST_SEGMENT".to_string(),
            prefixes,
            subdomain_id: None,
            vpc_id: None,
            segment_type: match segment_type {
                NetworkSegmentType::Admin => rpc::forge::NetworkSegmentType::Admin as i32,
                NetworkSegmentType::Tenant => rpc::forge::NetworkSegmentType::Tenant as i32,
                NetworkSegmentType::Underlay => rpc::forge::NetworkSegmentType::Underlay as i32,
                NetworkSegmentType::HostInband => rpc::forge::NetworkSegmentType::HostInband as i32,
            },
            infer_slaac_eui64_addresses: false,
        }
    }

    fn ipv4_prefix(prefix: &str, gateway: Option<&str>) -> rpc::forge::NetworkPrefix {
        rpc::forge::NetworkPrefix {
            id: None,
            prefix: prefix.to_string(),
            gateway: gateway.map(|g| g.to_string()),
            reserve_first: 1,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }
    }

    fn ipv6_prefix(prefix: &str) -> rpc::forge::NetworkPrefix {
        rpc::forge::NetworkPrefix {
            id: None,
            prefix: prefix.to_string(),
            gateway: None,
            reserve_first: 0,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }
    }

    // Every row drives the same conversion (NewNetworkSegment::try_from): IPv6 and
    // dual-stack prefixes are accepted (and the resulting prefix count / IPv6-ness
    // preserved), while tenant segments reject too-small IPv4 (/31, /32) and IPv6
    // (/127, /128) prefixes. Accepting rows project to (prefix count, first prefix
    // is IPv6); rejecting rows assert only that the conversion fails.
    #[test]
    fn try_from_creation_request_validates_prefixes() {
        scenarios!(
            // The error type (RpcDataConversionError) is not asserted by these rows,
            // so failing rows discard it; accepting rows project to the prefix count
            // and whether the first prefix is IPv6.
            run = |request| {
                NewNetworkSegment::try_from(request)
                    .map(|segment| (segment.prefixes.len(), segment.prefixes[0].prefix.is_ipv6()))
                    .map_err(drop)
            };
            "ipv6 prefix accepted (admin)" {
                make_test_creation_request(
                    vec![ipv6_prefix("2001:db8::/64")],
                    NetworkSegmentType::Admin,
                ) => Yields((1, true)),
            }

            "dual-stack prefixes accepted (admin)" {
                make_test_creation_request(
                    vec![
                        ipv4_prefix("192.0.2.0/24", Some("192.0.2.1")),
                        ipv6_prefix("2001:db8::/64"),
                    ],
                    NetworkSegmentType::Admin,
                ) => Yields((2, false)),
            }

            "two IPv4 prefixes rejected" {
                make_test_creation_request(
                    vec![
                        ipv4_prefix("192.0.2.0/24", Some("192.0.2.1")),
                        ipv4_prefix("198.51.100.0/24", Some("198.51.100.1")),
                    ],
                    NetworkSegmentType::Admin,
                ) => Fails,
            }

            "two IPv6 prefixes rejected" {
                make_test_creation_request(
                    vec![
                        ipv6_prefix("2001:db8:1::/64"),
                        ipv6_prefix("2001:db8:2::/64"),
                    ],
                    NetworkSegmentType::Admin,
                ) => Fails,
            }

            "tenant /64 IPv6 allowed" {
                make_test_creation_request(
                    vec![ipv6_prefix("2001:db8::/64")],
                    NetworkSegmentType::Tenant,
                ) => Yields((1, true)),
            }

            "tenant /127 IPv6 rejected" {
                make_test_creation_request(
                    vec![ipv6_prefix("2001:db8::1/127")],
                    NetworkSegmentType::Tenant,
                ) => Fails,
            }

            "tenant /128 IPv6 rejected" {
                make_test_creation_request(
                    vec![ipv6_prefix("2001:db8::1/128")],
                    NetworkSegmentType::Tenant,
                ) => Fails,
            }

            "tenant /24 IPv4 allowed" {
                make_test_creation_request(
                    vec![ipv4_prefix("192.0.2.0/24", Some("192.0.2.1"))],
                    NetworkSegmentType::Tenant,
                ) => Yields((1, false)),
            }

            "tenant /31 IPv4 rejected" {
                make_test_creation_request(
                    vec![ipv4_prefix("192.0.2.0/31", Some("192.0.2.1"))],
                    NetworkSegmentType::Tenant,
                ) => Fails,
            }

            "tenant /32 IPv4 rejected" {
                make_test_creation_request(
                    vec![ipv4_prefix("192.0.2.0/32", None)],
                    NetworkSegmentType::Tenant,
                ) => Fails,
            }
        );
    }
}
