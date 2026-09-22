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

use ::rpc::forge::IpxeTemplateParameter;
use clap::Parser;

use crate::operating_system::common::parse_param;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Rename an OS definition and update its description:
    $ nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 \
    --name ubuntu-22.04 --description \"Ubuntu 22.04 base\"

Deactivate an OS definition:
    $ nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 --is-active false

Replace the iPXE boot script:
    $ nico-admin-cli operating-system update 12345678-1234-5678-90ab-cdef01234567 \
    --ipxe-script \"#!ipxe …\"

")]
pub(crate) struct Args {
    #[clap(help = "UUID of the operating system definition to update.")]
    pub(super) id: String,

    #[clap(short, long, help = "New name for the operating system definition.")]
    pub(super) name: Option<String>,

    #[clap(short, long, help = "New description.")]
    pub(super) description: Option<String>,

    #[clap(long, help = "Set whether this OS definition is active.")]
    pub(super) is_active: Option<bool>,

    #[clap(
        long,
        help = "Set whether instance requests can override the raw iPXE boot script stored by this OS definition. Applies only when the definition stores a raw iPXE script; does not affect templated definitions or user data."
    )]
    pub(super) allow_override: Option<bool>,

    #[clap(
        long,
        help = "Set whether instances using this OS definition wait for a guest phone-home callback before reporting ready. If the callback never arrives, the instance remains in a provisioning state. REST workflows inject the cloud-init phone_home block and require valid cloud-init YAML; callers using Core directly must arrange the callback. See https://github.com/NVIDIA/infra-controller/blob/main/docs/configuration/tenant_management.md#phone-home."
    )]
    pub(super) phone_home_enabled: Option<bool>,

    #[clap(long, help = "Update the cloud-init / user-data script.")]
    pub(super) user_data: Option<String>,

    #[clap(
        long,
        conflicts_with = "ipxe_template_id",
        help = "Update the raw iPXE boot script."
    )]
    pub(super) ipxe_script: Option<String>,

    #[clap(
        long,
        conflicts_with = "ipxe_script",
        help = "Update the iPXE template ID."
    )]
    pub(super) ipxe_template_id: Option<String>,

    #[clap(
        long = "param",
        value_name = "KEY=VALUE",
        value_parser = parse_param,
        num_args = 0..,
        help = "Replace all iPXE parameters with these KEY=VALUE pairs. May be repeated. Pass without values to clear."
    )]
    pub(super) params: Option<Vec<IpxeTemplateParameter>>,
}
