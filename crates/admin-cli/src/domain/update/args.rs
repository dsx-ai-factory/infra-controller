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

use carbide_uuid::domain::DomainId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Change a domain's default record TTL to ten minutes:
    $ nico-admin-cli domain update 12345678-1234-5678-90ab-cdef01234567 --default-ttl 600

")]
pub(crate) struct Args {
    #[clap(value_name = "DomainId", help = "ID of the domain to update")]
    pub(super) domain: DomainId,

    #[clap(
        long,
        value_name = "SECONDS",
        help = "Default TTL for the zone's records, 30 to 86400 seconds. Once set it cannot be \
                cleared back to the site default"
    )]
    pub(super) default_ttl: u32,
}
