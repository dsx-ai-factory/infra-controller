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

use std::net::Ipv4Addr;
use std::time::Duration;

use eyre::eyre;
use futures_util::TryStreamExt;
use ipnetwork::IpNetwork;
use rtnetlink::packet_route::address::AddressAttribute;
use rtnetlink::{Handle, LinkUnspec};
use tokio::process::Command as TokioCommand;

const LINK_LOOKUP_RETRIES: u32 = 15;
const LINK_LOOKUP_BACKOFF: Duration = Duration::from_secs(2);

/// Assign `cidr` to the interface named `name` and bring it up. Idempotent:
/// if the address is already present, the add is skipped. Retries while the
/// interface is not yet attached (Multus may attach the NIC slightly after
/// the pod starts).
pub async fn assign_address(name: &str, cidr: IpNetwork) -> eyre::Result<()> {
    let (connection, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(connection);

    let index = wait_for_link(&handle, name).await?;

    if address_already_present(&handle, index, cidr).await? {
        tracing::info!(interface_name = name, %cidr, "address already assigned; skipping add");
    } else {
        handle
            .address()
            .add(index, cidr.ip(), cidr.prefix())
            .execute()
            .await?;
        tracing::info!(interface_name = name, %cidr, "assigned address");
    }

    handle
        .link()
        .set(LinkUnspec::new_with_index(index).up().build())
        .execute()
        .await?;
    Ok(())
}

/// Set up policy routing so replies from 169.254.169.254 egress via the metadata interface.
/// Computes the gateway as the first host in the /30 (e.g. 169.254.169.253 for 169.254.169.254/30).
pub async fn setup_metadata_routing(interface_name: &str, cidr: IpNetwork) -> eyre::Result<()> {
    let IpNetwork::V4(net) = cidr else {
        return Ok(());
    };

    let src_ip = net.ip();
    let gateway = Ipv4Addr::from(u32::from(net.network()) + 1);

    let rule = TokioCommand::new("ip")
        .args(["rule", "add", "from", &src_ip.to_string(), "lookup", "100"])
        .output()
        .await?;
    if !rule.status.success() {
        let stderr = String::from_utf8_lossy(&rule.stderr);
        if !stderr.contains("File exists") {
            eyre::bail!("ip rule add failed: {stderr}");
        }
    }
    tracing::info!(source_ip_address = %src_ip, table = 100, "policy rule configured");

    let route = TokioCommand::new("ip")
        .args([
            "route",
            "add",
            "default",
            "via",
            &gateway.to_string(),
            "dev",
            interface_name,
            "table",
            "100",
        ])
        .output()
        .await?;
    if !route.status.success() {
        let stderr = String::from_utf8_lossy(&route.stderr);
        if !stderr.contains("File exists") {
            eyre::bail!("ip route add failed: {stderr}");
        }
    }
    tracing::info!(
        gateway = %gateway,
        interface_name,
        table = 100,
        "default policy route configured",
    );

    Ok(())
}

async fn wait_for_link(handle: &Handle, name: &str) -> eyre::Result<u32> {
    for attempt in 1..=LINK_LOOKUP_RETRIES {
        let mut links = handle.link().get().match_name(name.to_string()).execute();
        match links.try_next().await {
            Ok(Some(link)) => return Ok(link.header.index),
            Ok(None) | Err(rtnetlink::Error::NetlinkError(_)) => {
                tracing::debug!(
                    interface_name = name,
                    attempt,
                    "interface not yet present, retrying"
                );
            }
            Err(e) => return Err(e.into()),
        }
        tokio::time::sleep(LINK_LOOKUP_BACKOFF).await;
    }
    Err(eyre!(
        "interface {name} not found after {LINK_LOOKUP_RETRIES} attempts"
    ))
}

async fn address_already_present(
    handle: &Handle,
    index: u32,
    cidr: IpNetwork,
) -> eyre::Result<bool> {
    let mut addrs = handle
        .address()
        .get()
        .set_link_index_filter(index)
        .execute();
    while let Some(msg) = addrs.try_next().await? {
        let prefix_len = msg.header.prefix_len;
        for attr in &msg.attributes {
            if let AddressAttribute::Address(ip) = attr
                && *ip == cidr.ip()
                && prefix_len == cidr.prefix()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
