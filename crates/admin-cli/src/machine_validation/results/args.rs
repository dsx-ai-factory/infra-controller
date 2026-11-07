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

use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use clap::{ArgGroup, Parser};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show validation results for a machine:
    $ nico-admin-cli machine-validation results show --machine 12345678-1234-5678-90ab-cdef01234567

Show results for a specific validation run:
    $ nico-admin-cli machine-validation results show --validation-id 12345678-1234-5678-90ab-cdef01234567

Show one test's result within a run (with history):
    $ nico-admin-cli machine-validation results show --validation-id 12345678-1234-5678-90ab-cdef01234567 \
    --test-name gpu_bandwidth --history

")]
pub(crate) enum Args {
    #[clap(about = "Show results")]
    Show(ShowResultsOptions),
}

#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&[
    "validation_id",
    "test_name",
    "machine",
    ])))]
pub(crate) struct ShowResultsOptions {
    #[clap(
        short = 'm',
        long,
        group = "group",
        help = "Show machine validation result of a machine"
    )]
    pub(super) machine: Option<MachineId>,

    #[clap(short = 'v', long, group = "group", help = "Machine validation id")]
    pub(super) validation_id: Option<MachineValidationId>,

    #[clap(
        short = 't',
        long,
        group = "group",
        requires("validation_id"),
        help = "Name of the test case"
    )]
    pub(super) test_name: Option<String>,

    #[clap(long, default_value = "false", help = "Results history")]
    pub(super) history: bool,
}
