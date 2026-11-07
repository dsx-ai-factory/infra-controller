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

/// Replace all entries in the expected machines table with the entries from an inputted json file.
///
/// Example json file:
///    {
///        "expected_machines":
///        [
///            {
///                "bmc_mac_address": "1a:1b:1c:1d:1e:1f",
///                "bmc_username": "user",
///                "bmc_password": "pass",
///                "chassis_serial_number": "sample_serial-1"
///            },
///            {
///                "bmc_mac_address": "2a:2b:2c:2d:2e:2f",
///                "bmc_username": "user",
///                "bmc_password": "pass",
///                "chassis_serial_number": "sample_serial-2",
///                "fallback_dpu_serial_numbers": ["MT020100000003"],
///                "metadata": {
///                    "name": "MyMachine",
///                    "description": "My Machine",
///                    "labels": [{"key": "ABC", "value": "DEF"}]
///                }
///            }
///        ]
///    }
#[derive(Parser, Debug)]
#[clap(verbatim_doc_comment)]
#[command(after_long_help = "\
EXAMPLES:

Replace the entire expected machines table from a JSON file:
    $ nico-admin-cli expected-machine replace-all --filename ./expected-machines.json

")]
pub(crate) struct Args {
    #[clap(short, long)]
    pub(super) filename: String,
}
