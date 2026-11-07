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

use carbide_uuid::switch::SwitchId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all managed switches:
    $ nico-admin-cli managed-switch show

Show one managed switch by ID or name:
    $ nico-admin-cli managed-switch show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(help = "Switch ID or name to show details for (leave empty for all)")]
    identifier: Option<String>,

    #[clap(
        short,
        long,
        action,
        help = "Show BMC/NVOS MAC details in summary",
        conflicts_with = "identifier"
    )]
    pub(super) ips: bool,

    #[clap(
        short,
        long,
        action,
        help = "Show serial, power, and health details in summary",
        conflicts_with = "identifier"
    )]
    pub(super) more: bool,
}

impl Args {
    pub(super) fn parse_identifier(&self) -> (Option<SwitchId>, Option<String>) {
        match &self.identifier {
            Some(id) if !id.is_empty() => match SwitchId::from_str(id) {
                Ok(switch_id) => (Some(switch_id), None),
                Err(_) => (None, Some(id.clone())),
            },
            _ => (None, None),
        }
    }
}
