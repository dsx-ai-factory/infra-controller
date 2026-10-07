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

use crate::credential::common::url_validator;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add a UFM credential with a token:
    $ nico-admin-cli credential add-ufm --url https://192.0.2.10 --token mypassword

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "The UFM url")]
    url: String,

    #[clap(long, default_value(""), help = "The UFM token")]
    token: String,
}

impl TryFrom<Args> for forgerpc::CredentialCreationRequest {
    type Error = CarbideCliError;
    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let username = url_validator(args.url)?;
        Ok(Self {
            credential_type: CredentialType::Ufm.into(),
            username: Some(username),
            password: args.token,
            mac_address: None,
            vendor: None,
            credential_name: None,
        })
    }
}
