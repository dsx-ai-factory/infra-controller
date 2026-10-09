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

use crate::credential::common::DEFAULT_IB_FABRIC_NAME;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Generate a UFM cert for the default fabric:
    $ nico-admin-cli credential generate-ufm-cert

Generate a UFM cert for a named fabric:
    $ nico-admin-cli credential generate-ufm-cert --fabric default

")]
pub(crate) struct Args {
    #[clap(long, default_value_t = DEFAULT_IB_FABRIC_NAME.to_string(), help = "Infiniband fabric.")]
    fabric: String,
}

impl From<Args> for forgerpc::CredentialCreationRequest {
    fn from(args: Args) -> Self {
        Self {
            credential_type: CredentialType::Ufm.into(),
            username: None,
            password: "".to_string(),
            mac_address: None,
            vendor: Some(args.fabric),
            credential_name: None,
        }
    }
}
