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

mod args;
mod cmd;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub(super) use args::Args;
use serde::Deserialize;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::expected_machines::common::ExpectedMachineJson;

/// `expected-machine replace-all`: reads a JSON list and calls `replace_all_expected_machines`.
/// Each `ExpectedMachineJson` may include `bmc_ip_address`; the API runs the same pre-allocation
/// path as `add_expected_machine` for each entry.
impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let json_file_path = Path::new(&self.filename);
        let reader = BufReader::new(File::open(json_file_path)?);
        #[derive(Debug, Deserialize)]
        struct ExpectedMachineList {
            expected_machines: Vec<ExpectedMachineJson>,
            expected_machines_count: Option<usize>,
        }
        let expected_machine_list: ExpectedMachineList = serde_json::from_reader(reader)?;

        if expected_machine_list
            .expected_machines_count
            .is_some_and(|c| c != expected_machine_list.expected_machines.len())
        {
            eprintln!("WARNING: expected_machines_count does not match actual number of entries");
        }

        ctx.api_client
            .replace_all_expected_machines(expected_machine_list.expected_machines)
            .await?;
        Ok(())
    }
}
