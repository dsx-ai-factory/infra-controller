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

use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create a forward DNS domain using the site default TTL:
    $ nico-admin-cli domain create example.com

Create a domain with a ten-minute default record TTL:
    $ nico-admin-cli domain create example.com --default-ttl 600

")]
pub(crate) struct Args {
    #[clap(value_name = "NAME", help = "Lowercase forward DNS domain name")]
    pub(super) name: String,
    #[clap(
        long,
        value_name = "SECONDS",
        help = "Default record TTL, 30 to 86400 seconds"
    )]
    pub(super) default_ttl: Option<u32>,
}
