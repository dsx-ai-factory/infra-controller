/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod set;
mod show;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(
    long_about = "Read or atomically change the live NIC firmware profiles selected for a host and its cards. These commands manage stored intent, not firmware execution. Use firmware nic desired to inspect resolution against stored device facts.",
    after_long_help = "\
EXAMPLES:

Read saved selections and their shared machine edit version:
    $ nico-admin-cli --format json firmware nic machine show \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Select a machine profile while preserving saved card selections:
    $ nico-admin-cli firmware nic machine set \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --profile connectx-production --if-version-match V1-T1789080000000000

"
)]
pub(crate) enum Cmd {
    #[command(about = "Show saved NIC firmware selections and the machine edit version")]
    Show(show::Args),
    #[command(about = "Set or clear machine and card selections at an observed machine version")]
    Set(set::Args),
}
