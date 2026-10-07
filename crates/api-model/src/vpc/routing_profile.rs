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

use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};

/// A BGP route target used in FNN VRF import/export policies.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteTargetConfig {
    /// Autonomous System Number component of the route target.
    #[serde(default)]
    pub asn: u32,

    /// Virtual Network Identifier component of the route target.
    #[serde(default)]
    pub vni: u32,
}

/// An entry used by a DPU prefix-list policy.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrefixFilterPolicyEntry {
    /// Prefix matched by the policy.
    pub prefix: IpNetwork,
}

/// Routing-profile values set directly on a VPC.
///
/// Each present value overrides the corresponding property from the VPC's
/// named routing profile. `tenant_prefix_overlap_eligible`, `internal`, and
/// `access_tier` are intentionally absent because VPCs cannot override the base
/// profile's overlap admission, allocation, or access controls.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VpcRoutingProfileOverrides {
    pub route_target_imports: Option<Vec<RouteTargetConfig>>,
    pub route_targets_on_exports: Option<Vec<RouteTargetConfig>>,
    pub leak_default_route_from_underlay: Option<bool>,
    pub leak_tenant_host_routes_to_underlay: Option<bool>,
    pub tenant_leak_communities_accepted: Option<bool>,
    pub accepted_leaks_from_underlay: Option<Vec<PrefixFilterPolicyEntry>>,
    pub allowed_anycast_prefixes: Option<Vec<PrefixFilterPolicyEntry>>,
}
