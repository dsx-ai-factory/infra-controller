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

Create an OS definition:
    $ nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a

Create one with a description and mark it inactive:
    $ nico-admin-cli operating-system create --name ubuntu-22.04 --org fds34511233a \
    --description \"Ubuntu 22.04 base\" --is-active false

")]
pub(crate) struct Args {
    #[clap(short, long, help = "Name of the operating system definition.")]
    pub(super) name: String,

    #[clap(
        short,
        long,
        help = "Optional tenant organization identifier for this OS definition. Omit for OS definitions owned by provider."
    )]
    pub(super) org: Option<String>,

    #[clap(
        long,
        help = "Optional UUID for the new OS definition (default: server-generated)."
    )]
    pub(super) id: Option<String>,

    #[clap(short, long, help = "Optional human-readable description.")]
    pub(super) description: Option<String>,

    #[clap(long, help = "Whether this OS definition is active (default: true).")]
    pub(super) is_active: Option<bool>,

    #[clap(
        long,
        default_value = "false",
        help = "Allow instance requests to override the raw iPXE boot script stored by this OS definition. Applies only when the definition stores a raw iPXE script; does not affect templated definitions or user data."
    )]
    pub(super) allow_override: bool,

    #[clap(
        long,
        default_value = "false",
        help = "Whether instances using this OS definition wait for a guest phone-home callback before reporting ready. If the callback never arrives, the instance remains in a provisioning state. REST workflows inject the cloud-init phone_home block and require valid cloud-init YAML; callers using Core directly must arrange the callback. See https://github.com/NVIDIA/infra-controller/blob/main/docs/configuration/tenant_management.md#phone-home."
    )]
    pub(super) phone_home_enabled: bool,

    #[clap(long, help = "Optional cloud-init / user-data script.")]
    pub(super) user_data: Option<String>,

    #[clap(
        long,
        conflicts_with_all = ["ipxe_template_id"],
        help = "Raw iPXE boot script (mutually exclusive with --ipxe-template-id)."
    )]
    pub(super) ipxe_script: Option<String>,

    #[clap(
        long,
        conflicts_with_all = ["ipxe_script"],
        help = "ID of the iPXE template to use (mutually exclusive with --ipxe-script)."
    )]
    pub(super) ipxe_template_id: Option<String>,

    #[clap(
        long = "param",
        value_name = "KEY=VALUE",
        value_parser = parse_param,
        help = "iPXE parameter in KEY=VALUE format. May be repeated."
    )]
    pub(super) params: Vec<IpxeTemplateParameter>,
}
