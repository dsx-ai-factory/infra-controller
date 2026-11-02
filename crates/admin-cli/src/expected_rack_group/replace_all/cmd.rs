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

use std::fs::File;
use std::io::BufReader;

use color_eyre::eyre::WrapErr;
use serde::Deserialize;

use super::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::expected_rack_group::common::ExpectedRackGroupJson;
use crate::rpc::ApiClient;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedRackGroupList {
    expected_rack_groups: Vec<ExpectedRackGroupJson>,
    expected_rack_groups_count: Option<usize>,
}

pub(super) async fn replace_all(args: Args, client: &ApiClient) -> CarbideCliResult<()> {
    let file = File::open(&args.filename).wrap_err_with(|| format!("opening {}", args.filename))?;
    let input: ExpectedRackGroupList = serde_json::from_reader(BufReader::new(file))
        .wrap_err("reading expected rack group inventory JSON")?;
    if let Some(count) = input.expected_rack_groups_count
        && count != input.expected_rack_groups.len()
    {
        return Err(CarbideCliError::GenericError(format!(
            "expected_rack_groups_count is {count}, but the array contains {} groups",
            input.expected_rack_groups.len()
        )));
    }
    client
        .0
        .replace_all_expected_rack_groups(rpc::forge::ExpectedRackGroupList {
            expected_rack_groups: input
                .expected_rack_groups
                .into_iter()
                .map(Into::into)
                .collect(),
        })
        .await
        .wrap_err("replacing expected rack group inventory")?;
    Ok(())
}
