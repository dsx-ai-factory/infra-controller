/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
List NIC firmware profile definitions from Core in database collation order \
by ID. Omit both hardware options to list every profile, or supply both \
--part-number and --psid to list profiles containing that exact hardware pair. \
Core requires each identifier to contain 1–256 UTF-8 bytes without surrounding \
whitespace or controls; matching is literal and case-sensitive. The filter \
checks compatibility, not whether a profile is selected for any device. \
An unknown valid hardware pair returns an empty list. Finish upgrading every \
Core replica before using the hardware filter; older replicas ignore it and \
return the unfiltered list.

The CLI fetches details in bounded batches; profiles deleted during the \
listing are omitted. A profile edited after ID enumeration can return details \
that no longer match the hardware filter. Entries retain their registered \
order. This is not an atomic catalog snapshot and does not select or update \
firmware. If a returned definition cannot be decoded, the listing fails before \
writing results.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON and YAML output is an array of complete profiles \
containing id, config, and version. Table and CSV output has one row per entry. An empty \
catalog returns an empty array, or a table or CSV header with no data rows.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

List profile IDs, edit versions, and firmware targets:
    $ nico-admin-cli firmware nic profile list

List profiles compatible with one exact hardware pair:
    $ nico-admin-cli firmware nic profile list --part-number MCX75310AAS-NEAT \
    --psid MT_0000000838

Save all complete profile definitions:
    $ nico-admin-cli --format yaml --output ./nic-profiles.yaml \
    firmware nic profile list

"
)]
pub(crate) struct Args {
    #[arg(
        long,
        requires = "psid",
        value_name = "PN",
        help = "Literal, case-sensitive manufacturer part number; requires --psid"
    )]
    pub(super) part_number: Option<String>,
    #[arg(
        long,
        requires = "part_number",
        help = "Literal, case-sensitive firmware parameter-set identifier; requires --part-number"
    )]
    pub(super) psid: Option<String>,
}
