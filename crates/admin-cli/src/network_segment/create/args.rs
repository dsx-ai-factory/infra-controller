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

use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use ipnet::IpNet;
use rpc::forge;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a tenant network segment with an IPv4 prefix:
    $ nico-admin-cli --cloud-unsafe-op=admin network-segment create --name tenant-segment-1 --vpc-id 12345678-1234-5678-90ab-cdef01234567 --prefix 10.0.0.0/24 --gateway 10.0.0.1

Create a dual-stack host in-band segment with a chosen ID and SLAAC EUI-64 inference:
    $ nico-admin-cli --cloud-unsafe-op=admin network-segment create --name host-inband-a \
    --segment-type host-inband --id 12345678-1234-5678-90ab-cdef01234567 \
    --prefix 192.0.2.0/24 --gateway 192.0.2.1 --prefix 2001:db8::/64 \
    --subdomain-id abcdef01-2345-6789-abcd-ef0123456789 \
    --infer-slaac-eui64-addresses

")]
pub(crate) struct Args {
    #[clap(long, help = "Network segment name")]
    name: String,

    #[clap(
        long,
        value_name = "NetworkSegmentId",
        help = "Optional network segment ID to use instead of allowing the API server to generate one"
    )]
    id: Option<NetworkSegmentId>,

    #[clap(
        long,
        value_name = "VpcId",
        help = "Optional VPC ID to attach the new segment to"
    )]
    vpc_id: Option<VpcId>,

    #[clap(
        long,
        value_name = "DomainId",
        help = "DNS subdomain ID used for DHCP and DNS records on the segment. Required for segments of type host-inband"
    )]
    pub(super) subdomain_id: Option<DomainId>,

    #[clap(
        long,
        value_name = "MTU",
        help = "Optional MTU for the segment. Defaults to 9000 for tenant segments and 1500 for other segment types"
    )]
    mtu: Option<i32>,

    #[clap(
        long,
        name = "prefix",
        value_name = "CIDR-prefix",
        help = "Network prefix in CIDR notation. Repeat once per address family",
        action = clap::ArgAction::Append,
        required = true
    )]
    prefix: Vec<IpNet>,

    #[clap(
        long,
        value_name = "IPv4-address",
        help = "IPv4 gateway for the IPv4 prefix"
    )]
    gateway: Option<Ipv4Addr>,

    #[clap(
        long,
        default_value_t = 0,
        value_name = "COUNT",
        help = "Number of addresses to reserve before dynamic allocation starts"
    )]
    reserve_first: i32,

    #[clap(
        long,
        value_enum,
        default_value = "tenant",
        help = "Network segment type"
    )]
    pub(super) segment_type: forge::NetworkSegmentType,

    #[clap(
        long,
        help = "Infer modified EUI-64 SLAAC addresses for stateless DHCPv6 clients and add them to interface address state. Off by default; use only when clients derive SLAAC addresses from their MAC addresses, and only on a dynamic segment with exactly one IPv6 /64. Existing IPv6 addresses are not replaced"
    )]
    infer_slaac_eui64_addresses: bool,
}

impl Args {
    /// Validates cross-field constraints that clap cannot express.
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        // `no_gateway_on_ipv6` means the request's gateway needs an IPv4
        // prefix to carry it.
        if self.gateway.is_some()
            && !self
                .prefix
                .iter()
                .any(|prefix| matches!(prefix, IpNet::V4(_)))
        {
            return Err(Self::command()
                .bin_name("nico-admin-cli network-segment create")
                .error(
                    ErrorKind::ArgumentConflict,
                    "--gateway requires an IPv4 --prefix",
                ));
        }

        Ok(())
    }
}

impl From<Args> for forge::NetworkSegmentCreationRequest {
    fn from(args: Args) -> Self {
        let prefixes = args
            .prefix
            .into_iter()
            .map(|prefix| {
                let gateway = if matches!(prefix, IpNet::V4(_)) {
                    args.gateway.map(|gateway| gateway.to_string())
                } else {
                    None
                };

                forge::NetworkPrefix {
                    id: None,
                    prefix: prefix.to_string(),
                    gateway,
                    reserve_first: args.reserve_first,
                    free_ip_count: 0,
                    svi_ip: None,
                    free_ip_count_v2: None,
                    free_ip_count_saturated: false,
                }
            })
            .collect();

        Self {
            vpc_id: args.vpc_id,
            name: args.name,
            subdomain_id: args.subdomain_id,
            mtu: args.mtu,
            prefixes,
            segment_type: args.segment_type as i32,
            id: args.id,
            infer_slaac_eui64_addresses: args.infer_slaac_eui64_addresses,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn validate_gateway_by_prefix_family() {
        #[derive(Debug, PartialEq)]
        struct ValidationFailure {
            kind: clap::error::ErrorKind,
            exit_code: i32,
            names_conflict: bool,
            shows_create_usage: bool,
        }

        scenarios!(
            run = |argv| {
                Args::try_parse_from(argv.iter().copied())
                    .expect("scenario must parse as network-segment create")
                    .validate()
                    .map_err(|err| {
                        let rendered = err.to_string();
                        ValidationFailure {
                            kind: err.kind(),
                            exit_code: err.exit_code(),
                            names_conflict: rendered
                                .contains("--gateway requires an IPv4 --prefix"),
                            shows_create_usage: rendered
                                .contains("Usage: nico-admin-cli network-segment create"),
                        }
                    })
            };
            "dual-stack prefix accepts an IPv4 gateway" {
                &[
                    "network-segment-create",
                    "--name=segment",
                    "--prefix=192.0.2.0/24",
                    "--gateway=192.0.2.1",
                    "--prefix=2001:db8::/64",
                ][..] => Yields(()),
            }

            "IPv6-only prefix rejects an IPv4 gateway" {
                &[
                    "network-segment-create",
                    "--name=segment",
                    "--prefix=2001:db8::/64",
                    "--gateway=192.0.2.1",
                ][..] => FailsWith(ValidationFailure {
                    kind: clap::error::ErrorKind::ArgumentConflict,
                    exit_code: 2,
                    names_conflict: true,
                    shows_create_usage: true,
                }),
            }
        );
    }
}
