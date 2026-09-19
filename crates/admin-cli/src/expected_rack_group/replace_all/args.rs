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

Replace all expected rack groups from a JSON file:
    $ nico-admin-cli expected-rack-group replace-all --filename ./rack-groups.json

File shape (an empty expected_rack_groups array clears all groups):
    {\"expected_rack_groups\":[{\"rack_group_id\":\"nvl5-gp1-jhb01\",\"topology\":\"gb200_nvl72r1_c2g4\",\"rack_ids\":[],\"members\":[]}]}

The optional expected_rack_groups_count must equal the array length.
metadata uses name, description, and labels as [{\"key\":\"location.datacenter\",\"value\":\"JHB01\"}].

")]
pub(crate) struct Args {
    /// JSON inventory file. Missing rack_ids/members/metadata default to empty.
    ///
    /// The root object contains expected_rack_groups, an array of objects with
    /// rack_group_id, topology, rack_ids, members, and metadata. The optional
    /// expected_rack_groups_count must match the array length. An empty array
    /// clears all groups.
    ///
    /// Each member has type, manufacturer, and id. Metadata contains name,
    /// description, and labels as an array of {key, value} objects. Export a
    /// compatible file with --format json expected-rack-group show.
    #[arg(short, long)]
    pub(super) filename: String,
}
