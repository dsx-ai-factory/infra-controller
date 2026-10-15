/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::path::PathBuf;

use carbide_uuid::machine::HostMachineId;
use clap::{ArgGroup, Parser};
use config_version::ConfigVersion;
use model::nic_firmware::NicFirmwareProfileId;

#[derive(Parser, Debug)]
#[command(
    group(ArgGroup::new("machine_action").required(true).args(["profile", "clear_profile"])),
    long_about = "\
Atomically set or clear a host's machine and card NIC firmware selections at \
the observed Machine.version from firmware nic machine show. Choose exactly \
one of --profile or --clear-profile, even when changing only cards. Resubmitting \
a machine profile rechecks compatibility for devices inheriting it; clearing \
it restores allocation/site inheritance without clearing saved cards.

Omitting --cards-file preserves all saved card selections. Supplying it replaces \
the complete card list in the same request; include every selection you want to \
keep, or use [] to clear all cards. Removing a card restores inheritance from \
the requested machine profile, allocation or site selection. A supplied machine \
profile must support newly inheriting devices. Profiles are live, not pinned \
revisions. Finish upgrading Core before using these configuration commands.

Core validates each supplied card against its latest stored device report. \
Missing, ambiguous, overlapping, managed-DPU or incompatible devices return \
FAILED_PRECONDITION. Missing profiles or hosts return NOT_FOUND; a stale \
machine version returns FAILED_PRECONDITION. Rejection leaves all selections \
unchanged. Every successful write advances the shared machine edit token, \
including unchanged selections. No firmware is fetched or applied, and machine \
readiness does not change.

The command never refreshes a token or retries a mutation. After a connection \
error, inspect machine show before another write: the change may have committed. \
Keep the original token when repeating the same request. Success writes no \
output; use machine show to read the new version. Global --format, --sort-by \
and --extended have no effect. --output PATH before firmware still creates \
a new empty file before the operation, including when the operation fails.",
    after_long_help = "\
EXAMPLES:

Select a machine profile while preserving saved card selections:
    $ nico-admin-cli firmware nic machine set \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --profile connectx-production --if-version-match V1-T1789080000000000

Replace card selections while explicitly keeping the desired machine profile:
    $ nico-admin-cli firmware nic machine set \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --profile connectx-production --cards-file ./nic-cards.json \
    --if-version-match V2-T1789080001000000

Clear only the machine selection, preserving saved cards:
    $ nico-admin-cli firmware nic machine set \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --clear-profile --if-version-match V3-T1789080002000000

Clear both machine and card selections using an explicit empty card array:
    $ printf '[]' | nico-admin-cli firmware nic machine set \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --clear-profile --cards-file - --if-version-match V4-T1789080003000000

"
)]
pub(crate) struct Args {
    #[arg(
        value_name = "HOST",
        help = "Stable or predicted host machine ID, not a DPU ID"
    )]
    pub(super) machine_id: HostMachineId,
    #[arg(
        long,
        value_name = "ID",
        help = "Live machine profile ID; case-sensitive, nonempty, without surrounding whitespace or controls"
    )]
    pub(super) profile: Option<NicFirmwareProfileId>,
    #[arg(
        long,
        help = "Clear the machine profile to inherit; saved cards are preserved unless --cards-file replaces them"
    )]
    pub(super) clear_profile: bool,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Complete observed Machine.version; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
    #[arg(long, value_name = "PATH", help = "Replace all card selections from a JSON file, or '-' for stdin; omission preserves cards", long_help = super::input::CARDS_HELP)]
    pub(super) cards_file: Option<PathBuf>,
}
