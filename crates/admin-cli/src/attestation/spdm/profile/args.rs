/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use clap::{Parser, ValueEnum};
use rpc::forge::AttesterSelectionMode;

use crate::cfg::dispatch::Dispatch;

#[derive(Dispatch, Parser, Debug)]
pub(crate) enum Cmd {
    #[clap(about = "List every stored attestation profile")]
    List(List),
    #[clap(about = "Show the attestation profile for one hardware class")]
    Get(Get),
    #[clap(about = "Store an attestation profile for a hardware class")]
    Create(Create),
    #[clap(about = "Replace the selection of a stored attestation profile")]
    Update(Update),
    #[clap(about = "Remove the attestation profile for a hardware class")]
    Delete(Delete),
}

/// The spelling an operator writes and the one tables print back. Kept apart
/// from the wire enum, which carries an unset sentinel that is not a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Mode {
    /// Attest nothing. Attestation is off for this hardware.
    None,
    /// Attest every attester the BMC reports.
    All,
    /// Attest only the attesters a pattern matches.
    Allowlist,
    /// Attest every attester the BMC reports except those a pattern matches.
    Denylist,
}

impl From<Mode> for AttesterSelectionMode {
    fn from(value: Mode) -> Self {
        match value {
            Mode::None => Self::None,
            Mode::All => Self::All,
            Mode::Allowlist => Self::Allowlist,
            Mode::Denylist => Self::Denylist,
        }
    }
}

/// The patterns of one selection, in the order they were given. `ALLOWLIST` and
/// `DENYLIST` require at least one; `ALL` and `NONE` take none. The server
/// enforces that, so these flags stay free of inter-flag rules that would have
/// to agree with it.
#[derive(Parser, Debug)]
pub(crate) struct Patterns {
    #[clap(
        long,
        value_name = "COMPONENT_ID",
        help = "Match one ComponentIntegrity ID in full. Repeatable"
    )]
    pub(super) exact: Vec<String>,
    #[clap(
        long,
        value_name = "PREFIX",
        help = "Match every ComponentIntegrity ID starting with this. Repeatable"
    )]
    pub(super) prefix: Vec<String>,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List the stored profiles with the version each update takes:
    $ nico-admin-cli attestation spdm profile list

")]
pub(crate) struct List {}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show the profile that applies to GB200 trays:
    $ nico-admin-cli attestation spdm profile get Gb200

Show the fallback that applies to hardware with no profile of its own:
    $ nico-admin-cli attestation spdm profile get any

")]
pub(crate) struct Get {
    #[clap(help = "Hardware class the profile is keyed to")]
    pub(super) hardware_class: String,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Attest the GPU integrity reports on GB200 trays:
    $ nico-admin-cli attestation spdm profile create Gb200 \
    --mode allowlist --prefix HGX_IRoT_GPU_

Mix pattern kinds in one selection:
    $ nico-admin-cli attestation spdm profile create Gb200 \
    --mode allowlist --prefix HGX_IRoT_GPU_ --exact VERA_CPU_0

Attest everything reported by hardware that has no profile of its own:
    $ nico-admin-cli attestation spdm profile create any --mode all

")]
pub(crate) struct Create {
    #[clap(help = "Hardware class the profile is keyed to. One of the classes \
                exploration records, or 'any' for the fallback. Run \
                'attestation spdm coverage' to see the classes this site has")]
    pub(super) hardware_class: String,
    #[clap(long, value_enum, help = "Which attesters the hardware requires")]
    pub(super) mode: Mode,
    #[clap(flatten)]
    pub(super) patterns: Patterns,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Replace the selection, failing if someone else wrote first:
    $ nico-admin-cli attestation spdm profile update Gb200 \
    --mode denylist --exact HGX_BMC_0 --if-version-match V7-T1789080000000000

Switch the fallback off, which stops attesting every class without a profile
of its own:
    $ nico-admin-cli attestation spdm profile update any --mode none --force

")]
pub(crate) struct Update {
    #[clap(help = "Hardware class the profile is keyed to")]
    pub(super) hardware_class: String,
    #[clap(long, value_enum, help = "Which attesters the hardware requires")]
    pub(super) mode: Mode,
    #[clap(flatten)]
    pub(super) patterns: Patterns,
    #[clap(
        long,
        help = "Apply only if the stored version still matches this. Omitted \
                applies to whatever is stored now"
    )]
    pub(super) if_version_match: Option<String>,
    #[clap(
        long,
        help = "Required to switch the 'any' fallback to mode none, which \
                stops attesting every class without a profile of its own"
    )]
    pub(super) force: bool,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Remove a profile, failing if someone else wrote first:
    $ nico-admin-cli attestation spdm profile delete Gb200 \
    --if-version-match V7-T1789080000000000

Remove the fallback, which leaves every class without a profile of its own
attesting nothing:
    $ nico-admin-cli attestation spdm profile delete any --force

")]
pub(crate) struct Delete {
    #[clap(help = "Hardware class the profile is keyed to")]
    pub(super) hardware_class: String,
    #[clap(
        long,
        help = "Apply only if the stored version still matches this. Omitted \
                applies to whatever is stored now"
    )]
    pub(super) if_version_match: Option<String>,
    #[clap(
        long,
        help = "Required to remove the 'any' fallback, which leaves every \
                class without a profile of its own attesting nothing"
    )]
    pub(super) force: bool,
}
