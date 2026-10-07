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
use std::path::PathBuf;

use eyre::Context;
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::pretty_cmd;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IpLink {
    pub ifindex: u32,
    pub ifname: Option<String>,
    pub flags: Vec<String>,
    pub mtu: u32,
    pub qdisc: String,
    pub operstate: String,
    pub linkmode: Option<String>,
    pub group: String,
    pub txqlen: Option<u32>,
    pub link_type: Option<String>,
    pub address: String,
    pub broadcast: String,
    pub vinfo_list: Option<Vec<String>>,
}

impl IpLink {
    pub async fn get_link_by_name(interface: &str) -> eyre::Result<Option<IpLink>> {
        let data = Self::ip_links().await?;
        tracing::trace!(ip_link_data = ?data, "interfaces data from ip show");
        let data = serde_json::from_str::<Vec<IpLink>>(&data).map_err(|err| eyre::eyre!(err));
        data.map(|i| {
            i.into_iter()
                .find(|x| x.ifname == Some(interface.to_string()))
        })
    }
    async fn ip_links() -> eyre::Result<String> {
        if cfg!(test) || std::env::var("NO_DPU_ARMOS_INTERFACE").is_ok() {
            let test_data_dir = PathBuf::from(crate::dpu::ARMOS_TEST_DATA_DIR);

            std::fs::read_to_string(test_data_dir.join("iplink.json")).map_err(|e| {
                error!(error = %e, "Could not read iplink.json");
                eyre::eyre!("could not read iplink.json: {}", e)
            })
        } else {
            let mut cmd = tokio::process::Command::new("bash");
            cmd.args(vec!["-c", "ip -j link show"]);
            cmd.kill_on_drop(true);

            let cmd_str = pretty_cmd(cmd.as_std());

            let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
                .await
                .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

            let fout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(fout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IpLink;
    use crate::dpu::interface::IpInterface;

    const IP_LINK_OUT: &str = r#"[{"ifindex":256,"ifname":"pf0vf51","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":9216,"qdisc":"mq","operstate":"UP","linkmode":"DEFAULT","group":"default","txqlen":128,"link_type":"ether","address":"26:97:d8:57:c8:31","broadcast":"ff:ff:ff:ff:ff:ff","addr_info":[]}]"#;

    #[test]
    fn test_parse_ifindex_above_u8_max() -> eyre::Result<()> {
        let links: Vec<IpLink> = serde_json::from_str(IP_LINK_OUT)?;
        assert_eq!(links[0].ifindex, 256);

        let interfaces: Vec<IpInterface> = serde_json::from_str(IP_LINK_OUT)?;
        assert_eq!(interfaces[0].link.ifindex, 256);
        Ok(())
    }
}
