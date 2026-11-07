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

/*!
 *  Measured Boot CLI arguments for the `measurement mock-machine` subcommand.
 *
 * This provides the CLI subcommands and arguments for:
 *  - `mock-machine create`: Creates a new "mock" machine.
 *  - `mock-machine delete`: Deletes an existing mock machine.
 *  - `mock-machine attest`: Sends a measurement report for a mock machine.
 *  - `mock-machine show [id]`: Shows detailed info about mock machine(s).
 *  - `mock-machine list``: Lists all mock machines.
 */

use ::rpc::protos::measured_boot::{
    AttestCandidateMachineRequest, ShowCandidateMachineRequest, show_candidate_machine_request,
};
use carbide_uuid::machine::MachineId;
use clap::Parser;
use measured_boot::pcr::PcrRegisterValue;

use crate::cfg::dispatch::Dispatch;
use crate::cfg::measurement::parse_pcr_register_values;
use crate::errors::CarbideCliError;

/// CmdMachine provides a container for the `mock-machine`
/// subcommand, which itself contains other subcommands
/// for working with mock machines.
#[derive(Parser, Debug, Dispatch)]
pub(crate) enum CmdMachine {
    #[clap(about = "Send measurements for a machine.", visible_alias = "a")]
    Attest(Attest),

    #[clap(about = "Get all info about a machine.", visible_alias = "s")]
    Show(Show),

    #[clap(about = "List all machines + their info.", visible_alias = "l")]
    List(List),
}

/// Attest sends a measurement report for the given machine ID,
/// where the measurement report then goes through attestation in an
/// attempt to match a bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Send a measurement report (two PCR values) for a mock machine:
    $ nico-admin-cli attestation measured-boot machine attest \
    12345678-1234-5678-90ab-cdef01234567 0:abc123,7:def456

")]
pub(crate) struct Attest {
    #[clap(help = "The machine ID of the machine to associate this report with.")]
    machine_id: MachineId,

    #[clap(
        required = true,
        use_value_delimiter = true,
        value_delimiter = ',',
        help = "Comma-separated list of {pcr_register:value,...} to associate with this report."
    )]
    #[arg(value_parser = parse_pcr_register_values)]
    values: Vec<PcrRegisterValue>,
}

/// List lists all candidate machines.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all mock machines:
    $ nico-admin-cli attestation measured-boot machine list

")]
pub(crate) struct List {}

/// Show will get a candidate machine for the given ID, or all machines
/// if no machine ID is provided.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show all mock machines:
    $ nico-admin-cli attestation measured-boot machine show

Show one mock machine by ID:
    $ nico-admin-cli attestation measured-boot machine show \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Show {
    #[clap(help = "The machine ID to show.")]
    pub(super) machine_id: Option<MachineId>,
}

impl From<Attest> for AttestCandidateMachineRequest {
    fn from(attest: Attest) -> Self {
        Self {
            machine_id: attest.machine_id.to_string(),
            pcr_values: attest.values.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<Show> for ShowCandidateMachineRequest {
    type Error = CarbideCliError;
    fn try_from(show: Show) -> Result<Self, Self::Error> {
        let machine_id = show
            .machine_id
            .ok_or(CarbideCliError::GenericError(String::from(
                "machine_id must be set to get a machine",
            )))?;
        Ok(Self {
            selector: Some(show_candidate_machine_request::Selector::MachineId(
                machine_id.to_string(),
            )),
        })
    }
}
