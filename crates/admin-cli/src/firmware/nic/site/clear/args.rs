/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use config_version::ConfigVersion;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Clear the site NIC firmware selection for one exact part-number/PSID pair. \
The complete observed selection version is required; the profile edit version \
is not interchangeable. Core returns NOT_FOUND for an absent selection and \
FAILED_PRECONDITION for a stale token. Part number and PSID are case-sensitive \
literal identifiers; Core requires 1–256 UTF-8 bytes each without surrounding \
whitespace or controls, returning INVALID_ARGUMENT for invalid hardware.

This removes only the site selection. It leaves the live profile and any \
machine or card selections unchanged, and does not fetch artifacts or start \
firmware updates. Devices with no other selection become unmanaged. The CLI \
does not read a fresh token or retry a rejected write. After a connection \
error, inspect the hardware pair with firmware nic site list before another \
write; the clear may have committed. Keep the original token when repeating \
the same request.

Success writes no bytes in any --format. Global --format and --output PATH \
must precede the firmware subcommand. Output defaults to stdout. --output \
requires a path that does not exist; the file is created before the operation \
and remains empty even if the clear fails. --sort-by and --extended have no \
effect.",
    after_long_help = "\
EXAMPLES:

Clear a site selection at its previously observed selection version:
    $ nico-admin-cli firmware nic site clear --part-number MCX75310AAS-NEAT \
    --psid MT_0000000838 --if-version-match V1-T1789080000000000

"
)]
pub(crate) struct Args {
    #[arg(long, value_name = "PN", help = "Literal manufacturer part number")]
    pub(super) part_number: String,
    #[arg(long, help = "Literal firmware parameter-set identifier")]
    pub(super) psid: String,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Original observed selection version, such as V1-T1789080000000000; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
}
