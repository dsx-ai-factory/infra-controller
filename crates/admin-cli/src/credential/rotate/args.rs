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
use rpc::forge as forgerpc;

use crate::credential::common::{RotationCredentialKind, password_validator};
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Rotate the site-wide BMC root password, letting the server auto-generate a strong one:
    $ nico-admin-cli credential rotate --type=bmc

Rotate the host UEFI password to an explicit value:
    $ nico-admin-cli credential rotate --type=host-uefi --password=Str0ng-Explicit-Pw!

Stage a site-wide NVOS target password after seeding current per-switch credentials:
    $ nico-admin-cli credential rotate --type=nvos --password=MyNvosPassword-2026

Rotate the SuperNIC lockdown IKM with an audit note:
    $ nico-admin-cli credential rotate --type=lockdown-ikm --reason=\"quarterly rotation\"

Rotate the BF4 DPU BMC service account password (converges every enrolled BF4 DPU):
    $ nico-admin-cli credential rotate --type=dpu-bmc-service

")]
pub(crate) struct Args {
    #[clap(
        long = "type",
        require_equals(true),
        required(true),
        help = "Credential family to rotate"
    )]
    credential_type: RotationCredentialKind,

    #[clap(
        long,
        require_equals(true),
        help = "Explicit rotate-to password. Omit to have the server auto-generate a strong one."
    )]
    password: Option<String>,

    #[clap(
        long,
        help = "Free-form note recorded with the rotation (must not contain secrets)"
    )]
    reason: Option<String>,
}

impl TryFrom<Args> for forgerpc::RotateCredentialRequest {
    type Error = CarbideCliError;
    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let password = match args.password {
            Some(password) => Some(password_validator(password)?),
            None => None,
        };
        Ok(Self {
            credential_type: forgerpc::RotationCredentialType::from(args.credential_type) as i32,
            password,
            reason: args.reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use rpc::forge::RotationCredentialType;

    use super::*;

    #[test]
    fn rotate_maps_to_proto() {
        let explicit = Args {
            credential_type: RotationCredentialKind::DpuUefi,
            password: Some("Str0ng-Explicit-Pw!".to_string()),
            reason: Some("note".to_string()),
        };
        let request =
            forgerpc::RotateCredentialRequest::try_from(explicit).expect("convert explicit");
        assert_eq!(
            request.credential_type,
            RotationCredentialType::RotationDpuUefi as i32
        );
        assert_eq!(request.password, Some("Str0ng-Explicit-Pw!".to_string()));
        assert_eq!(request.reason, Some("note".to_string()));

        let auto = Args {
            credential_type: RotationCredentialKind::Bmc,
            password: None,
            reason: None,
        };
        let request = forgerpc::RotateCredentialRequest::try_from(auto).expect("convert automatic");
        assert_eq!(
            request.credential_type,
            RotationCredentialType::RotationBmc as i32
        );
        assert!(request.password.is_none());
    }
}
