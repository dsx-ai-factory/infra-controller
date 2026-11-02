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

Show all named profile definitions:
    $ nico-admin-cli routing-profile show

Show one configured profile in JSON, preserving unset properties:
    $ nico-admin-cli --format json routing-profile show --name MAINTENANCE

Show multiple configured profiles:
    $ nico-admin-cli routing-profile show --name MAINTENANCE EXTERNAL

Show internal and external profiles at access tier 1 or higher:
    $ nico-admin-cli routing-profile show --minimum-access-tier 1

Show selected internal profiles at access tier 1 or higher:
    $ nico-admin-cli routing-profile show --name MAINTENANCE INTERNAL --internal true --minimum-access-tier 1

Show external profiles, including profiles whose internal setting is unset:
    $ nico-admin-cli routing-profile show --internal false

")]
pub(crate) struct Args {
    #[arg(
        value_name = "NAME",
        long = "name",
        num_args = 1..,
        help = "Exact, case-sensitive names; omit to discover matching profiles. Unknown explicit names fail; repeat --name or supply multiple values"
    )]
    pub(super) names: Vec<String>,
    #[arg(
        long,
        help = "True returns only internal profiles; false returns only external profiles. Omit to include both. Profiles with no value for the internal setting are treated as external."
    )]
    pub(super) internal: Option<bool>,
    #[arg(
        long,
        help = "Inclusive minimum access tier; profiles without an access_tier setting count as zero"
    )]
    pub(super) minimum_access_tier: Option<u32>,
}
