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
Select a compatible live profile for one exact site hardware pair. Omit \
--if-version-match only to create a new selection; Core returns ALREADY_EXISTS \
if the pair already has a selection. To replace a selection, pass its complete \
observed selection version, not the profile edit version. Stale versions return \
FAILED_PRECONDITION and absent selections return NOT_FOUND. A missing profile \
returns NOT_FOUND; an incompatible profile returns FAILED_PRECONDITION. Core \
checks profile compatibility before the selection version.

Part number and PSID are case-sensitive literal identifiers. Core requires \
1–256 UTF-8 bytes each, without surrounding whitespace or controls; invalid \
hardware returns INVALID_ARGUMENT. Wildcards are not expanded. The CLI neither \
reads a fresh token nor retries a rejected write. After a connection error, \
inspect the hardware pair with firmware nic site list before another write; \
the change may have committed. Keep the original token when repeating the \
same request.

The selection follows subsequent edits to the live profile without changing \
its independent selection token. This command leaves the profile definition \
unchanged and does not fetch artifacts or start firmware updates. Finish \
upgrading all Core replicas before assigning site selections.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns a one-element array with hardware \
(part_number and psid), profile_id, and version. Table/CSV returns one row with \
Part Number, PSID, Profile ID, and Selection Version columns. Output defaults \
to stdout. --output requires a path that does not exist and creates the file \
before the operation, even if it later fails. --sort-by and --extended have no \
effect.",
    after_long_help = "\
EXAMPLES:

Create a new site selection:
    $ nico-admin-cli firmware nic site set --part-number MCX75310AAS-NEAT \
    --psid MT_0000000838 --profile connectx-production

Replace a selection using its previously observed selection version:
    $ nico-admin-cli --format json firmware nic site set \
    --part-number MCX75310AAS-NEAT --psid MT_0000000838 \
    --profile connectx-candidate --if-version-match V1-T1789080000000000

"
)]
pub(crate) struct Args {
    #[arg(long, value_name = "PN", help = "Literal manufacturer part number")]
    pub(super) part_number: String,
    #[arg(long, help = "Literal firmware parameter-set identifier")]
    pub(super) psid: String,
    #[arg(
        long,
        value_name = "ID",
        help = "Case-sensitive compatible profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) profile: NicFirmwareProfileId,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Original observed selection version, such as V1-T1789080000000000; omit only to create"
    )]
    pub(super) if_version_match: Option<ConfigVersion>,
}
