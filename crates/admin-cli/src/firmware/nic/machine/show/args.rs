/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine::HostMachineId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Read saved machine and card NIC firmware selections, including selectors that \
no longer match the stored device report. This is configuration, not effective \
firmware; use firmware nic desired for resolution and errors. The machine \
version is the shared edit token required by machine set, not a profile version.

Use matching v2.4 CLI and Core NIC firmware schemas, and finish upgrading all \
Core replicas before relying on the reported selections.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
firmware. JSON/YAML returns one object with machine_id, version, profile_id \
(null means inherit), and card_selections. YAML selectors use !BaseMac and \
!BaseGuid tags. Extract card_selections as JSON for --cards-file; YAML is not \
accepted as input. Tables have one machine row plus \
one row per card in saved order; an empty machine profile cell means inherit. \
Card ordering has no selection significance.

An unknown host fails. Output defaults to stdout; --output must name a new \
file and creates it before the read, even on failure. --sort-by and --extended \
have no effect. No configuration, readiness, or hardware is changed.",
    after_long_help = "\
EXAMPLES:

Read saved selections and their shared edit version:
    $ nico-admin-cli firmware nic machine show \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Save the configuration as JSON for inspection before editing:
    $ nico-admin-cli --format json --output ./nic-machine.json \
    firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

"
)]
pub(crate) struct Args {
    #[arg(
        value_name = "HOST",
        help = "Stable or predicted host machine ID, not a DPU ID"
    )]
    pub(super) machine_id: HostMachineId,
}
