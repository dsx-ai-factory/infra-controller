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
use serde::{Deserialize, Serialize};

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::expected_power_shelf::common::ExpectedPowerShelfJson;

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let json_file_path = Path::new(&self.filename);
        let reader = BufReader::new(File::open(json_file_path)?);

        #[derive(Debug, Serialize, Deserialize)]
        struct ExpectedPowerShelfList {
            expected_power_shelves: Vec<ExpectedPowerShelfJson>,
            expected_power_shelves_count: Option<usize>,
        }

        let expected_power_shelf_list: ExpectedPowerShelfList = serde_json::from_reader(reader)?;

        if expected_power_shelf_list
            .expected_power_shelves_count
            .is_some_and(|count| count != expected_power_shelf_list.expected_power_shelves.len())
        {
            return Err(CarbideCliError::GenericError(format!(
                "Json File specified an invalid count: {:#?}; actual count: {}",
                expected_power_shelf_list
                    .expected_power_shelves_count
                    .unwrap_or_default(),
                expected_power_shelf_list.expected_power_shelves.len()
            )));
        }

        ctx.api_client
            .replace_all_expected_power_shelves(expected_power_shelf_list.expected_power_shelves)
            .await?;
        Ok(())
    }
}
