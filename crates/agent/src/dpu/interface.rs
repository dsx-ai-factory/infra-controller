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
use std::fmt::Debug;
use std::net::IpAddr;
use std::path::PathBuf;

use carbide_utils::none_if_empty::NoneIfEmpty;
use eyre::WrapErr;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::dpu::link::IpLink;
use crate::dpu::{Action, DpuNetworkInterfaces};
use crate::pretty_cmd;

pub(crate) type DpuNetworkInterfacePlan = HashMap<Action, HashMap<String, Option<Vec<IpNetwork>>>>;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IpInterface {
    #[serde(flatten)]
    pub link: IpLink,
    pub addr_info: Vec<IpInterfaceAddress>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct IpInterfaceAddress {
    pub family: String,
    pub local: IpAddr,
    pub prefixlen: u8,
    pub broadcast: Option<IpAddr>,
    pub scope: String,
    pub label: Option<String>,
    pub valid_life_time: u32,
    pub preferred_life_time: u32,
}

#[derive(Debug, Clone)]
pub struct Interface {
    /// Represents the current configuration of interfaces as described in the forge-dpu-agent configuration file
    /// Interfaces configured outside forge-dpu-agent are not represented here
    pub current: Vec<IpInterfaceAddress>,
    /// Represents the plan, a series of actions that are taken to reconcile the current configuration with the desired configuration
    pub desired: HashMap<String, Vec<IpNetwork>>,
}

impl Interface {
    pub async fn apply(plan: DpuNetworkInterfacePlan) -> eyre::Result<()> {
        for (action, interfaces) in plan {
            for (interface, networks) in interfaces {
                match action {
                    Action::Add => {
                        if let Some(networks) = networks
                            && !networks.is_empty()
                        {
                            tracing::info!(?networks, interface_name = %interface, "Adding addresses");

                            for network in networks {
                                Interface::ip_addrs_add(&interface, network).await?;
                            }
                        }
                    }
                    Action::Remove => {
                        // We deliberately do not allow removing an ip address
                        unimplemented!("Remove Interface {:?} {:?}", interface, networks);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn plan(
        interface: &str,
        proposed: DpuNetworkInterfaces,
    ) -> eyre::Result<DpuNetworkInterfacePlan> {
        let mut interface_plan: DpuNetworkInterfacePlan = HashMap::new();

        let current_addresses = Interface::current_addresses(interface)
            .await?
            .iter()
            .flat_map(|x| &x.addr_info)
            .filter_map(|y| IpNetwork::new(y.local, y.prefixlen).ok())
            .collect::<Vec<_>>();

        let mut proposed_addresses = proposed.desired.clone();
        if let Some(iface) = IpLink::get_link_by_name(interface).await? {
            // if the interface exists
            if let Some(ifname) = &iface.ifname {
                // get all addresses for the interface
                let addr = Interface::get_addresses_for_interface(ifname).await?;
                if !addr.is_empty() && !proposed_addresses.is_empty() {
                    if let Some(common_networks) =
                        Interface::find_common_addresses(&current_addresses, &proposed_addresses)
                    {
                        tracing::trace!(
                            interface_name = %interface,
                            ?common_networks,
                            "Proposed addresses already present"
                        );

                        // If the proposed addresses are already present on the interface
                        // remove them from the list
                        // TODO - this is a bit of a hack, we shouldy probably change this logic to that of
                        // the Route::plan
                        proposed_addresses.retain(|x| !common_networks.contains(x));
                    }
                    tracing::trace!(
                        interface_name = %interface,
                        ?proposed_addresses,
                        "Proposed addresses needing to be added"
                    );
                    let entry = interface_plan.entry(Action::Add).or_default();
                    entry.insert(interface.to_string(), Some(proposed_addresses.clone()));
                }
            }
        } else {
            tracing::error!(
                interface_name = interface,
                "FMDS cannot add IP address to non-existent interface"
            );
        }

        Ok(interface_plan)
    }

    async fn ip_addrs(interface: &str) -> eyre::Result<String> {
        if cfg!(test) || std::env::var("NO_DPU_ARMOS_NETWORK").is_ok() {
            let test_data_dir = PathBuf::from(crate::dpu::ARMOS_TEST_DATA_DIR);

            std::fs::read_to_string(test_data_dir.join("ipaddr.json")).map_err(|e| {
                error!(error = %e, "Could not read ipaddr.json");
                eyre::eyre!("could not read ipaddr.json: {}", e)
            })
        } else {
            let mut cmd = tokio::process::Command::new("bash");
            cmd.args(vec!["-c", &format!("ip -j addr show dev {interface}")]);
            cmd.kill_on_drop(true);

            let cmd_str = pretty_cmd(cmd.as_std());

            let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
                .await
                .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

            let fout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(fout)
        }
    }

    pub async fn current_addresses(interface: &str) -> eyre::Result<Vec<IpInterface>> {
        let data = Self::ip_addrs(interface).await?;
        tracing::trace!(ip_address_data = ?data, "interface data from ip addr show");
        serde_json::from_str::<Vec<IpInterface>>(&data).map_err(|err| eyre::eyre!(err))
    }

    async fn ip_addrs_add(interface: &str, address: IpNetwork) -> eyre::Result<bool> {
        let cmdargs = format!(
            "ip addr add {}/{} dev {}",
            address.ip(),
            address.prefix(),
            interface
        );
        // ip addr add 169.254.169.254/30 dev pf0dpu1_if

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
            tracing::error!(command_output = ?fout, "Failed to add address");
            Ok(false)
        }
    }

    pub async fn get_addresses_for_interface(
        interface: &str,
    ) -> eyre::Result<Vec<IpInterfaceAddress>> {
        let data = Self::ip_addrs(interface).await?;
        tracing::trace!(ip_link_data = ?data, "interfaces data from ip show");
        let data =
            serde_json::from_str::<Vec<IpInterface>>(&data).map_err(|err| eyre::eyre!(err))?;
        let filtered_list: Vec<_> = data
            .into_iter()
            .filter(|iface| iface.link.ifname.as_deref() == Some(interface))
            .flat_map(|iface| iface.addr_info)
            .collect();

        Ok(filtered_list)
    }

    pub fn find_common_addresses(
        current_addresses: &[IpNetwork],
        proposed_addresses: &[IpNetwork],
    ) -> Option<Vec<IpNetwork>> {
        let res = proposed_addresses
            .iter()
            .filter(|x| current_addresses.contains(x))
            .copied()
            .collect::<Vec<_>>();

        res.none_if_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HBNDeviceNames;
    use crate::dpu::DpuNetworkInterfaces;
    use crate::dpu::interface::IpInterface;

    struct TestInterfaceData {
        current: Vec<IpInterface>,
        desired: Vec<IpNetwork>,
    }
    impl TestInterfaceData {
        async fn new(interface: &str) -> Self {
            Self {
                current: Interface::current_addresses(interface).await.unwrap(),
                desired: Vec::new(),
            }
        }

        async fn setup_test_data() -> TestInterfaceData {
            let test_ip = IpNetwork::new(IpAddr::from([192, 168, 0, 5]), 30).unwrap();
            let test_ip2 = IpNetwork::new(IpAddr::from([192, 168, 0, 10]), 30).unwrap();
            let test_ip3 = IpNetwork::new(IpAddr::from([192, 168, 0, 1]), 30).unwrap();
            let mut data = TestInterfaceData::new(HBNDeviceNames::hbn_23().sfs[0]).await;
            data.desired = vec![test_ip, test_ip2, test_ip3];
            data
        }

        async fn to_plan(&self, interface: &str) -> DpuNetworkInterfacePlan {
            Interface::plan(
                interface,
                DpuNetworkInterfaces {
                    desired: self.desired.clone(),
                },
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn common_addresses() {
        let data = TestInterfaceData::setup_test_data().await;
        let current = data.current;

        let current_addresses = current
            .iter()
            .flat_map(|x| &x.addr_info)
            .filter_map(|y| IpNetwork::new(y.local, y.prefixlen).ok())
            .collect::<Vec<_>>();

        let proposed = vec![
            IpNetwork::new(IpAddr::from([192, 168, 0, 1]), 30).unwrap(),
            IpNetwork::new(IpAddr::from([192, 168, 0, 2]), 30).unwrap(),
        ];

        let common = Interface::find_common_addresses(&current_addresses, &proposed).unwrap();

        let expect = IpNetwork::new(IpAddr::from([192, 168, 0, 1]), 30).unwrap();

        assert_eq!(common, vec![expect]);
    }
    #[tokio::test]
    async fn current_interface_addresses() {
        let data = TestInterfaceData::setup_test_data().await;
        let current = data.current;
        assert_eq!(current[0].addr_info[0].local, IpAddr::from([127, 0, 0, 1]));
    }

    #[tokio::test]
    async fn new_ip_addresses_are_add_action() {
        let data = TestInterfaceData::setup_test_data().await;
        let plan = data.to_plan(HBNDeviceNames::hbn_23().sfs[0]).await;

        let add = plan.get(&Action::Add).unwrap();

        let list_of_networks = add
            .get(HBNDeviceNames::hbn_23().sfs[0])
            .unwrap()
            .clone()
            .unwrap();

        assert_eq!(
            list_of_networks,
            vec![
                IpNetwork::new(IpAddr::from([192, 168, 0, 5]), 30).unwrap(),
                IpNetwork::new(IpAddr::from([192, 168, 0, 10]), 30).unwrap()
            ]
        );
    }

    #[tokio::test]
    async fn test_addresses() {
        let interface = Interface::get_addresses_for_interface(HBNDeviceNames::hbn_23().sfs[0])
            .await
            .unwrap();
        tracing::trace!(?interface, "Interface");

        assert_eq!(interface[0].prefixlen, 30);
        assert_eq!(interface[0].local, IpAddr::from([192, 168, 0, 1]));
    }

    #[tokio::test]
    async fn test_link() {
        let link = IpLink::get_link_by_name(HBNDeviceNames::hbn_23().sfs[0])
            .await
            .unwrap()
            .unwrap();
        tracing::trace!(?link, "Link");
        assert_eq!(link.ifindex, 16);
    }

    #[tokio::test]
    async fn plan_includes_ipv6_address() {
        let test_ipv6 = IpNetwork::new("fd00::1".parse().unwrap(), 64).unwrap();
        let mut data = TestInterfaceData::new(HBNDeviceNames::hbn_23().sfs[0]).await;
        data.desired = vec![test_ipv6];
        let plan = data.to_plan(HBNDeviceNames::hbn_23().sfs[0]).await;

        let add = plan.get(&Action::Add).unwrap();
        let networks = add
            .get(HBNDeviceNames::hbn_23().sfs[0])
            .unwrap()
            .clone()
            .unwrap();
        assert!(networks.contains(&test_ipv6));
    }
}
