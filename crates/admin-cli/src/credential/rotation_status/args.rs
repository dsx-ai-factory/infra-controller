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

use crate::credential::common::RotationCredentialKind;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show site-wide convergence for the BMC root rotation:
    $ nico-admin-cli credential rotation-status --type=bmc

Show site-wide convergence for the SuperNIC lockdown IKM rotation:
    $ nico-admin-cli credential rotation-status --type=lockdown-ikm

Show convergence for a single device by MAC:
    $ nico-admin-cli credential rotation-status --type=bmc --mac-address 00:11:22:33:44:55

")]
pub(crate) struct Args {
    #[clap(
        long = "type",
        require_equals(true),
        required(true),
        help = "Credential family to report on"
    )]
    pub(super) credential_type: RotationCredentialKind,
    #[clap(
        long,
        help = "Report on a single device by MAC instead of the whole site"
    )]
    pub(super) mac_address: Option<MacAddress>,
}
