/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::net::{Ipv4Addr, Ipv6Addr};

use carbide_uuid::network::NetworkSegmentId;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use ipnet::IpNet;
use rpc::admin_cli::OutputFormat;

// Core validates the stored segment; Clap rejects invalid option combinations.
#[derive(Parser, Debug)]
#[command(
    long_about = "Append a prefix to a live, Ready Underlay segment with exactly one prefix and no VPC attachment. Existing prefix IDs, addresses, policy and lifecycle remain unchanged, as does the stored initial TOML definition. Matching any stored same-family prefix and settings returns the segment without a write; different same-family settings fail. Overlap with any segment prefix or VPC-scoped VPC prefix is rejected; a global VPC parent may overlap an unattached underlay. Reverse DNS is committed with the addition for zone-aligned prefixes. Existing SLAAC inference policy applies to the new prefix. Site routing, router advertisements and DHCP relays are not configured. Requires administrator credentials and the global --cloud-unsafe-op acknowledgement. Do not use --cloud-unsafe-op against a production site. Output supports ascii-table, json and yaml; csv is not accepted.",
    after_long_help = "\
EXAMPLES:

Add IPv4 to an IPv6-only underlay:
    $ nico-admin-cli --cloud-unsafe-op=admin network-segment add-prefix \
    --id 12345678-1234-5678-90ab-cdef01234567 --prefix 192.0.2.0/24 --gateway 192.0.2.1

Add IPv6 with an optional relay link-address selector:
    $ nico-admin-cli --cloud-unsafe-op=admin network-segment add-prefix \
    --id 12345678-1234-5678-90ab-cdef01234567 --prefix 2001:db8::/64 \
    --dhcpv6-link-address 2001:db8:ffff::1 --reserve-first 5

"
)]
pub(crate) struct Args {
    #[clap(
        long,
        value_name = "NetworkSegmentId",
        help = "ID of the existing Underlay network segment"
    )]
    id: NetworkSegmentId,

    #[clap(
        long,
        value_name = "CIDR-prefix",
        help = "Network CIDR with an explicit numeric prefix length and no host bits; IPv4 must be shorter than /32. Adds the missing family to the segment or matches existing settings without a write"
    )]
    prefix: IpNet,

    #[clap(
        long,
        value_name = "IPv4-address",
        help = "Required for IPv4: an IPv4 gateway within the added prefix. Not accepted for IPv6"
    )]
    gateway: Option<Ipv4Addr>,

    #[clap(
        long,
        default_value_t = 0,
        value_name = "COUNT",
        value_parser = clap::value_parser!(i32).range(0..),
        help = "Non-negative number of initial addresses excluded from dynamic allocation in the new prefix"
    )]
    reserve_first: i32,

    #[clap(
        long,
        value_name = "IPv6-address",
        help = "Optional IPv6 DHCP relay link-address selector for an IPv6 prefix; it can be outside the prefix. Omit for prefix-based selection. Exact matches take precedence: choose a selector that does not redirect requests from another network"
    )]
    dhcpv6_link_address: Option<Ipv6Addr>,
}

impl Args {
    /// Rejects cross-field invocation errors through Clap before contacting Core.
    pub(super) fn validate(&self, format: OutputFormat) -> Result<(), clap::Error> {
        let command = || Self::command().bin_name("nico-admin-cli network-segment add-prefix");
        if format == OutputFormat::Csv {
            return Err(command().error(
                ErrorKind::ArgumentConflict,
                "--format csv is not supported for prefix append; use ascii-table, json, or yaml",
            ));
        }
        match (self.prefix, self.gateway) {
            (IpNet::V4(_), None) => {
                return Err(command().error(
                    ErrorKind::MissingRequiredArgument,
                    "--gateway is required for an IPv4 --prefix",
                ));
            }
            (IpNet::V6(_), Some(_)) => {
                return Err(command().error(
                    ErrorKind::ArgumentConflict,
                    "--gateway is not accepted for an IPv6 --prefix",
                ));
            }
            _ => {}
        }
        if matches!(self.prefix, IpNet::V4(_)) && self.dhcpv6_link_address.is_some() {
            return Err(command().error(
                ErrorKind::ArgumentConflict,
                "--dhcpv6-link-address requires an IPv6 --prefix",
            ));
        }
        Ok(())
    }
}

impl From<Args> for rpc::forge::AddNetworkSegmentPrefixRequest {
    fn from(args: Args) -> Self {
        Self {
            network_segment_id: Some(args.id),
            prefix: args.prefix.to_string(),
            gateway: args.gateway.map(|address| address.to_string()),
            reserve_first: args.reserve_first,
            dhcpv6_link_address: args.dhcpv6_link_address.map(|address| address.to_string()),
        }
    }
}
