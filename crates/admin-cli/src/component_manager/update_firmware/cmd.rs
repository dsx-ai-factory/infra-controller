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

use ::rpc::admin_cli::OutputFormat;
use prettytable::{Cell, Row, Table};

use super::args::Args;
use crate::component_manager::common;
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

pub(super) async fn update_firmware(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    let request: rpc::forge::UpdateComponentFirmwareRequest = opts.try_into()?;
    let response = api_client
        .0
        .update_component_firmware(request)
        .await
        .map_err(CarbideCliError::from)?;

    if format == OutputFormat::Json {
        let results = response
            .results
            .iter()
            .map(|result| common::component_result_json(Some(result)))
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        let mut table = Table::new();
        table.set_titles(Row::new(vec![
            Cell::new("Component ID"),
            Cell::new("Status"),
            Cell::new("Error"),
        ]));

        for result in &response.results {
            let (component_id, status, error) = common::component_result_fields(Some(result));
            table.add_row(Row::new(vec![
                Cell::new(&component_id),
                Cell::new(&status),
                Cell::new(&error),
            ]));
        }

        table.printstd();
    }

    let (failures, failure_summary) =
        common::component_failure_count_and_summary(response.results.iter().map(Some));

    if failures > 0 {
        return Err(CarbideCliError::GenericError(format!(
            "{failures} component firmware update request(s) failed{failure_summary}"
        )));
    }

    Ok(())
}
