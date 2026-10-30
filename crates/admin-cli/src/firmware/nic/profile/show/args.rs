/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use model::nic_firmware::NicFirmwareProfileId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Read one NIC firmware profile definition and its edit version from Core. An \
unknown ID fails with the CLI error \"NIC firmware profile <ID> not found\". \
No hardware selection or firmware update is performed.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON and YAML output is a one-element array containing id, \
config, and version. Extract [0].config from JSON output as JSON for create or update \
input; YAML output is not accepted as input. Table and CSV output has one row per entry. \
Keep the observed version for a later update or delete.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Show a profile and its edit version:
    $ nico-admin-cli firmware nic profile show connectx-production

Save the complete definition, including artifact URLs and digests:
    $ nico-admin-cli --format json --output ./nic-profile-record.json \
    firmware nic profile show connectx-production

"
)]
pub(crate) struct Args {
    #[arg(
        help = "Case-sensitive profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) id: NicFirmwareProfileId,
}
