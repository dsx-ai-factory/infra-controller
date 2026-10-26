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

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use eyre::{WrapErr, eyre};
use forge_dpu_agent_utils::{FMDS_IPV6_ADDRESS, FMDS_IPV6_GATEWAY};
use futures_util::TryStreamExt;
use ipnetwork::IpNetwork;
use rtnetlink::packet_route::AddressFamily;
use rtnetlink::packet_route::address::{AddressAttribute, AddressFlags, AddressMessage};
use rtnetlink::packet_route::rule::{RuleAction, RuleAttribute, RuleMessage};
use rtnetlink::{Handle, IpVersion, LinkUnspec, RouteMessageBuilder};

const LINK_LOOKUP_RETRIES: u32 = 15;
const LINK_LOOKUP_BACKOFF: Duration = Duration::from_secs(2);
const METADATA_ROUTE_TABLE: u32 = 100;
// These bound local kernel setup, not tenant requests. DAD normally completes
// in about one second; a duplicate or persistently tentative address must not
// leave startup reporting a usable IPv6 endpoint.
const ADDRESS_READY_TIMEOUT: Duration = Duration::from_secs(10);
const ROUTING_SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Assign `cidr` to the interface named `name` and bring it up. Idempotent:
/// if the address is already present, the add is skipped. Retries while the
/// interface is not yet attached (Multus may attach the NIC slightly after
/// the pod starts). IPv6 waits for duplicate-address detection within
/// `ADDRESS_READY_TIMEOUT` and fails for a duplicate or tentative address.
pub async fn assign_address(name: &str, cidr: IpNetwork) -> eyre::Result<()> {
    let (connection, handle, _) = rtnetlink::new_connection()?;
    with_netlink_connection(connection, async {
        let index = wait_for_link(&handle, name).await?;

        if find_address(&handle, index, cidr).await?.is_some() {
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

        if cidr.is_ipv6() {
            tokio::time::timeout(ADDRESS_READY_TIMEOUT, async {
                loop {
                    let address = find_address(&handle, index, cidr)
                        .await?
                        .ok_or_else(|| eyre!("metadata address {cidr} disappeared from {name}"))?;
                    if address_ready(&address)
                        .wrap_err_with(|| format!("metadata address {cidr} on {name} is not usable"))?
                    {
                        return Ok::<(), eyre::Report>(());
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .wrap_err_with(|| {
                format!(
                    "metadata address {cidr} on {name} remained tentative for {ADDRESS_READY_TIMEOUT:?}"
                )
            })??;
        }
        Ok(())
    })
    .await
    .wrap_err_with(|| format!("failed to assign metadata address {cidr} on {name}"))
}

/// Route metadata replies through HBN in table 100, with one source rule per
/// address family. IPv4 uses the first host in its configured subnet; IPv6
/// requires the `fd00:76b9::254/127` pair with HBN at `fd00:76b9::255`.
/// Repeated startup preserves an existing rule and replaces the default route
/// so a changed interface does not leave a stale next hop. Kernel setup is
/// bounded by `ROUTING_SETUP_TIMEOUT`; errors stop startup before listeners
/// are advertised. Custom IPv6 assignments do not call this fixed-peer setup.
pub async fn setup_metadata_routing(interface_name: &str, cidr: IpNetwork) -> eyre::Result<()> {
    let gateway = metadata_gateway(cidr)?;
    let (connection, handle, _) = rtnetlink::new_connection()?;
    let configure = async {
        let index = handle
            .link()
            .get()
            .match_name(interface_name.to_owned())
            .execute()
            .try_next()
            .await?
            .ok_or_else(|| eyre!("metadata interface {interface_name} not found"))?
            .header
            .index;
        let route = match gateway {
            IpAddr::V4(gateway) => RouteMessageBuilder::<Ipv4Addr>::new()
                .gateway(gateway)
                .output_interface(index)
                .table_id(METADATA_ROUTE_TABLE)
                .build(),
            IpAddr::V6(gateway) => RouteMessageBuilder::<std::net::Ipv6Addr>::new()
                .gateway(gateway)
                .output_interface(index)
                .table_id(METADATA_ROUTE_TABLE)
                .build(),
        };
        // Install the usable route before directing replies into this table.
        handle.route().add(route).replace().execute().await?;
        let version = if cidr.is_ipv4() {
            IpVersion::V4
        } else {
            IpVersion::V6
        };
        let mut rules = handle.rule().get(version).execute();
        while let Some(rule) = rules.try_next().await? {
            if is_metadata_rule(&rule, cidr.ip()) {
                return Ok::<(), eyre::Report>(());
            }
        }
        let rule = handle
            .rule()
            .add()
            .table_id(METADATA_ROUTE_TABLE)
            .action(RuleAction::ToTable);
        match cidr.ip() {
            IpAddr::V4(source) => rule.v4().source_prefix(source, 32).execute().await?,
            IpAddr::V6(source) => rule.v6().source_prefix(source, 128).execute().await?,
        }
        Ok(())
    };
    tokio::time::timeout(
        ROUTING_SETUP_TIMEOUT,
        with_netlink_connection(connection, configure),
    )
    .await
    .wrap_err_with(|| format!("metadata routing setup exceeded {ROUTING_SETUP_TIMEOUT:?}"))?
    .wrap_err_with(|| {
        format!("failed to configure metadata routing for {cidr} on {interface_name}")
    })?;
    tracing::info!(
        source_ip_address = %cidr.ip(),
        gateway_ip_address = %gateway,
        interface_name,
        table = METADATA_ROUTE_TABLE,
        "metadata policy route configured",
    );
    Ok(())
}

/// Poll the driver alongside its requests and drop it when setup ends. A
/// closed response stream can look successful to rtnetlink, so driver exit
/// takes precedence when both futures become ready in the same poll.
async fn with_netlink_connection(
    connection: impl Future<Output = ()>,
    operation: impl Future<Output = eyre::Result<()>>,
) -> eyre::Result<()> {
    tokio::select! {
        biased;
        () = connection => eyre::bail!("netlink connection stopped during metadata setup"),
        result = operation => result,
    }
}

/// Resolve the peer at the first IPv4 host or the fixed IPv6 HBN address.
fn metadata_gateway(cidr: IpNetwork) -> eyre::Result<IpAddr> {
    match cidr {
        IpNetwork::V4(net) => Ok(Ipv4Addr::from(
            u32::from(net.network())
                .checked_add(1)
                .ok_or_else(|| eyre!("metadata subnet {cidr} has no gateway"))?,
        )
        .into()),
        IpNetwork::V6(net) if net.ip() == FMDS_IPV6_ADDRESS && net.prefix() == 127 => {
            Ok(FMDS_IPV6_GATEWAY.into())
        }
        IpNetwork::V6(_) => Err(eyre!(
            "IPv6 metadata CIDR must be {FMDS_IPV6_ADDRESS}/127, got {cidr}"
        )),
    }
}

/// Match the unrestricted source rule, not a rule that happens to share its source
/// and table but adds a destination, mark, or interface restriction.
fn is_metadata_rule(rule: &RuleMessage, source: IpAddr) -> bool {
    let family = if source.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    };
    let table = rule
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            RuleAttribute::Table(table) => Some(*table),
            _ => None,
        })
        .unwrap_or(u32::from(rule.header.table));
    rule.header.family == family
        && rule.header.src_len == if source.is_ipv4() { 32 } else { 128 }
        && rule.header.dst_len == 0
        && rule.header.tos == 0
        && rule.header.flags.is_empty()
        && rule.header.action == RuleAction::ToTable
        && table == METADATA_ROUTE_TABLE
        && rule.attributes.contains(&RuleAttribute::Source(source))
        && rule.attributes.iter().all(|attribute| match attribute {
            RuleAttribute::Source(address) => *address == source,
            RuleAttribute::Table(table) => *table == METADATA_ROUTE_TABLE,
            // Priority and protocol identify how the rule was installed;
            // the kernel also reports an unset suppression limit as u32::MAX.
            RuleAttribute::Priority(_)
            | RuleAttribute::Protocol(_)
            | RuleAttribute::SuppressPrefixLen(u32::MAX) => true,
            _ => false,
        })
}

/// A tentative IPv6 address is not usable yet, and a duplicate must never be
/// reported as ready merely because it appears in an address dump.
fn address_ready(address: &AddressMessage) -> eyre::Result<bool> {
    let flags = address
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddressAttribute::Flags(flags) => Some(*flags),
            _ => None,
        })
        .unwrap_or_else(|| AddressFlags::from_bits_retain(u32::from(address.header.flags.bits())));
    if flags.contains(AddressFlags::Dadfailed) {
        eyre::bail!("IPv6 metadata address failed duplicate-address detection");
    }
    Ok(!flags.contains(AddressFlags::Tentative))
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

/// Find the exact configured address so a different prefix cannot satisfy
/// startup and IPv6 readiness checks can inspect the kernel's DAD flags.
async fn find_address(
    handle: &Handle,
    index: u32,
    cidr: IpNetwork,
) -> eyre::Result<Option<AddressMessage>> {
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
                return Ok(Some(msg));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    /// Driver termination must not turn a closed reply stream into successful
    /// setup, while normal completion still preserves the operation's result.
    #[tokio::test]
    async fn netlink_driver_exit_is_not_successful_setup() {
        use carbide_test_support::Outcome::FailsWith;
        use carbide_test_support::{Case, check_cases_async};

        #[derive(Clone, Copy)]
        enum Completion {
            Driver,
            Setup,
            KernelError,
        }

        check_cases_async(
            [
                // Simultaneous EOF and apparent success must choose the driver error.
                Case {
                    scenario: "driver exits before setup completes",
                    input: Completion::Driver,
                    expect: FailsWith(
                        "netlink connection stopped during metadata setup".to_owned(),
                    ),
                },
                // A live driver must not prevent completed setup from returning.
                Case {
                    scenario: "setup completes with a live driver",
                    input: Completion::Setup,
                    expect: Yields(()),
                },
                // Ordinary request errors retain their diagnostic rather than looking like EOF.
                Case {
                    scenario: "kernel rejects a request",
                    input: Completion::KernelError,
                    expect: FailsWith("kernel rejected request".to_owned()),
                },
            ],
            |completion| async move {
                let driver = async move {
                    if !matches!(completion, Completion::Driver) {
                        std::future::pending::<()>().await;
                    }
                };
                let operation = async move {
                    if matches!(completion, Completion::KernelError) {
                        eyre::bail!("kernel rejected request");
                    }
                    Ok(())
                };
                with_netlink_connection(driver, operation)
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await;
    }

    /// The IPv6 peer must agree with HBN, while IPv4 subnets use their first
    /// host as the gateway, including custom address overrides.
    #[test]
    fn metadata_gateways_follow_the_address_family() {
        scenarios!(run = |cidr: &str| metadata_gateway(cidr.parse().expect("CIDR")).map_err(drop);
            // The first host is the peer for both default and custom subnets.
            "IPv4 gateway" {
                "169.254.169.254/30" => Yields("169.254.169.253".parse().expect("gateway")),
                "192.0.2.6/30" => Yields("192.0.2.5".parse().expect("gateway")),
            }
            // The adjacent /127 peer is ::255, not the IPv4-style ::253.
            "IPv6 gateway" {
                "fd00:76b9::254/127" => Yields("fd00:76b9::255".parse().expect("gateway")),
            }
            // An unrelated subnet must not be given a guessed HBN peer.
            "unsupported IPv6 subnet" {
                "fd00:76b9::254/64" => Fails,
            }
        );
    }

    /// Only an unrestricted source rule satisfies restart reconciliation;
    /// another policy's mark or narrower destination cannot suppress our add.
    #[test]
    fn metadata_rule_matching_preserves_source_only_semantics() {
        use carbide_test_support::value_scenarios;

        /// Model one kernel rule without contradictory duplicate Table attributes.
        fn rule(source: IpAddr, table: u8, attributes: Vec<RuleAttribute>) -> RuleMessage {
            let mut rule = RuleMessage::default();
            rule.header.family = if source.is_ipv4() {
                AddressFamily::Inet
            } else {
                AddressFamily::Inet6
            };
            rule.header.src_len = if source.is_ipv4() { 32 } else { 128 };
            rule.header.table = table;
            rule.header.action = RuleAction::ToTable;
            rule.attributes.push(RuleAttribute::Source(source));
            rule.attributes.extend(attributes);
            rule
        }

        let ipv4 = IpAddr::from([169, 254, 169, 254]);
        let ipv6 = IpAddr::from(FMDS_IPV6_ADDRESS);
        let mut restricted_destination = rule(
            ipv6,
            100,
            vec![RuleAttribute::Destination(
                "2001:db8::1".parse().expect("destination"),
            )],
        );
        restricted_destination.header.dst_len = 128;
        value_scenarios!(run = |(rule, source): (RuleMessage, IpAddr)| {
            is_metadata_rule(&rule, source)
        };
            // An IPv4 rule can encode the table only in its header.
            "IPv4 header table" { (rule(ipv4, 100, vec![]), ipv4) => true }
            // Kernel-added priority/table/suppression attributes are normal for IPv6 too.
            "IPv6 table attribute" {
                (rule(ipv6, 100, vec![
                    RuleAttribute::Priority(32765),
                    RuleAttribute::Table(100),
                    RuleAttribute::SuppressPrefixLen(u32::MAX),
                ]), ipv6) => true
            }
            // The header fallback must reject another routing table.
            "different header table" { (rule(ipv6, 200, vec![]), ipv6) => false }
            // A rule naming another table is not reusable even with normal attributes.
            "different table attribute" {
                (rule(ipv6, 200, vec![RuleAttribute::Table(200)]), ipv6) => false
            }
            // A marked rule leaves unmarked metadata replies unmatched.
            "marked policy" {
                (rule(ipv6, 100, vec![RuleAttribute::FwMark(1)]), ipv6) => false
            }
            // A destination restriction cannot serve arbitrary tenant replies.
            "restricted destination" { (restricted_destination, ipv6) => false }
        );
    }

    /// DAD failure takes precedence over tentative state, so a duplicate
    /// address cannot turn into either a healthy endpoint or an endless wait.
    #[test]
    fn ipv6_readiness_checks_duplicate_and_tentative_flags() {
        scenarios!(run = |flags: AddressFlags| {
            let mut address = AddressMessage::default();
            address.attributes.push(AddressAttribute::Flags(flags));
            address_ready(&address).map_err(drop)
        };
            // Permanent addresses may be used after DAD completes.
            "ready" { AddressFlags::Permanent => Yields(true) }
            // A present address may still be unusable while DAD runs.
            "waiting" { AddressFlags::Tentative => Yields(false) }
            // Linux may retain tentative together with the failure bit.
            "duplicate" { AddressFlags::Tentative | AddressFlags::Dadfailed => Fails }
        );
    }
}
