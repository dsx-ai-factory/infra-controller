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

/// Update an expected machine from a JSON file.
#[derive(Parser, Debug)]
#[command(long_about = r#"Update an expected machine from a JSON file.

The file requires `bmc_mac_address`, `bmc_username`, `bmc_password`, and `chassis_serial_number`.
The MAC selects the existing machine; the `id` in the file is ignored. Core PATCH requires nonempty
credentials and a serial containing 4-32 ASCII letters, digits, hyphens, or underscores.
Legacy fallback uses the validation rules on the older server.

Supplied fields replace their stored values. Omitted or `null` optional fields preserve them,
except `metadata`: its `name`, `description`, and `labels` are always replaced. Omitted or `null` `metadata`
clears all three. A supplied `metadata` object requires `name`, `description`, and `labels`.
Omitted or `null` `interfaces` preserves the stored list; `[]` clears it. The `host_nics` alias is also
accepted, but do not supply both spellings. Interface role and allocation inheritance follow
`expected-machine patch`. An empty `fallback_dpu_serial_numbers` array clears that list.

Omitted or `null` `host_lifecycle_profile` preserves the profile. When supplying the object, set
`disable_lockdown` explicitly to `true` or `false`. An empty object preserves the setting with both
Core PATCH and legacy fallback.

The command first tries Core PATCH, which merges selected fields atomically. It falls back to
the legacy update on `Unimplemented` or `PermissionDenied`, or when a MAC lookup returns no ID.
The legacy machine update reads the record, merges changes locally, and replaces it. Concurrent
changes can be overwritten on that path. The legacy request still requires authorization.
Other PATCH errors and failed legacy updates remain errors.

https://github.com/dsx-ai-factory/infra-controller/pull/6359

Example JSON file:

```json

   {
       "bmc_mac_address": "1a:1b:1c:1d:1e:1f",
       "bmc_username": "user",
       "bmc_password": "pass",
       "chassis_serial_number": "sample_serial-1",
       "fallback_dpu_serial_numbers": ["MT020100000003"],
       "metadata": {
           "name": "MyMachine",
           "description": "My Machine",
           "labels": [{"key": "ABC", "value": "DEF"}]
       },
       "sku_id": "sku_id_123"
   }

```"#)]
#[command(after_long_help = "\
EXAMPLES:

Update an expected machine from a JSON file:
    $ nico-admin-cli expected-machine update --filename ./machine.json

")]
pub(crate) struct Args {
    #[clap(
        short,
        long,
        help = "Path to JSON file containing the expected machine data"
    )]
    pub(super) filename: String,
}
