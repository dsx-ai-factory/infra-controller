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

use crate::component_manager::common::SwitchIdArgs;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Rotate the NVOS mTLS certificate on one switch via Maintenance:
    $ nico-admin-cli component-manager configure-switch-certificate \
    --switch-id 12345678-1234-5678-90ab-cdef01234567

Rotate certificates on multiple switches:
    $ nico-admin-cli component-manager configure-switch-certificate \
    --switch-id 12345678-1234-5678-90ab-cdef01234567,abcdef01-2345-6789-abcd-ef0123456789

Dispatch directly to the component backend, bypassing the switch state controller:
    $ nico-admin-cli component-manager configure-switch-certificate \
    --switch-id 12345678-1234-5678-90ab-cdef01234567 --bypass-state-controller

")]
pub(crate) struct Args {
    #[clap(flatten)]
    ids: SwitchIdArgs,

    #[clap(
        long = "domain-name",
        help = "Optional certificate domain passed through to RMS; omit to use the RMS default"
    )]
    domain_name: Option<String>,

    #[clap(
        long = "bypass-state-controller",
        help = "Bypass the switch state controller and dispatch directly to the component backend"
    )]
    bypass_state_controller: bool,
}

impl From<Args> for rpc::forge::ComponentConfigureSwitchCertificateRequest {
    fn from(args: Args) -> Self {
        Self {
            switch_ids: Some(args.ids.into()),
            domain_name: args.domain_name,
            bypass_state_controller: args.bypass_state_controller,
        }
    }
}
