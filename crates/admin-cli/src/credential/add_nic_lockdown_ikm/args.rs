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

Set the site-wide SuperNIC lockdown IKM (input key material):
    $ nico-admin-cli credential add-nic-lockdown-ikm --password mypassword

")]
pub(crate) struct Args {
    #[clap(long, required(true), help = "The site-wide NIC lockdown IKM value")]
    password: String,
}

impl TryFrom<Args> for forgerpc::CredentialCreationRequest {
    type Error = CarbideCliError;
    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let password = password_validator(args.password)?;
        Ok(Self {
            credential_type: CredentialType::SiteWideNicLockdownIkm.into(),
            username: None,
            password,
            mac_address: None,
            vendor: None,
            credential_name: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use rpc::forge::CredentialType;

    use super::*;

    #[test]
    fn add_nic_lockdown_ikm_maps_to_proto() {
        let args = Args {
            password: "ikm-secret".to_string(),
        };
        let request = forgerpc::CredentialCreationRequest::try_from(args).expect("convert");

        assert_eq!(
            request.credential_type,
            CredentialType::SiteWideNicLockdownIkm as i32
        );
        assert_eq!(request.password, "ikm-secret");
        assert!(request.username.is_none());
        assert!(request.mac_address.is_none());
    }
}
