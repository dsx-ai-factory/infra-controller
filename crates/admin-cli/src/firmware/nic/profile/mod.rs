/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod create;
mod delete;
mod input;
mod list;
mod output;
mod show;
mod update;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(
    long_about = "\
Manage named NIC firmware profile definitions stored in Core. These commands \
do not select profiles for hardware, fetch firmware artifacts, or start firmware \
updates. Updates replace the complete definition and require its observed edit \
version; each deletion also requires its observed version.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. Create, show, list, and update return an array of \
complete profiles in JSON or YAML. Table and CSV output has one row per firmware entry, \
including the profile edit version and target firmware version. Extract \
[0].config from JSON output as JSON for create or update input; YAML output is not \
accepted as input.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. Delete \
writes no bytes but still creates a requested output file. --sort-by and \
--extended have no effect on these commands.",
    after_long_help = "\
EXAMPLES:

List registered profiles:
    $ nico-admin-cli firmware nic profile list

Read a profile and its edit version:
    $ nico-admin-cli --format json firmware nic profile show connectx-production

Create a profile from standard input:
    $ cat ./nic-profile.json | nico-admin-cli firmware nic profile create \
    connectx-production

Replace a profile at its observed edit version:
    $ nico-admin-cli firmware nic profile update connectx-production \
    --config-file ./nic-profile.json --if-version-match V1-T1789080000000000

Delete a profile with no selections, using its independently observed edit version:
    $ nico-admin-cli firmware nic profile delete connectx-retired \
    --if-version-match V3-T1789080002000000

"
)]
pub(crate) enum Cmd {
    #[command(about = "Create a NIC firmware profile definition")]
    Create(create::Args),
    #[command(about = "Show one NIC firmware profile definition and its edit version")]
    Show(show::Args),
    #[command(about = "List NIC firmware profile definitions and their edit versions")]
    List(list::Args),
    #[command(about = "Replace a NIC firmware profile definition at its observed version")]
    Update(update::Args),
    #[command(about = "Delete a NIC firmware profile with no selections at its observed version")]
    Delete(delete::Args),
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use crate::firmware::Cmd;

    /// The command tree requires the `firmware nic profile` hierarchy.
    #[test]
    fn command_structure_is_valid() {
        Cmd::command().debug_assert();
        // `nic-profile` is not an alias for the `nic` domain.
        assert_eq!(
            Cmd::try_parse_from(["firmware", "nic-profile", "profile", "list"])
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidSubcommand
        );
    }
}
