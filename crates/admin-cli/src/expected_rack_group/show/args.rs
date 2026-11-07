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

use carbide_uuid::rack::RackGroupId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all expected rack groups:
    $ nico-admin-cli expected-rack-group show

Show one group:
    $ nico-admin-cli expected-rack-group show nvl5-gp1-jhb01

Export the full inventory for replace-all:
    $ nico-admin-cli --format json expected-rack-group show

")]
pub(crate) struct Args {
    /// External group ID; omit to show all groups.
    pub(super) rack_group_id: Option<RackGroupId>,
}
