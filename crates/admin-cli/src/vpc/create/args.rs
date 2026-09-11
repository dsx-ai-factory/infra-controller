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

use carbide_uuid::vpc::VpcId;
use clap::{Parser, ValueEnum};
use rpc::{Metadata, forge};

#[derive(ValueEnum, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
enum VpcVirtualizationTypeArg {
    EthernetVirtualizer,
    /// Deprecated. NVUE is implied; use ethernet-virtualizer.
    EthernetVirtualizerWithNvue,
    /// Deprecated. Use fnn.
    FnnClassic,
    /// Deprecated. Use fnn.
    FnnL3,
    Fnn,
    /// Flat networking for instances attached directly to the underlay.
    /// NICo does not manage routing or ACL enforcement between Flat VPCs and
    /// other VPCs; that is the responsibility of the network operator.
    Flat,
}

#[allow(deprecated)]
impl From<VpcVirtualizationTypeArg> for forge::VpcVirtualizationType {
    fn from(value: VpcVirtualizationTypeArg) -> Self {
        match value {
            VpcVirtualizationTypeArg::EthernetVirtualizer => Self::EthernetVirtualizer,
            VpcVirtualizationTypeArg::EthernetVirtualizerWithNvue => {
                Self::EthernetVirtualizerWithNvue
            }
            VpcVirtualizationTypeArg::FnnClassic => Self::FnnClassic,
            VpcVirtualizationTypeArg::FnnL3 => Self::FnnL3,
            VpcVirtualizationTypeArg::Fnn => Self::Fnn,
            VpcVirtualizationTypeArg::Flat => Self::Flat,
        }
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a tenant VPC:
    $ nico-admin-cli --cloud-unsafe-op=my_username vpc create --name tenant-vpc-1 --org-id tenant-org-1

Create a tenant VPC with flat virtualization and a chosen ID:
    $ nico-admin-cli --cloud-unsafe-op=my_username vpc create --name tenant-vpc-1 --org-id tenant-org-1 --id ad1f9fd5-8438-4407-b259-72fdb7896d42 --virtualization-type flat

Create an FNN VPC with SLAAC enabled:
    $ nico-admin-cli --cloud-unsafe-op=admin vpc create --name tenant-vpc-1 --org-id fds34511233a --virtualization-type fnn --slaac-enabled true

")]
pub(crate) struct Args {
    #[clap(long, help = "Name to give the new VPC")]
    name: String,

    #[clap(long, help = "Description for the new VPC")]
    description: Option<String>,

    #[clap(
        long,
        value_name = "VpcId",
        help = "Optional VPC ID to use instead of allowing the API server to generate one"
    )]
    id: Option<VpcId>,

    #[clap(
        long,
        help = "Tenant organization ID (Plain text string, used by cloud API)"
    )]
    org_id: String,

    #[clap(
        long,
        value_enum,
        default_value = "ethernet-virtualizer",
        help = "Network virtualization type"
    )]
    virtualization_type: VpcVirtualizationTypeArg,

    #[clap(
        long,
        action = clap::ArgAction::Set,
        value_name = "SLAAC_ENABLED",
        help = "Whether Core should allocate an IPv6 /64 for each IPv6-enabled instance interface. Supported only for FNN VPCs; NICo does not configure router advertisements. Enabling requires the connected Core to advertise VPC SLAAC support and fails otherwise. Omit or set false to disable. This setting cannot be changed after creation"
    )]
    slaac_enabled: Option<bool>,
}

impl From<Args> for forge::VpcCreationRequest {
    fn from(args: Args) -> Self {
        Self {
            tenant_organization_id: args.org_id,
            tenant_keyset_id: None,
            network_virtualization_type: Some(forge::VpcVirtualizationType::from(
                args.virtualization_type,
            ) as _),
            id: None,
            metadata: Some(Metadata {
                name: args.name,
                description: args.description.unwrap_or_default(),
                ..Default::default()
            }),
            network_security_group_id: None,
            default_nvlink_logical_partition_id: None,
            vni: None,
            routing_profile_type: None,
            routing_profile_overrides: None,
            power_resource_group: None,
            slaac_enabled: args.slaac_enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use ::rpc::forge::{self, VpcVirtualizationType};
    use carbide_test_support::value_scenarios;
    use clap::Parser;

    use super::Args;

    #[test]
    fn vpc_creation_request_preserves_slaac_presence() {
        value_scenarios!(
            run = |slaac_enabled: Option<&str>| {
                let mut argv = vec![
                    "vpc-create",
                    "--name",
                    "tenant-vpc",
                    "--org-id",
                    "tenant-org",
                ];
                if let Some(slaac_enabled) = slaac_enabled {
                    argv.extend(["--slaac-enabled", slaac_enabled]);
                }
                let request: forge::VpcCreationRequest = Args::try_parse_from(argv)
                    .expect("valid VPC create arguments")
                    .into();
                request.slaac_enabled
            };
            "presence" {
                None => None,
                Some("false") => Some(false),
                Some("true") => Some(true),
            }
        );
    }

    #[test]
    #[allow(deprecated)]
    fn virtualization_types_preserve_the_rpc_values() {
        let cases = [
            (
                "ethernet-virtualizer",
                VpcVirtualizationType::EthernetVirtualizer,
            ),
            (
                "ethernet-virtualizer-with-nvue",
                VpcVirtualizationType::EthernetVirtualizerWithNvue,
            ),
            ("fnn-classic", VpcVirtualizationType::FnnClassic),
            ("fnn-l3", VpcVirtualizationType::FnnL3),
            ("fnn", VpcVirtualizationType::Fnn),
            ("flat", VpcVirtualizationType::Flat),
        ];

        for (cli_value, expected) in cases {
            let args = Args::try_parse_from([
                "create",
                "--name",
                "test-vpc",
                "--org-id",
                "test-org",
                "--virtualization-type",
                cli_value,
            ])
            .unwrap_or_else(|error| panic!("failed to parse {cli_value}: {error}"));

            assert_eq!(
                VpcVirtualizationType::from(args.virtualization_type),
                expected,
                "CLI value: {cli_value}"
            );
        }
    }

    #[test]
    fn virtualization_type_defaults_to_ethernet_virtualizer() {
        let args = Args::try_parse_from(["create", "--name", "test-vpc", "--org-id", "test-org"])
            .expect("default virtualization type should parse");

        assert_eq!(
            VpcVirtualizationType::from(args.virtualization_type),
            VpcVirtualizationType::EthernetVirtualizer
        );
    }
}
