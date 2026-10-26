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
use std::sync::Arc;

use rpc::forge::forge_server::Forge;
use rpc::forge::{
    NamedRoutingProfileConfig, PrefixFilterPolicyEntries, PrefixFilterPolicyEntry,
    RoutingProfileSearchFilter, RoutingProfilesByNamesRequest,
};
use tonic::{Code, Request};

use crate::cfg::file::{AdminFnnConfig, FnnConfig, FnnRoutingProfileConfig};
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::create_test_env;

#[crate::sqlx_test]
async fn routing_profiles_read_loaded_config(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    let mut config = env.api.runtime_config.as_ref().clone();
    config.max_find_by_ids = 3;
    // Parsing the actual configuration format also proves that explicit empty
    // collections and unset fields survive the path from TOML to the RPC.
    config.fnn = Some(
        toml::from_str::<FnnConfig>(
            r#"
        [routing_profiles."site/custom"]
        [routing_profiles.INTERNAL]
        internal = true
        access_tier = 1
        tenant_prefix_overlap_eligible = true
        leak_default_route_from_underlay = true
        leak_tenant_host_routes_to_underlay = true
        tenant_leak_communities_accepted = true
        route_target_imports = [{ asn = 64512, vni = 4000 }, { asn = 64512, vni_self = true }]
        route_targets_on_exports = [{ asn = 64513, vni = 5000 }, { asn = 64513, vni_self = true }]
        accepted_leaks_from_underlay = [{ prefix = "192.0.2.0/24" }]
        allowed_anycast_prefixes = [{ prefix = "2001:db8::/64" }]
        [routing_profiles.EXTERNAL]
        internal = false
        access_tier = 0
        leak_default_route_from_underlay = false
        leak_tenant_host_routes_to_underlay = false
        tenant_leak_communities_accepted = false
        route_target_imports = []
        route_targets_on_exports = []
        accepted_leaks_from_underlay = []
        allowed_anycast_prefixes = []
    "#,
        )
        .unwrap(),
    );
    let api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(Arc::new(config))
    .build();

    for (scenario, filter, expected) in [
        (
            "all sorted",
            RoutingProfileSearchFilter::default(),
            vec!["EXTERNAL", "INTERNAL", "site/custom"],
        ),
        (
            "internal",
            RoutingProfileSearchFilter {
                internal: Some(true),
                ..Default::default()
            },
            vec!["INTERNAL"],
        ),
        (
            "external includes default",
            RoutingProfileSearchFilter {
                internal: Some(false),
                ..Default::default()
            },
            vec!["EXTERNAL", "site/custom"],
        ),
        (
            "zero minimum includes all tiers",
            RoutingProfileSearchFilter {
                minimum_access_tier: Some(0),
                ..Default::default()
            },
            vec!["EXTERNAL", "INTERNAL", "site/custom"],
        ),
        (
            "nonzero tier",
            RoutingProfileSearchFilter {
                minimum_access_tier: Some(1),
                ..Default::default()
            },
            vec!["INTERNAL"],
        ),
        (
            "matching conjunction",
            RoutingProfileSearchFilter {
                internal: Some(true),
                minimum_access_tier: Some(1),
            },
            vec!["INTERNAL"],
        ),
        (
            "conflicting conjunction",
            RoutingProfileSearchFilter {
                internal: Some(false),
                minimum_access_tier: Some(1),
            },
            vec![],
        ),
        (
            "tier mismatch",
            RoutingProfileSearchFilter {
                minimum_access_tier: Some(2),
                internal: None,
            },
            vec![],
        ),
    ] {
        let result = api
            .find_routing_profile_names(Request::new(filter))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(result.names, expected, "{scenario}");
    }

    let profiles = api
        .find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest {
            names: vec!["site/custom".into(), "INTERNAL".into(), "EXTERNAL".into()],
        }))
        .await
        .unwrap()
        .into_inner()
        .profiles;
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["EXTERNAL", "INTERNAL", "site/custom"]
    );
    assert_eq!(
        profiles[0].config,
        Some(NamedRoutingProfileConfig {
            internal: Some(false),
            access_tier: Some(0),
            tenant_prefix_overlap_eligible: false,
            leak_default_route_from_underlay: Some(false),
            leak_tenant_host_routes_to_underlay: Some(false),
            tenant_leak_communities_accepted: Some(false),
            route_target_imports: Some(rpc::common::RouteTargets { values: vec![] }),
            route_targets_on_exports: Some(rpc::common::RouteTargets { values: vec![] }),
            accepted_leaks_from_underlay: Some(PrefixFilterPolicyEntries { values: vec![] }),
            allowed_anycast_prefixes: Some(PrefixFilterPolicyEntries { values: vec![] }),
        })
    );
    assert_eq!(
        profiles[1].config,
        Some(NamedRoutingProfileConfig {
            internal: Some(true),
            access_tier: Some(1),
            tenant_prefix_overlap_eligible: true,
            leak_default_route_from_underlay: Some(true),
            leak_tenant_host_routes_to_underlay: Some(true),
            tenant_leak_communities_accepted: Some(true),
            route_target_imports: Some(rpc::common::RouteTargets {
                values: vec![
                    rpc::common::RouteTarget {
                        asn: 64512,
                        vni: Some(4000),
                        vni_self: false,
                    },
                    rpc::common::RouteTarget {
                        asn: 64512,
                        vni: None,
                        vni_self: true,
                    },
                ]
            }),
            route_targets_on_exports: Some(rpc::common::RouteTargets {
                values: vec![
                    rpc::common::RouteTarget {
                        asn: 64513,
                        vni: Some(5000),
                        vni_self: false,
                    },
                    rpc::common::RouteTarget {
                        asn: 64513,
                        vni: None,
                        vni_self: true,
                    },
                ]
            }),
            accepted_leaks_from_underlay: Some(PrefixFilterPolicyEntries {
                values: vec![PrefixFilterPolicyEntry {
                    prefix: "192.0.2.0/24".into()
                }]
            }),
            allowed_anycast_prefixes: Some(PrefixFilterPolicyEntries {
                values: vec![PrefixFilterPolicyEntry {
                    prefix: "2001:db8::/64".into()
                }]
            }),
        })
    );
    assert_eq!(
        profiles[2].config,
        Some(NamedRoutingProfileConfig::default())
    );

    let names = api
        .find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest {
            names: vec!["INTERNAL".into(), "internal".into(), "INTERNAL".into()],
        }))
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .map(|profile| profile.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["INTERNAL"]);
    for names in [vec![], vec!["INTERNAL".to_string(); 4]] {
        let error = api
            .find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest { names }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
    }
}

#[crate::sqlx_test]
async fn routing_profiles_unicode_names_do_not_panic(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    let mut config = env.api.runtime_config.as_ref().clone();
    let mut names = (0..35)
        .map(|index| format!("{}-{index}", "é".repeat(18)))
        .collect::<Vec<_>>();
    config.max_find_by_ids = names.len() as u32;
    let profile_config = names
        .iter()
        .map(|name| format!("[routing_profiles.\"{name}\"]\n"))
        .collect::<String>();
    config.fnn = Some(toml::from_str::<FnnConfig>(&profile_config).unwrap());
    let api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(Arc::new(config))
    .build();

    let request = RoutingProfilesByNamesRequest {
        names: names.clone(),
    };
    // The original logger truncated this valid request in the middle of a
    // UTF-8 character at its 1500-byte limit, panicking before lookup.
    let request_debug = format!("{request:?}");
    assert!(request_debug.len() > 1500);
    assert!(!request_debug.is_char_boundary(1500));
    let profiles = api
        .find_routing_profiles_by_names(Request::new(request))
        .await
        .unwrap()
        .into_inner()
        .profiles;
    names.sort();
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect::<Vec<_>>(),
        names
    );
    assert!(
        profiles
            .iter()
            .all(|profile| profile.config == Some(NamedRoutingProfileConfig::default()))
    );

    // Oversized Unicode batches must reach normal request validation too.
    names.push(names[0].clone());
    let error = api
        .find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest { names }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(
        error
            .message()
            .contains("no more than 35 routing profile names")
    );
}

#[crate::sqlx_test]
async fn routing_profiles_absent_or_empty_config(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    for fnn in [
        None,
        Some(FnnConfig {
            // The inline admin profile is not a named inventory entry.
            admin_vpc: Some(AdminFnnConfig {
                enabled: true,
                vpc_vni: Some(9000),
                routing_profile: FnnRoutingProfileConfig::default(),
            }),
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::<String, FnnRoutingProfileConfig>::new(),
            use_vpc_vrf_loopback: false,
        }),
    ] {
        let mut config = env.api.runtime_config.as_ref().clone();
        config.fnn = fnn;
        let api = TestApiBuilder::new(
            pool.clone(),
            env.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_runtime_config(Arc::new(config))
        .build();
        assert!(
            api.find_routing_profile_names(Request::new(RoutingProfileSearchFilter::default()))
                .await
                .unwrap()
                .into_inner()
                .names
                .is_empty()
        );
        assert!(
            api.find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest {
                names: vec!["INTERNAL".into()]
            }))
            .await
            .unwrap()
            .into_inner()
            .profiles
            .is_empty()
        );
        assert_eq!(
            api.find_routing_profiles_by_names(Request::new(RoutingProfilesByNamesRequest {
                names: vec![]
            }))
            .await
            .unwrap_err()
            .code(),
            Code::InvalidArgument
        );
    }
}
