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

use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_request_data_redacted};
use crate::cfg::file::FnnRoutingProfileConfig;

pub(crate) fn find_names(
    api: &Api,
    request: Request<rpc::RoutingProfileSearchFilter>,
) -> Result<Response<rpc::RoutingProfileNameList>, Status> {
    log_request_data(&request);
    let filter = request.into_inner();
    let mut names = api
        .runtime_config
        .fnn
        .as_ref()
        .map_or_else(Vec::new, |fnn| {
            fnn.routing_profiles
                .iter()
                .filter(|(_, profile)| {
                    filter
                        .internal
                        .is_none_or(|expected| profile.internal.unwrap_or_default() == expected)
                        && filter.minimum_access_tier.is_none_or(|expected| {
                            profile.access_tier.unwrap_or_default() >= expected
                        })
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        });
    names.sort();
    Ok(Response::new(rpc::RoutingProfileNameList { names }))
}

pub(crate) fn find_by_names(
    api: &Api,
    request: Request<rpc::RoutingProfilesByNamesRequest>,
) -> Result<Response<rpc::NamedRoutingProfileList>, Status> {
    let mut names = request.into_inner().names;
    // Keep request logs bounded for arbitrary UTF-8 profile names.
    log_request_data_redacted(format!(
        "RoutingProfilesByNamesRequest {{ name_count: {} }}",
        names.len()
    ));
    let max_names = api.runtime_config.max_find_by_ids as usize;
    if names.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one routing profile name must be provided".into(),
        )
        .into());
    }
    if names.len() > max_names {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_names} routing profile names can be accepted"
        ))
        .into());
    }
    names.sort();
    names.dedup();
    let profiles = api
        .runtime_config
        .fnn
        .as_ref()
        .map_or_else(Vec::new, |fnn| {
            names
                .into_iter()
                .filter_map(|name| {
                    fnn.routing_profiles
                        .get(&name)
                        .map(|profile| rpc::NamedRoutingProfile {
                            name,
                            config: Some(profile.into()),
                        })
                })
                .collect()
        });
    Ok(Response::new(rpc::NamedRoutingProfileList { profiles }))
}

impl From<&FnnRoutingProfileConfig> for rpc::NamedRoutingProfileConfig {
    fn from(profile: &FnnRoutingProfileConfig) -> Self {
        Self {
            route_target_imports: profile.route_target_imports.as_ref().map(|targets| {
                ::rpc::common::RouteTargets {
                    values: targets
                        .iter()
                        .map(|target| ::rpc::common::RouteTarget {
                            asn: target.asn,
                            vni: target.vni,
                            vni_self: target.vni_self,
                        })
                        .collect(),
                }
            }),
            route_targets_on_exports: profile.route_targets_on_exports.as_ref().map(|targets| {
                ::rpc::common::RouteTargets {
                    values: targets
                        .iter()
                        .map(|target| ::rpc::common::RouteTarget {
                            asn: target.asn,
                            vni: target.vni,
                            vni_self: target.vni_self,
                        })
                        .collect(),
                }
            }),
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            accepted_leaks_from_underlay: profile.accepted_leaks_from_underlay.as_ref().map(
                |entries| rpc::PrefixFilterPolicyEntries {
                    values: entries
                        .iter()
                        .map(|entry| rpc::PrefixFilterPolicyEntry {
                            prefix: entry.prefix.to_string(),
                        })
                        .collect(),
                },
            ),
            allowed_anycast_prefixes: profile.allowed_anycast_prefixes.as_ref().map(|entries| {
                rpc::PrefixFilterPolicyEntries {
                    values: entries
                        .iter()
                        .map(|entry| rpc::PrefixFilterPolicyEntry {
                            prefix: entry.prefix.to_string(),
                        })
                        .collect(),
                }
            }),
            internal: profile.internal,
            access_tier: profile.access_tier,
            tenant_prefix_overlap_eligible: profile.tenant_prefix_overlap_eligible,
        }
    }
}
