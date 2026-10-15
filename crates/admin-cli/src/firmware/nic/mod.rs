/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod profile;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List registered NIC firmware profiles:
    $ nico-admin-cli firmware nic profile list

Read a profile and its edit version:
    $ nico-admin-cli --format json firmware nic profile show connectx-production

")]
pub(crate) enum Cmd {
    #[dispatch]
    #[clap(subcommand, about = "Manage NIC firmware profile definitions in Core")]
    Profile(profile::Cmd),
}
