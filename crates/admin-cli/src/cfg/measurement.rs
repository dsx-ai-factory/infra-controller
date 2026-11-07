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

/*

/// cfg/measurement.rs
/// Baseline top-level arguments for the Measured Boot CLI commands.

*/

use clap::Parser;
use measured_boot::pcr::PcrRegisterValue;

use crate::attestation::measured_boot::{bundle, journal, machine, profile, report, site};

// KvPair is a really simple struct for holding
// a key/value pair, and is used for parsing
// k:v,... groupings via the CLI.
#[derive(Clone, Debug)]
pub(crate) struct KvPair {
    pub(crate) key: String,
    pub(crate) value: String,
}

pub(crate) fn parse_colon_pairs(arg: &str) -> eyre::Result<KvPair> {
    let pair: Vec<&str> = arg.split(':').collect();
    if pair.len() != 2 {
        return Err(eyre::eyre!("must be <first>:<second>"));
    }

    Ok(KvPair {
        key: pair[0].to_string(),
        value: pair[1].to_string(),
    })
}

pub(crate) fn parse_pcr_register_values(arg: &str) -> eyre::Result<PcrRegisterValue> {
    let pair: Vec<&str> = arg.split(':').collect();
    if pair.len() != 2 {
        return Err(eyre::eyre!("must be <num>:<val>"));
    }

    let pcr_register = pair[0]
        .parse::<i16>()
        .map_err(|_| eyre::eyre!("pcr_register must be a number"))?;
    let sha = pair[1].to_string();
    Ok(PcrRegisterValue {
        pcr_register,
        sha_any: sha,
    })
}

/// Cmd is the top-level subcommands enum, which contains mappings for all
/// top-level commands (e.g. `bundle`, `journal`, etc).

#[derive(Parser, Debug)]
pub(crate) enum Cmd {
    #[clap(
        subcommand,
        about = "Work with golden measurement bundles.",
        visible_alias = "b"
    )]
    Bundle(bundle::args::CmdBundle),

    #[clap(
        subcommand,
        about = "Work with machine meausrement journals",
        visible_alias = "j"
    )]
    Journal(journal::args::CmdJournal),

    #[clap(subcommand, about = "Work with machine reports", visible_alias = "r")]
    Report(report::args::CmdReport),

    #[clap(
        subcommand,
        about = "Work with mock-machine entries",
        visible_alias = "m"
    )]
    Machine(machine::args::CmdMachine),

    #[clap(
        subcommand,
        about = "Work with machine hardware profiles",
        visible_alias = "p"
    )]
    Profile(profile::args::CmdProfile),

    #[clap(subcommand, about = "Work with site-wide things.", visible_alias = "s")]
    Site(site::args::CmdSite),
}
