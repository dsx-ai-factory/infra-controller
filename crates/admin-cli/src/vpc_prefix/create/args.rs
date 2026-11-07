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

use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use clap::Parser;
use ipnet::IpNet;
use rpc::forge::VpcPrefixCreationRequest;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a prefix in a VPC:
    $ nico-admin-cli vpc-prefix create --vpc-id 12345678-1234-5678-90ab-cdef01234567 \
    --prefix 10.0.0.0/24 --name web-tier

Create a prefix with a description and labels:
    $ nico-admin-cli vpc-prefix create --vpc-id 12345678-1234-5678-90ab-cdef01234567 \
    --prefix 10.0.0.0/24 --name web-tier --description \"Front-end subnet\" \
    --label environment:production --label team:platform

Create a prefix from a specific SitePrefix:
    $ nico-admin-cli vpc-prefix create --vpc-id 12345678-1234-5678-90ab-cdef01234567 \
    --site-prefix-id abcdef01-2345-6789-abcd-ef0123456789 \
    --prefix 10.0.0.0/24 --name web-tier

")]
pub(crate) struct Args {
    #[clap(
        long,
        name = "vpc-id",
        value_name = "VpcId",
        help = "The ID of the VPC to contain this prefix"
    )]
    vpc_id: VpcId,

    #[clap(
        long,
        name = "site-prefix-id",
        value_name = "SitePrefixId",
        help = "The exact parent SitePrefix ID. Required for tenant-managed SitePrefixes or when multiple operator-managed SitePrefixes contain this VPC prefix. When omitted, Core selects the unique containing operator-managed SitePrefix when one exists"
    )]
    site_prefix_id: Option<SitePrefixId>,

    #[clap(
        long,
        name = "prefix",
        value_name = "CIDR-prefix",
        help = "The IP prefix in CIDR notation"
    )]
    prefix: IpNet,

    #[clap(
        long,
        name = "name",
        value_name = "prefix-name",
        help = "A short descriptive name for the prefix"
    )]
    name: String,

    #[clap(
        long,
        name = "description",
        value_name = "description",
        help = "Optionally, a longer description for the prefix"
    )]
    description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "A labels that will be added as metadata for the newly created VPC prefix. The labels key and value must be separated by a : character. E.g. environment:production",
        action = clap::ArgAction::Append
    )]
    labels: Option<Vec<String>>,

    #[clap(
        long,
        name = "vpc-prefix-id",
        value_name = "VpcPrefixId",
        help = "Specify the VpcPrefixId for the API to use instead of it auto-generating one"
    )]
    vpc_prefix_id: Option<VpcPrefixId>,
}

fn parse_label(s: &str) -> rpc::forge::Label {
    match s.split_once(':') {
        Some((k, v)) => rpc::forge::Label {
            key: k.trim().to_string(),
            value: Some(v.trim().to_string()),
        },
        None => rpc::forge::Label {
            key: s.trim().to_string(),
            value: None,
        },
    }
}

impl From<Args> for VpcPrefixCreationRequest {
    fn from(args: Args) -> Self {
        let labels = args
            .labels
            .unwrap_or_default()
            .iter()
            .map(|s| parse_label(s))
            .collect();

        VpcPrefixCreationRequest {
            id: args.vpc_prefix_id,
            prefix: String::new(), // Deprecated field
            vpc_id: Some(args.vpc_id),
            site_prefix_id: args.site_prefix_id,
            config: Some(rpc::forge::VpcPrefixConfig {
                prefix: args.prefix.to_string(),
            }),
            metadata: Some(rpc::forge::Metadata {
                name: args.name,
                labels,
                description: args.description.unwrap_or_default(),
            }),
        }
    }
}
