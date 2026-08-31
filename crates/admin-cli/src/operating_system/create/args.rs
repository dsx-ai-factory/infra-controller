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
use clap::{ArgGroup, Parser};

use crate::operating_system::common::parse_param;

#[derive(Parser, Debug, Clone)]
#[command(group(
    ArgGroup::new("ipxe_variant")
        .required(true)
        .multiple(false)
        .args(["ipxe_script", "ipxe_template_id"])
))]
#[command(after_long_help = "\
EXAMPLES:

Create a provider-owned OS definition:
    $ nico-admin-cli operating-system create --name provider-ubuntu-22.04 \
    --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567

Create a tenant-owned OS definition:
    $ nico-admin-cli operating-system create --name tenant-ubuntu-22.04 --org fds34511233a \
    --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567

Create one with a description, inactive, allowing parameter overrides:
    $ nico-admin-cli operating-system create --name tenant-ubuntu-22.04 --org fds34511233a \
    --description \"Ubuntu 22.04 base\" --is-active false --allow-override \
    --ipxe-template-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(short, long, help = "Name of the operating system definition.")]
    pub(super) name: String,

    #[clap(
        short,
        long,
        help = "Optional tenant organization identifier for this OS definition. Omit for a provider-owned definition. An explicitly empty value is invalid."
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
        help = "Allow an Instance request to override the user data of this OS definition."
    )]
    pub(super) allow_override: bool,

    #[clap(
        long,
        default_value = "false",
        help = "Enable phone-home on first boot."
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Args;

    #[test]
    fn create_requires_exactly_one_ipxe_variant() {
        let cases = [
            (
                "neither variant",
                vec!["create", "--name", "test-os"],
                false,
            ),
            (
                "both variants",
                vec![
                    "create",
                    "--name",
                    "test-os",
                    "--ipxe-script",
                    "#!ipxe",
                    "--ipxe-template-id",
                    "12345678-1234-5678-90ab-cdef01234567",
                ],
                false,
            ),
            (
                "raw script",
                vec!["create", "--name", "test-os", "--ipxe-script", "#!ipxe"],
                true,
            ),
            (
                "template ID",
                vec![
                    "create",
                    "--name",
                    "test-os",
                    "--ipxe-template-id",
                    "12345678-1234-5678-90ab-cdef01234567",
                ],
                true,
            ),
        ];

        for (name, argv, expected_valid) in cases {
            assert_eq!(
                Args::try_parse_from(argv).is_ok(),
                expected_valid,
                "case: {name}"
            );
        }
    }
}
