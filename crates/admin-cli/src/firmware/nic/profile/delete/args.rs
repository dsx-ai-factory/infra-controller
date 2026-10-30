/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use config_version::ConfigVersion;
use model::nic_firmware::NicFirmwareProfileId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Delete a NIC firmware profile using its original observed --if-version-match \
token. For an unknown profile, Core returns server status NOT_FOUND. A stale \
version or any site, machine, card, or tenant selection referencing the profile \
returns server status FAILED_PRECONDITION, including saved card selectors no longer in \
inventory.

The command preserves the server diagnostic, does not read a fresh version, and \
does not retry deletion. After a connection error, inspect the profile; deletion \
may have committed. Keep the original token when repeating the same request. \
No hardware selection is changed and no firmware update is started.

Delete writes no bytes to stdout or the requested output file, for every \
--format. Put --output PATH before the firmware subcommand. PATH must not exist \
and is created before deletion, even if deletion later fails; successful deletion \
leaves that file empty. --sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Delete a profile with no selections, using its previously observed edit version:
    $ nico-admin-cli firmware nic profile delete connectx-retired \
    --if-version-match V3-T1789080002000000

"
)]
pub(crate) struct Args {
    #[arg(
        help = "Case-sensitive profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) id: NicFirmwareProfileId,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Original observed edit version, such as V1-T1789080000000000; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
}
