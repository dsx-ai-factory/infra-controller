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
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create an infrastructure domain:
    $ nico-admin-cli domain create mysite.example.com

Create a domain owned by a VPC:
    $ nico-admin-cli domain create compute.customer.example \
    --vpc-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        value_name = "NAME",
        help = "Domain name. Reverse zones (in-addr.arpa, ip6.arpa) and duplicate live names are rejected"
    )]
    pub(super) name: String,

    #[clap(
        long,
        value_name = "VpcId",
        help = "VPC that owns the domain. A VPC owns at most one live domain"
    )]
    pub(super) vpc_id: Option<VpcId>,

    #[clap(
        long,
        value_name = "SECONDS",
        help = "Default TTL for the zone's records, 30 to 86400 seconds. Omit for the site default"
    )]
    pub(super) default_ttl: Option<u32>,
}
