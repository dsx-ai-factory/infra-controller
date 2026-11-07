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

use std::str::FromStr;

use carbide_uuid::power_shelf::PowerShelfId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Delete a power shelf by ID:
    $ nico-admin-cli power-shelf delete 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(help = "Power Shelf ID to delete.")]
    power_shelf_id: String,
}

impl Args {
    pub(super) fn parse_power_shelf_id(&self) -> Result<PowerShelfId, String> {
        PowerShelfId::from_str(&self.power_shelf_id)
            .map_err(|_| format!("Invalid power shelf ID: {}", self.power_shelf_id))
    }
}
