/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
List all NIC firmware profile definitions from Core in database collation order \
by ID. The CLI fetches details in bounded batches; profiles deleted during the \
listing are omitted. Entries retain their registered order. This is not an \
atomic catalog snapshot and does not select or update firmware. If a returned \
definition cannot be decoded, the listing fails before writing results.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns an array of complete profiles \
containing id, config, and version. Table/CSV shows one row per entry. An empty \
catalog returns an empty array or a table/CSV header with no data rows.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

List profile IDs, edit versions, and firmware targets:
    $ nico-admin-cli firmware nic profile list

Save all complete profile definitions:
    $ nico-admin-cli --format yaml --output ./nic-profiles.yaml \
    firmware nic profile list

"
)]
pub(crate) struct Args {}
