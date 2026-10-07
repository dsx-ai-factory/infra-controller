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
use std::net::IpAddr;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;

use eyre::Context;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::dpu::Action;
use crate::pretty_cmd;

pub(crate) type DpuRoutePlan = HashMap<Action, Vec<IpRoute>>;

/// IpRoute is a representation of a route in the system when it is deserializes from the output of `ip -j route show`
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

impl Route {
    pub async fn plan(interface: &str, desired: Vec<IpRoute>) -> eyre::Result<DpuRoutePlan> {
        let current = Self::current_routes(interface).await?;

        let mut plan = HashMap::new();

        // Process desired routes
        for route in &desired {
            if !current.contains(route) {
                let entry = plan.entry(Action::Add).or_insert_with(Vec::new);
                entry.push(route.clone());
            }
        }

        // Process routes to remove
        for route in &current {
            if !desired.contains(route) {
                let entry = plan.entry(Action::Remove).or_insert_with(Vec::new);
                entry.extend(current.clone());
            }
        }

        // Combine into plan
        let mut final_plan = HashMap::new();
        for (action, routes) in &plan {
            let entry = final_plan.entry(action).or_insert_with(Vec::new);
            entry.extend(routes);
        }

        tracing::trace!(route_plan = ?plan, "Route plan");
        Ok(plan)
    }

    pub async fn apply(plan: DpuRoutePlan) -> eyre::Result<()> {
        for (action, routes) in plan {
            match action {
                Action::Add => {
                    if !routes.is_empty() {
                        for r in routes {
                            tracing::info!(route = ?r, "Adding route to Dpu");
                            Self::ip_route_add(r.dst, r.dev.as_deref(), r.prefsrc, r.gateway)
                                .await?;
                        }
                    }
                }
                Action::Remove => {
                    if !routes.is_empty() {
                        for r in routes {
                            tracing::info!(route = ?r, "Removing route from Dpu");
                            Self::ip_route_del(r.dst, r.dev.as_deref(), r.prefsrc, r.gateway)
                                .await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn ip_route(interface: &str) -> eyre::Result<String> {
        if cfg!(test) || std::env::var("NO_DPU_ARMOS_NETWORK").is_ok() {
            let test_data_dir = PathBuf::from(crate::dpu::ARMOS_TEST_DATA_DIR);

            std::fs::read_to_string(test_data_dir.join("iproute.json")).map_err(|e| {
                error!(error = %e, "Could not read iproute.json");
                eyre::eyre!("could not read iproute.json: {}", e)
            })
        } else {
            let mut cmd = tokio::process::Command::new("bash");
            cmd.args(vec!["-c", &format!("ip -j route show dev {interface}")]);
            cmd.kill_on_drop(true);

            let cmd_str = pretty_cmd(cmd.as_std());

            let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
                .await
                .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

            let fout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(fout)
        }
    }

    async fn ip_route_add(
        net: IpNetwork,
        device: Option<&str>,
        source_ip: Option<IpAddr>,
        gateway_ip: Option<IpAddr>,
    ) -> eyre::Result<bool> {
        let mut cmdargs = format!("ip route add {}/{}", net.network(), net.prefix());

        if let Some(dev) = device {
            cmdargs.push_str(&format!(" dev {dev}"));
        }

        if let Some(gateway_ip) = gateway_ip {
            cmdargs.push_str(&format!(" via {gateway_ip}"));
        }

        if let Some(source_ip) = source_ip {
            cmdargs.push_str(&format!(" src {source_ip}"));
        }

        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", &cmdargs]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tracing::trace!(command = ?cmd_str, "Running command");

        let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

        let fout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status.success() {
            Ok(true)
        } else {
            tracing::error!(command_output = ?fout, "Failed to add route");
            Ok(false)
        }
    }
    async fn ip_route_del(
        net: IpNetwork,
        device: Option<&str>,
        source_ip: Option<IpAddr>,
        gateway_ip: Option<IpAddr>,
    ) -> eyre::Result<bool> {
        let mut cmdargs = format!("ip route del {}/{}", net.network(), net.prefix(),);

        if let Some(dev) = device {
            cmdargs.push_str(&format!(" dev {dev}"));
        }

        if let Some(gateway_ip) = gateway_ip {
            cmdargs.push_str(&format!(" via {gateway_ip}"));
        }

        if let Some(source_ip) = source_ip {
            cmdargs.push_str(&format!(" src {source_ip}"));
        }

        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", &cmdargs]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tracing::trace!(command = ?cmd_str, "Running command");

        let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

        let fout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status == ExitStatus::from_raw(0) {
            Ok(true)
        } else {
            tracing::error!(command_output = ?fout, "Failed to remove route");
            Ok(false)
        }
    }

    pub async fn current_routes(interface: &str) -> eyre::Result<Vec<IpRoute>> {
        let data = Self::ip_route(interface).await?;
        tracing::trace!(route_data = ?data, "route data from ip route show");
        let mut data =
            serde_json::from_str::<Vec<IpRoute>>(&data).map_err(|err| eyre::eyre!(err))?;

        // Ignore routes provisioned by the kernel
        data.retain(|d| d.protocol != Some("kernel".to_string()));
        Ok(data)
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
        //let current = Route::current_routes().await.unwrap();
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
            dev: None,
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
}
