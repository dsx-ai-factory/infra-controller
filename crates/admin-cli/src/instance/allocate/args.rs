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

use carbide_uuid::machine::MachineId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use clap::error::ErrorKind;
use clap::{ArgGroup, CommandFactory, Parser};
use rpc::forge::{InstanceOperatingSystemConfig, InstanceSpxConfig};

#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("selector").required(true).multiple(true).args(&["subnet", "vpc_prefix_id", "flat_vpc_id"])))]
#[command(after_long_help = "\
EXAMPLES:

Allocate one instance onto a VPC prefix:
    $ nico-admin-cli instance allocate --prefix-name eth0 \
    --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567

Allocate one instance onto a subnet:
    $ nico-admin-cli instance allocate --prefix-name eth0 --subnet 192.0.2.0/24

Allocate several instances at once:
    $ nico-admin-cli instance allocate --number 4 --prefix-name eth0 \
    --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567

Allocate transactionally (all-or-nothing for --number > 1):
    $ nico-admin-cli instance allocate --number 4 --prefix-name eth0 \
    --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 --transactional

Allocate onto specific machines:
    $ nico-admin-cli instance allocate --prefix-name eth0 \
    --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 \
    --machine-id 12345678-1234-5678-90ab-cdef01234567

Allocate with an expected instance type and a network security group:
    $ nico-admin-cli instance allocate --prefix-name eth0 \
    --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 \
    --instance-type-id abcdef01-2345-6789-abcd-ef0123456789 \
    --network-security-group-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(short, long)]
    pub(super) number: Option<u16>,

    #[clap(short, long, help = "The subnet to assign to a PF")]
    pub(crate) subnet: Vec<String>,

    #[clap(short, long)]
    // This will not be needed after vpc_prefix implementation.
    // Code can query to carbide and fetch it from db using vpc_prefix_id.
    pub(crate) tenant_org: Option<String>,

    #[clap(short, long, required = true)]
    pub(super) prefix_name: String,

    #[clap(long, help = "The key of label instance to query")]
    pub(crate) label_key: Option<String>,

    #[clap(long, help = "The value of label instance to query")]
    pub(crate) label_value: Option<String>,

    #[clap(
        long,
        help = "The ID of a network security group to apply to the new instance upon creation"
    )]
    pub(crate) network_security_group_id: Option<String>,

    #[clap(
        long,
        help = "The expected instance type id for the instance, which will be compared to type ID set for the machine of the request"
    )]
    pub(crate) instance_type_id: Option<String>,

    #[clap(long, help = "OS definition in JSON format", value_name = "OS_JSON")]
    pub(crate) os: Option<InstanceOperatingSystemConfig>,

    #[clap(
        long,
        help = "SPX configuration in JSON format",
        value_name = "SPX_JSON"
    )]
    pub(crate) spxconfig: Option<InstanceSpxConfig>,

    #[clap(long, help = "The subnet to assign to a VF")]
    pub(crate) vf_subnet: Vec<String>,

    #[clap(short, long, help = "The VPC prefix to assign to a PF")]
    pub(crate) vpc_prefix_id: Vec<VpcPrefixId>,

    #[clap(
        long,
        help = "Create an instance in the given \"flat\" VPC, for machines without DPUs"
    )]
    pub(crate) flat_vpc_id: Option<VpcId>,

    #[clap(long, help = "The VPC prefix to assign to a VF")]
    pub(crate) vf_vpc_prefix_id: Vec<VpcPrefixId>,

    #[clap(long, help = "Explicit IPv4 address to request for each PF interface")]
    pub(crate) ip_address: Vec<String>,

    #[clap(long, help = "Explicit IPv4 address to request for each VF interface")]
    pub(crate) vf_ip_address: Vec<String>,

    #[clap(
        long,
        conflicts_with_all = ["subnet", "vf_subnet", "flat_vpc_id"],
        help = "IPv6 VPC prefix to pair with each PF vpc-prefix-id for dual-stack"
    )]
    pub(crate) ipv6_vpc_prefix_id: Vec<VpcPrefixId>,

    #[clap(
        long,
        requires = "vf_vpc_prefix_id",
        conflicts_with_all = ["subnet", "vf_subnet", "flat_vpc_id"],
        help = "IPv6 VPC prefix to pair with each VF vf-vpc-prefix-id for dual-stack"
    )]
    pub(crate) ipv6_vf_prefix_id: Vec<VpcPrefixId>,

    #[clap(
        long,
        requires = "ipv6_vpc_prefix_id",
        help = "Explicit IPv6 address to request for each PF interface (dual-stack)"
    )]
    pub(crate) ipv6_ip_address: Vec<String>,

    #[clap(
        long,
        requires = "ipv6_vf_prefix_id",
        help = "Explicit IPv6 address to request for each VF interface (dual-stack)"
    )]
    pub(crate) ipv6_vf_ip_address: Vec<String>,

    #[clap(
        long,
        help = "The machine ids for the machines to use (instead of searching)"
    )]
    pub(super) machine_id: Vec<MachineId>,

    #[clap(
        long,
        help = "Use batch API for all-or-nothing allocation (requires --number > 1)"
    )]
    pub(super) transactional: bool,
}

impl Args {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        // Lists pair by position; omitted trailing IPv6 values are valid.
        for (flag, count, paired_flag, paired_count) in [
            (
                "--ipv6-vpc-prefix-id",
                self.ipv6_vpc_prefix_id.len(),
                "--vpc-prefix-id",
                self.vpc_prefix_id.len(),
            ),
            (
                "--ipv6-vf-prefix-id",
                self.ipv6_vf_prefix_id.len(),
                "--vf-vpc-prefix-id",
                self.vf_vpc_prefix_id.len(),
            ),
            (
                "--ipv6-ip-address",
                self.ipv6_ip_address.len(),
                "--ipv6-vpc-prefix-id",
                self.ipv6_vpc_prefix_id.len(),
            ),
            (
                "--ipv6-vf-ip-address",
                self.ipv6_vf_ip_address.len(),
                "--ipv6-vf-prefix-id",
                self.ipv6_vf_prefix_id.len(),
            ),
        ] {
            if count > paired_count {
                return Err(Self::command()
                    .bin_name("nico-admin-cli instance allocate")
                    .error(
                        ErrorKind::TooManyValues,
                        format!(
                            "{flag} has {count} values but {paired_flag} has {paired_count}; supply at most one {flag} value per {paired_flag} value"
                        ),
                    ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn ipv6_options_require_matching_interfaces_and_prefixes() {
        scenarios!(
            run = |argv: &[&str]| {
                Args::try_parse_from(
                    ["allocate", "--prefix-name=ipv6-test"]
                        .into_iter()
                        .chain(argv.iter().copied()),
                )
                .and_then(|args| args.validate())
                .map_err(|error| error.kind())
            };
            "IPv6 prefix alone does not select an interface" {
                &["--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000001"][..]
                    => FailsWith(ErrorKind::MissingRequiredArgument),
            }
            "PF address requires its IPv6 prefix" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--ipv6-ip-address=2001:db8::1",
                ][..] => FailsWith(ErrorKind::MissingRequiredArgument),
            }
            "VF address requires its IPv6 prefix" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--ipv6-vf-ip-address=2001:db8::1",
                ][..] => FailsWith(ErrorKind::MissingRequiredArgument),
            }
            "VF IPv6 prefix requires a VF" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000002",
                ][..] => FailsWith(ErrorKind::MissingRequiredArgument),
            }
            "subnet selection cannot consume IPv6 options" {
                &[
                    "--subnet=tenant-subnet",
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                ][..] => FailsWith(ErrorKind::ArgumentConflict),
            }
            "VF subnet selection cannot consume IPv6 options" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vf-subnet=tenant-subnet",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000003",
                ][..] => FailsWith(ErrorKind::ArgumentConflict),
            }
            "flat selection rejects explicit IPv6 configuration" {
                &[
                    "--flat-vpc-id=00000000-0000-0000-0000-000000000001",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000002",
                ][..] => FailsWith(ErrorKind::ArgumentConflict),
            }
            "PF IPv6 prefixes cannot outnumber PFs" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000003",
                ][..] => FailsWith(ErrorKind::TooManyValues),
            }
            "VF IPv6 prefixes cannot outnumber VFs" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000003",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000004",
                ][..] => FailsWith(ErrorKind::TooManyValues),
            }
            "PF IPv6 addresses cannot outnumber IPv6 prefixes" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000003",
                    "--ipv6-ip-address=2001:db8::1",
                    "--ipv6-ip-address=2001:db8::2",
                ][..] => FailsWith(ErrorKind::TooManyValues),
            }
            "VF IPv6 addresses cannot outnumber IPv6 prefixes" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000003",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000004",
                    "--ipv6-vf-ip-address=2001:db8::1",
                    "--ipv6-vf-ip-address=2001:db8::2",
                ][..] => FailsWith(ErrorKind::TooManyValues),
            }
            "omitted IPv6 lists remain valid" {
                &["--vpc-prefix-id=00000000-0000-0000-0000-000000000001"][..] => Yields(()),
            }
            "shorter IPv6 prefix and address lists remain valid for PFs and VFs" {
                &[
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                    "--vpc-prefix-id=00000000-0000-0000-0000-000000000003",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000004",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000005",
                    "--vf-vpc-prefix-id=00000000-0000-0000-0000-000000000006",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000007",
                    "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000008",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-000000000009",
                    "--ipv6-vf-prefix-id=00000000-0000-0000-0000-00000000000a",
                    "--ipv6-ip-address=2001:db8::1",
                    "--ipv6-vf-ip-address=2001:db8::2",
                ][..] => Yields(()),
            }
        );
    }
}
