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
use mac_address::MacAddress;
use rpc::{CredentialType, forge as forgerpc};

use crate::credential::common::BmcCredentialType;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Delete the site-wide BMC root credential:
    $ nico-admin-cli credential delete-bmc --kind=site-wide-root

Delete a per-BMC root credential for a specific MAC address:
    $ nico-admin-cli credential delete-bmc --kind=bmc-root --mac-address 00:11:22:33:44:55

")]
pub(crate) struct Args {
    #[clap(
        long,
        require_equals(true),
        required(true),
        help = "The BMC Credential kind"
    )]
    kind: BmcCredentialType,
    #[clap(long, help = "The MAC address of the BMC")]
    mac_address: Option<MacAddress>,
}

impl From<Args> for forgerpc::CredentialDeletionRequest {
    fn from(args: Args) -> Self {
        Self {
            credential_type: CredentialType::from(args.kind).into(),
            username: None,
            mac_address: args.mac_address.map(|mac| mac.to_string()),
            credential_name: None,
        }
    }
}
