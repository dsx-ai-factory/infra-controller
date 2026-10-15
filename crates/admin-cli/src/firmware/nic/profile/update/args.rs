/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use config_version::ConfigVersion;
use model::nic_firmware::NicFirmwareProfileId;

use super::super::input::ConfigInput;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Replace the complete NIC firmware profile config in Core. Include every entry \
you want to keep; this is not a merge or patch. Core requires the original \
observed --if-version-match token and advances the edit version on success. \
For an unknown profile, Core returns server status NOT_FOUND; stale versions \
return server status FAILED_PRECONDITION.

Core also rejects removal of hardware support required by existing selections \
with server status FAILED_PRECONDITION. Compatibility checks precede the \
stored-version check. The command preserves the server diagnostic and does not \
read a fresh version or retry the update. After a connection error, inspect the \
profile; the change may have committed. Keep the original token when repeating \
the same request.

This changes the live definition used by existing selections, without fetching \
artifacts or starting firmware updates. Use --format ascii-table (default), \
csv, json, or yaml and --output PATH before the firmware subcommand. JSON/YAML \
returns a one-element array containing the stored id, config, and new version. \
Table/CSV shows one row per entry.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Replace a profile using its previously observed edit version:
    $ nico-admin-cli firmware nic profile update connectx-production \
    --config-file ./nic-profile.json --if-version-match V1-T1789080000000000

Replace from standard input and print the new definition as JSON:
    $ cat ./nic-profile.json | nico-admin-cli --format json firmware nic profile update \
    connectx-production --if-version-match V1-T1789080000000000

"
)]
pub(crate) struct Args {
    #[arg(
        help = "Case-sensitive profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) id: NicFirmwareProfileId,
    #[command(flatten)]
    pub(super) input: ConfigInput,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Original observed edit version, such as V1-T1789080000000000; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
}
