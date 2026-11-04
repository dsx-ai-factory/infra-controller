/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use model::nic_firmware::NicFirmwareProfileId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
List site NIC firmware selections from Core, ordered by part number then PSID \
in database collation order. Omit --profile to list all selections, or supply \
an exact profile ID to list only its selections. An unknown valid profile ID \
returns an empty list. The CLI enumerates keys and fetches details in bounded \
batches; selections cleared between those reads are omitted. A selection \
changed after key enumeration can show a different profile than --profile \
requested. This is not an atomic snapshot. It does not change profiles, \
selections, or firmware.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns an array of objects with hardware \
(part_number and psid), profile_id, and version. Table/CSV returns Part Number, \
PSID, Profile ID, and Selection Version columns. The selection version is \
independent of the profile edit version. An empty list produces an empty array \
or a table/CSV header with no data rows.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

List all site selections:
    $ nico-admin-cli firmware nic site list

List selections using one profile:
    $ nico-admin-cli firmware nic site list --profile connectx-production

Save complete selections and their version tokens:
    $ nico-admin-cli --format json --output ./nic-site-selections.json \
    firmware nic site list

"
)]
pub(crate) struct Args {
    #[arg(
        long,
        value_name = "ID",
        help = "Exact case-sensitive profile ID; omit to list all selections"
    )]
    pub(super) profile: Option<NicFirmwareProfileId>,
}
