/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use model::nic_firmware::NicFirmwareProfileId;

use super::super::input::ConfigInput;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Create a named NIC firmware profile in Core. An existing ID returns the server \
status ALREADY_EXISTS. Registration validates and stores the definition without \
fetching artifacts, selecting hardware, or starting updates. The returned \
profile includes its new edit version.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns a one-element array containing id, \
config, and version. Table/CSV shows one row per entry. The command does not \
retry creation; after a connection error, use show to inspect whether it committed.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Create a profile from a JSON file:
    $ nico-admin-cli firmware nic profile create connectx-production \
    --config-file ./nic-profile.json

Create a profile from standard input and print its stored definition as JSON:
    $ cat ./nic-profile.json | nico-admin-cli --format json firmware nic profile create \
    connectx-production

"
)]
pub(crate) struct Args {
    #[arg(
        help = "Case-sensitive profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) id: NicFirmwareProfileId,
    #[command(flatten)]
    pub(super) input: ConfigInput,
}
