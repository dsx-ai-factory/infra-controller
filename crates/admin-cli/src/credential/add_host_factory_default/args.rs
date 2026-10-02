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
use rpc::{CredentialType, forge as forgerpc};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add the factory-default host BMC credential for a vendor:
    $ nico-admin-cli credential add-host-factory-default --vendor nvidia \
    --username admin --password mypassword

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "Default username: root, ADMIN, etc")]
    username: String,
    #[clap(long, required(true), help = "Manufacturer default password")]
    password: String,
    #[clap(long, required(true))]
    vendor: bmc_vendor::BMCVendor,
}

impl From<Args> for forgerpc::CredentialCreationRequest {
    fn from(args: Args) -> Self {
        Self {
            credential_type: CredentialType::HostBmcFactoryDefault.into(),
            username: Some(args.username),
            password: args.password,
            mac_address: None,
            vendor: Some(args.vendor.to_string()),
            credential_name: None,
        }
    }
}
