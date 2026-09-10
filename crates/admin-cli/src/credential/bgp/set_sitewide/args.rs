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

use crate::credential::common::password_validator;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set the site-wide leaf BGP session password:
    $ nico-admin-cli credential bgp set-sitewide --password mynewpassword

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "Leaf BGP session password")]
    password: String,
}

impl TryFrom<Args> for forgerpc::CredentialCreationRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        Ok(Self {
            credential_type: CredentialType::BgpSiteWideLeafPassword.into(),
            username: None,
            password: password_validator(args.password)?,
            mac_address: None,
            vendor: None,
            credential_name: None,
        })
    }
}
