/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod desired;
mod machine;
mod profile;
mod site;
mod tenant;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List registered NIC firmware profiles:
    $ nico-admin-cli firmware nic profile list

Read a profile and its edit version:
    $ nico-admin-cli --format json firmware nic profile show connectx-production

List site-wide profile selections:
    $ nico-admin-cli firmware nic site list

Read a tenant's selection and shared configuration version:
    $ nico-admin-cli firmware nic tenant show --tenant-org-id fds34511233a

Inspect saved machine and card selections:
    $ nico-admin-cli firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Show desired firmware from the stored device report:
    $ nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

")]
pub(crate) enum Cmd {
    #[dispatch]
    #[clap(subcommand, about = "Manage NIC firmware profile definitions in Core")]
    Profile(profile::Cmd),
    #[dispatch]
    #[clap(subcommand, about = "Manage site-wide NIC firmware selections")]
    Site(site::Cmd),
    #[dispatch]
    #[clap(subcommand, about = "Manage tenant NIC firmware profile selections")]
    Tenant(tenant::Cmd),
    #[dispatch]
    #[clap(
        subcommand,
        about = "Manage machine and card NIC firmware configuration"
    )]
    Machine(machine::Cmd),
    #[command(about = "Show desired NIC firmware and selection diagnostics")]
    Desired(desired::Args),
}
