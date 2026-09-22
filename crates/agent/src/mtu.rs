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

use serde::Deserialize;
use tokio::process::Command as TokioCommand;

const CORRECT_MTU: usize = 9216;

/// Ensures that p0 and p1 have expected MTU.
///
/// HBN sets this but there is a race condition with interfaces being renamed on startup.
/// https://github.com/NVIDIA/infra-controller/issues/5555
pub(super) async fn ensure() -> eyre::Result<()> {
    for iface in ["p0", "p1"] {
        let current = get_mtu(iface).await?;
        if current != CORRECT_MTU {
            tracing::info!(
                interface_name = iface,
                current_mtu = current,
                target_mtu = CORRECT_MTU,
                "Interface has incorrect MTU. Setting target MTU."
            );
            set_mtu(iface, CORRECT_MTU).await?;
        }
    }
    Ok(())
}

async fn get_mtu(iface: &str) -> eyre::Result<usize> {
    let mut cmd = TokioCommand::new("ip");
    let cmd = cmd.args(["-json", "link", "list", iface]);
    let out = cmd.output().await?;
    if out.status.success() {
        let o: Vec<LinkList> = serde_json::from_str(&String::from_utf8_lossy(&out.stdout))?;
        if o.len() != 1 {
            eyre::bail!(
                "expected a single entry, got {}. invalid output from: {}",
                o.len(),
                super::pretty_cmd(cmd.as_std())
            );
        }
        Ok(o[0].mtu)
    } else {
        tracing::debug!(
            command = %super::pretty_cmd(cmd.as_std()),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "MTU query command failed"
        );
        Err(eyre::eyre!(
            "{} for cmd '{}'",
            out.status,
            super::pretty_cmd(cmd.as_std())
        ))
    }
}

async fn set_mtu(iface: &str, mtu: usize) -> eyre::Result<()> {
    let mut cmd = TokioCommand::new("ip");
    let cmd = cmd.args(["link", "set", "dev", iface, "mtu", &mtu.to_string()]);
    let out = cmd.output().await?;
    if out.status.success() {
        Ok(())
    } else {
        tracing::debug!(
            command = %super::pretty_cmd(cmd.as_std()),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "MTU update command failed"
        );
        Err(eyre::eyre!(
            "{} for cmd '{}'",
            out.status,
            super::pretty_cmd(cmd.as_std())
        ))
    }
}

// There are a lot more fields in the output but so far MTU is the only one we
// use. See unit test below for example output.
#[derive(Deserialize, Debug)]
struct LinkList {
    mtu: usize,
}

#[cfg(test)]
mod tests {
    use super::{CORRECT_MTU, LinkList};

    const LINK_LIST_OUT: &str = r#"[{"ifindex":4,"ifname":"p0","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":9216,"qdisc":"mq","master":"ovs-system","operstate":"UP","linkmode":"DEFAULT","group":"default","txqlen":1000,"link_type":"ether","address":"b8:3f:d2:90:97:fa","broadcast":"ff:ff:ff:ff:ff:ff","vfinfo_list":[],"altnames":["enp3s0f0np0"]}]"#;

    #[test]
    fn test_parse_link_list() -> eyre::Result<()> {
        let o: Vec<LinkList> = serde_json::from_str(LINK_LIST_OUT)?;
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].mtu, CORRECT_MTU);
        Ok(())
    }
}
