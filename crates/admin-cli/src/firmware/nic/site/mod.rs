/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod clear;
mod list;
mod output;
mod set;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(
    long_about = "\
Manage site-wide NIC firmware profile selections by exact part-number/PSID pair. \
A site selection applies only when no card, machine, or allocation selection \
exists for a device. It selects a live profile, not a pinned profile revision; \
the selection version is independent of the profile's edit version. These \
commands do not change profile definitions, fetch artifacts, or start firmware \
updates. Finish upgrading all Core replicas before assigning site selections.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. List and set return arrays of hardware, profile_id, \
and version objects in JSON/YAML, or one row per selection in table/CSV. Clear \
writes no bytes. Output defaults to stdout. --output requires a path that does \
not exist and creates the file before the operation, even if the operation \
later fails. --sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

List site selections and their edit versions:
    $ nico-admin-cli firmware nic site list

Create a site selection for one exact hardware pair:
    $ nico-admin-cli firmware nic site set --part-number MCX75310AAS-NEAT \
    --psid MT_0000000838 --profile connectx-production

Clear a site selection using its previously observed selection version:
    $ nico-admin-cli firmware nic site clear --part-number MCX75310AAS-NEAT \
    --psid MT_0000000838 --if-version-match V1-T1789080000000000

"
)]
pub(crate) enum Cmd {
    #[command(about = "List site NIC firmware selections and their edit versions")]
    List(list::Args),
    #[command(about = "Create or conditionally replace a site NIC firmware selection")]
    Set(set::Args),
    #[command(about = "Clear a site NIC firmware selection at its observed version")]
    Clear(clear::Args),
}
