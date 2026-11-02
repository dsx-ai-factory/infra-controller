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
use std::cmp::Ordering;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use eyre::Context;
use forge_dpu_agent_utils::FMDS_IPV6_GATEWAY;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};

use crate::dpu::{Action, run_ip_command, use_networking_fixture};

pub(crate) type DpuRoutePlan = HashMap<Action, Vec<IpRoute>>;

const FMDS_HBN_IPV4: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 253));
const FMDS_HBN_IPV6: IpAddr = IpAddr::V6(FMDS_IPV6_GATEWAY);

/// `IpRoute` describes an FMDS reply route after kernel destinations have been
/// parsed, or a desired route to install through the metadata peer.
#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq, Hash)]
pub struct IpRoute {
    pub dst: IpNetwork,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev: Option<String>,
    pub protocol: Option<String>,
    pub gateway: Option<IpAddr>,
    pub scope: Option<String>,
    pub prefsrc: Option<IpAddr>,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Route {
    /// Represents the current configuration of routes
    pub current: Vec<IpRoute>,
    /// Represents the plan which describes the actions needed to
    /// reconcile the current configuration with the desired configuration
    pub desired: DpuRoutePlan,
}

impl Ord for IpRoute {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dst.cmp(&other.dst)
    }
}

impl PartialOrd for IpRoute {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl IpRoute {
    /// Compare the route fields we install, ignoring metadata reported by the kernel.
    fn matches_route(&self, other: &Self, interface: &str) -> bool {
        self.dst.network() == other.dst.network()
            && self.dst.prefix() == other.dst.prefix()
            && self.dev.as_deref().unwrap_or(interface) == other.dev.as_deref().unwrap_or(interface)
            && self.gateway == other.gateway
            && self.prefsrc == other.prefsrc
    }

    /// Build a family-specific route command without interpreting interface names as shell text.
    fn command(&self, action: Action) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("ip");
        command.args([
            if self.dst.is_ipv6() { "-6" } else { "-4" },
            "route",
            match action {
                Action::Add => "add",
                Action::Remove => "del",
            },
        ]);
        command.arg(format!("{}/{}", self.dst.network(), self.dst.prefix()));
        if let Some(device) = &self.dev {
            command.args(["dev", device]);
        }
        if let Some(gateway) = self.gateway {
            command.arg("via").arg(gateway.to_string());
        }
        if let Some(source) = self.prefsrc {
            command.arg("src").arg(source.to_string());
        }
        command
    }
}

/// Keep the kernel's `default` destination as text until the route's family and ownership are known.
#[derive(Deserialize)]
struct ObservedRoute {
    dst: String,
    dev: Option<String>,
    protocol: Option<String>,
    gateway: Option<IpAddr>,
    scope: Option<String>,
    prefsrc: Option<IpAddr>,
    #[serde(default)]
    flags: Vec<String>,
}

impl Route {
    /// Plan FMDS reply routes on `interface`, preserving routes outside the metadata path.
    pub async fn plan(interface: &str, desired: Vec<IpRoute>) -> eyre::Result<DpuRoutePlan> {
        let current = Self::current_routes(interface).await?;
        Ok(Self::plan_from_routes(interface, &current, desired))
    }

    /// Plan additions and removals from observed routes without querying the
    /// kernel, so tests can check reconciliation against explicit snapshots.
    fn plan_from_routes(
        interface: &str,
        current: &[IpRoute],
        desired: Vec<IpRoute>,
    ) -> DpuRoutePlan {
        let mut plan = HashMap::new();

        for route in &desired {
            if !current
                .iter()
                .any(|current| current.matches_route(route, interface))
            {
                let entry = plan.entry(Action::Add).or_insert_with(Vec::new);
                // Several tenant interfaces can share a prefix. Install it once.
                if !entry
                    .iter()
                    .any(|added: &IpRoute| added.matches_route(route, interface))
                {
                    entry.push(route.clone());
                }
            }
        }

        for route in current {
            if !desired
                .iter()
                .any(|desired| route.matches_route(desired, interface))
            {
                let entry = plan.entry(Action::Remove).or_insert_with(Vec::new);
                entry.push(route.clone());
            }
        }

        tracing::trace!(route_plan = ?plan, "Route plan");
        plan
    }

    /// Apply removals before additions so a changed source or gateway cannot collide with its old route.
    /// Any failed command aborts the apply; the caller must reobserve routes before retrying.
    pub async fn apply(mut plan: DpuRoutePlan) -> eyre::Result<()> {
        for action in [Action::Remove, Action::Add] {
            if let Some(routes) = plan.remove(&action) {
                for route in routes {
                    tracing::info!(?route, %action, "Updating FMDS reply route");
                    run_ip_command(route.command(action)).await?;
                }
            }
        }
        Ok(())
    }

    /// Read FMDS routes through the metadata peers in both families, including after an agent restart.
    /// Kernel, dynamic, direct, and routes through other gateways remain untouched.
    pub async fn current_routes(interface: &str) -> eyre::Result<Vec<IpRoute>> {
        if use_networking_fixture("NO_DPU_ARMOS_NETWORK") {
            let path = PathBuf::from(crate::dpu::ARMOS_TEST_DATA_DIR).join("iproute.json");
            let data = std::fs::read_to_string(&path)
                .wrap_err_with(|| format!("could not read route fixture {}", path.display()))?;
            return Self::parse_fmds_routes(interface, &data);
        }
        let mut routes = Vec::new();
        for family in ["-4", "-6"] {
            let mut command = tokio::process::Command::new("ip");
            command.args(["-j", family, "route", "show", "dev", interface]);
            let data = run_ip_command(command).await?;
            routes.extend(Self::parse_fmds_routes(interface, &data)?);
        }
        Ok(routes)
    }

    /// Select FMDS routes before parsing destinations so unrelated default routes remain untouched.
    fn parse_fmds_routes(interface: &str, data: &str) -> eyre::Result<Vec<IpRoute>> {
        let routes: Vec<ObservedRoute> = serde_json::from_str(data)
            .wrap_err_with(|| format!("could not parse routes on interface {interface}"))?;
        routes
            .into_iter()
            .filter(|route| {
                // We install routes without a dedicated protocol tag. Their metadata
                // peer and explicit source identify them; an identical manual route
                // is indistinguishable.
                route
                    .dev
                    .as_deref()
                    .is_none_or(|device| device == interface)
                    && matches!(route.gateway, Some(FMDS_HBN_IPV4 | FMDS_HBN_IPV6))
                    && route.prefsrc.is_some()
                    && matches!(route.protocol.as_deref(), None | Some("boot" | "static"))
            })
            .map(|route| {
                let destination = match route.dst.as_str() {
                    "default" if route.gateway == Some(FMDS_HBN_IPV6) => "::/0",
                    "default" => "0.0.0.0/0",
                    destination => destination,
                };
                Ok(IpRoute {
                    dst: destination.parse().wrap_err_with(|| {
                        format!("invalid FMDS destination {destination} on interface {interface}")
                    })?,
                    // `ip route show dev` may omit `dev`; deletion must still name the interface.
                    dev: Some(interface.to_string()),
                    protocol: route.protocol,
                    gateway: route.gateway,
                    scope: route.scope,
                    prefsrc: route.prefsrc,
                    flags: route.flags,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::HBNDeviceNames;

    #[tokio::test]
    async fn test_current_routes() {
        let routes = Route::current_routes(HBNDeviceNames::hbn_23().sfs[0])
            .await
            .unwrap();
        let match_route = IpNetwork::from_str("10.217.4.168/29").unwrap();
        assert_eq!(routes[0].dst, match_route);
    }

    #[tokio::test]
    async fn test_add_route_plan() {
        let new_proposed_route1 = IpRoute {
            dst: IpNetwork::from_str("192.168.100.0/24").unwrap(),
            dev: Some(HBNDeviceNames::hbn_23().sfs[0].to_string()),
            protocol: None,
            scope: Some("link".to_string()),
            gateway: None,
            prefsrc: Some(IpAddr::from([192, 168, 100, 22])),
            flags: vec![],
        };
        let existing_route1 = IpRoute {
            dst: IpNetwork::from_str("10.217.4.168/29").unwrap(),
            dev: None,
            protocol: None,
            scope: None,
            gateway: Some(IpAddr::from([169, 254, 169, 253])),
            prefsrc: Some(IpAddr::from([169, 254, 169, 254])),
            flags: vec![],
        };
        let plan = Route::plan(
            HBNDeviceNames::hbn_23().sfs[0],
            vec![new_proposed_route1, existing_route1],
        )
        .await
        .unwrap();

        let remove = plan.get(&Action::Remove);
        let add = plan.get(&Action::Add).unwrap();

        assert!(add.len() == 1);
        assert!(remove.is_none());
        assert!(add[0].dst == IpNetwork::from_str("192.168.100.0/24").unwrap());
    }

    #[tokio::test]
    async fn test_noop_plan() {
        let new_proposed_route1 = IpRoute {
            dst: IpNetwork::from_str("10.217.4.168/29").unwrap(),
            dev: None,
            protocol: None,
            gateway: Some(IpAddr::from([169, 254, 169, 253])),
            scope: None,
            prefsrc: Some(IpAddr::from([169, 254, 169, 254])),
            flags: vec![],
        };
        let plan = Route::plan(
            HBNDeviceNames::hbn_23().sfs[0],
            vec![new_proposed_route1.clone()],
        )
        .await
        .unwrap();
        let remove = plan.get(&Action::Remove);
        let add = plan.get(&Action::Add);

        assert!(add.is_none());
        assert!(remove.is_none());
    }
    #[tokio::test]
    async fn test_full_plan() {
        let new_proposed_route1 = IpRoute {
            dst: IpNetwork::from_str("10.44.55.0/24").unwrap(),
            dev: Some(HBNDeviceNames::hbn_23().sfs[0].to_string()),
            protocol: None,
            scope: Some("link".to_string()),
            gateway: None,
            prefsrc: Some(IpAddr::from([192, 168, 0, 1])),
            flags: vec![],
        };

        // This route exists in the test json fixture data
        let route_to_remove = IpRoute {
            dst: IpNetwork::from_str("10.217.4.168/29").unwrap(),
            dev: Some(HBNDeviceNames::hbn_23().sfs[0].to_string()),
            protocol: None,
            scope: None,
            gateway: Some(IpAddr::from([169, 254, 169, 253])),
            prefsrc: Some(IpAddr::from([169, 254, 169, 254])),
            flags: vec![],
        };
        let new_proposed_route2 = IpRoute {
            dst: IpNetwork::from_str("192.168.200.0/24").unwrap(),
            dev: None,
            protocol: None,
            scope: None,
            gateway: None,
            prefsrc: Some(IpAddr::from([192, 168, 200, 1])),
            flags: vec![],
        };

        let plan = Route::plan(
            HBNDeviceNames::hbn_23().sfs[0],
            vec![new_proposed_route1.clone(), new_proposed_route2.clone()],
        )
        .await
        .unwrap();

        let remove = plan.get(&Action::Remove).unwrap();
        let add = plan.get(&Action::Add).unwrap();

        assert_eq!(remove.len(), 1);
        assert_eq!(add.len(), 2);

        assert_eq!(add[0], new_proposed_route1);
        assert_eq!(remove[0], route_to_remove);

        tracing::trace!(route_plan = ?plan, "Full Plan");
    }

    /// Adding and retiring IPv6 reply routes must preserve IPv4 and routes installed by other owners.
    #[test]
    fn ipv6_route_reload_preserves_other_routes() {
        let interface = HBNDeviceNames::hbn_23().sfs[0];
        let ipv4 = IpRoute {
            dst: "10.217.4.168/29".parse().expect("IPv4 tenant prefix"),
            dev: None,
            protocol: None,
            gateway: Some(FMDS_HBN_IPV4),
            scope: None,
            // Explicitly configured source addresses must remain recognizable after restart.
            prefsrc: Some("192.0.2.254".parse().expect("configured IPv4 source")),
            flags: vec![],
        };
        let ipv6 = IpRoute {
            dst: "2001:db8:1::/64".parse().expect("IPv6 tenant prefix"),
            dev: Some(interface.to_string()),
            protocol: None,
            gateway: Some(FMDS_HBN_IPV6),
            scope: None,
            prefsrc: Some("fd00:76b9::254".parse().expect("IPv6 metadata source")),
            flags: vec![],
        };
        let mut observed = vec![
            // `ip` can report a device and protocol that were omitted from the desired route.
            serde_json::json!({
                "dst": "10.217.4.168/29", "dev": interface, "protocol": "boot",
                "gateway": "169.254.169.253", "prefsrc": "192.0.2.254", "flags": []
            }),
            // A dynamic default is not ours even if it uses the same metadata peer and source.
            serde_json::json!({
                "dst": "default", "dev": interface, "protocol": "ra",
                "gateway": "fd00:76b9::255", "prefsrc": "fd00:76b9::254", "flags": []
            }),
            // Connected routes must never enter the removal plan.
            serde_json::json!({
                "dst": "fd00:76b9::254/127", "dev": interface, "protocol": "kernel",
                "prefsrc": "fd00:76b9::254", "scope": "link", "flags": []
            }),
            // A static route's source alone does not establish FMDS ownership.
            serde_json::json!({
                "dst": "2001:db8:2::/64", "dev": interface, "protocol": "static",
                "gateway": "fd00:76b9::1", "prefsrc": "fd00:76b9::254", "flags": []
            }),
        ];

        let current = Route::parse_fmds_routes(
            interface,
            &serde_json::to_string(&observed).expect("initial route observation"),
        )
        .expect("parse initial routes");
        assert_eq!(current.len(), 1);
        // Repeated tenant prefixes must not result in a second, failing `ip route add`.
        assert_eq!(
            Route::plan_from_routes(
                interface,
                &current,
                vec![ipv4.clone(), ipv6.clone(), ipv6.clone()],
            ),
            HashMap::from([(Action::Add, vec![ipv6.clone()])])
        );
        let command = ipv6.command(Action::Add);
        assert_eq!(
            command
                .as_std()
                .get_args()
                .map(|argument| argument.to_str().expect("UTF-8 ip argument"))
                .collect::<Vec<_>>(),
            [
                "-6",
                "route",
                "add",
                "2001:db8:1::/64",
                "dev",
                interface,
                "via",
                "fd00:76b9::255",
                "src",
                "fd00:76b9::254",
            ]
        );

        // Reobserve the installed IPv6 route with kernel metadata, as an agent restart does.
        observed.push(serde_json::json!({
            "dst": "2001:db8:1::/64", "dev": interface, "protocol": "static",
            "gateway": "fd00:76b9::255", "prefsrc": "fd00:76b9::254",
            "scope": "global", "metric": 1024, "flags": []
        }));
        let current = Route::parse_fmds_routes(
            interface,
            &serde_json::to_string(&observed).expect("installed route observation"),
        )
        .expect("parse installed routes");
        assert_eq!(current.len(), 2);
        assert!(Route::plan_from_routes(interface, &current, vec![ipv4.clone(), ipv6]).is_empty());

        // Removing one tenant family must not repeat the entire observed route list as removals.
        assert_eq!(
            Route::plan_from_routes(interface, &current, vec![ipv4]),
            HashMap::from([(Action::Remove, vec![current[1].clone()])])
        );
    }

    /// An owned default route must retain its address family when `ip` renders
    /// the destination as `default`, so restart reconciliation can compare it.
    #[test]
    fn owned_default_routes_preserve_address_family() {
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases};

        let cases = [
            // IPv4 defaults use the same metadata peer as explicit IPv4 prefixes.
            Case {
                scenario: "IPv4 default",
                input: serde_json::json!([{
                    "dst": "default", "gateway": "169.254.169.253",
                    "prefsrc": "169.254.169.254", "protocol": "boot"
                }]),
                expect: Yields(vec![
                    "0.0.0.0/0".parse::<IpNetwork>().expect("IPv4 default"),
                ]),
            },
            // IPv6 defaults must not fall through to the IPv4 /0 conversion.
            Case {
                scenario: "IPv6 default",
                input: serde_json::json!([{
                    "dst": "default", "gateway": "fd00:76b9::255",
                    "prefsrc": "fd00:76b9::254", "protocol": "static"
                }]),
                expect: Yields(vec!["::/0".parse::<IpNetwork>().expect("IPv6 default")]),
            },
        ];
        check_cases(cases, |observed| {
            Route::parse_fmds_routes("fmds0", &observed.to_string())
                .map(|routes| {
                    routes
                        .into_iter()
                        .map(|route| route.dst)
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        });
    }
}
