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
use std::path::Path;

use ::rpc::forge as rpc_forge;
use serde::{Deserialize, Serialize};

use super::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::expected_rack::common::ExpectedRackJson;
use crate::rpc::ApiClient;

#[derive(Debug, Serialize, Deserialize)]
struct ExpectedRackList {
    expected_racks: Vec<ExpectedRackJson>,
    expected_racks_count: Option<usize>,
}

/// replace_all clears all expected racks and replaces them with the contents of a JSON file.
pub(super) async fn replace_all(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let json_file_path = Path::new(&args.filename);
    let reader = BufReader::new(File::open(json_file_path)?);

    let expected_rack_list: ExpectedRackList = serde_json::from_reader(reader)?;

    if expected_rack_list
        .expected_racks_count
        .is_some_and(|count| count != expected_rack_list.expected_racks.len())
    {
        return Err(CarbideCliError::GenericError(format!(
            "Json File specified an invalid count: {:#?}; actual count: {}",
            expected_rack_list.expected_racks_count.unwrap_or_default(),
            expected_rack_list.expected_racks.len()
        )));
    }

    let request = rpc_forge::ExpectedRackList {
        expected_racks: expected_rack_list
            .expected_racks
            .into_iter()
            .map(|rack| rpc_forge::ExpectedRack {
                rack_id: Some(rack.rack_id),
                rack_profile_id: None,
                metadata: rack.metadata,
            })
            .collect(),
    };

    api_client
        .0
        .replace_all_expected_racks(request)
        .await
        .map_err(CarbideCliError::ApiInvocationError)?;
    Ok(())
}
