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

List all domains:
    $ nico-admin-cli domain show

Show one domain by ID:
    $ nico-admin-cli domain show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(
        short,
        long,
        action,
        conflicts_with = "domain",
        help = "Show all domains (DEPRECATED)"
    )]
    pub(crate) all: bool,

    #[clap(
        default_value(None),
        help = "The domain to query, leave empty for all (default)"
    )]
    pub(crate) domain: Option<DomainId>,
}
