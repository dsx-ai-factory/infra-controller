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

fn ensure_firmware_status_succeeded(
    statuses: &[::rpc::forge::FirmwareUpdateStatus],
) -> Result<(), CarbideCliError> {
    let (failures, failure_summary) = common::component_failure_count_and_summary(
        statuses.iter().map(|status| status.result.as_ref()),
    );

    if failures > 0 {
        return Err(CarbideCliError::GenericError(format!(
            "{failures} component firmware status result(s) failed{failure_summary}"
        )));
    }

    Ok(())
}

pub(super) async fn get_status(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    let response = api_client
        .0
        .get_component_firmware_status(opts)
        .await
        .map_err(CarbideCliError::from)?;

    if format == OutputFormat::Json {
        let statuses = response
            .statuses
            .iter()
            .map(|status| {
                serde_json::json!({
                    "result": common::component_result_json(status.result.as_ref()),
                    "state": common::firmware_state_name(status.state),
                    "state_code": status.state,
                    "target_version": status.target_version,
                    "updated_at": common::timestamp_json(status.updated_at.as_ref()),
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&statuses)?);
    } else {
        let mut table = Table::new();
        table.set_titles(Row::new(vec![
            Cell::new("Component ID"),
            Cell::new("Result"),
            Cell::new("State"),
            Cell::new("Target Version"),
            Cell::new("Updated At"),
            Cell::new("Error"),
        ]));

        for status in &response.statuses {
            let (component_id, result_status, error) =
                common::component_result_fields(status.result.as_ref());
            let updated_at = common::timestamp_string(status.updated_at.as_ref());
            table.add_row(Row::new(vec![
                Cell::new(&component_id),
                Cell::new(&result_status),
                Cell::new(common::firmware_state_name(status.state)),
                Cell::new(&common::display_or_dash(&status.target_version)),
                Cell::new(&updated_at),
                Cell::new(&error),
            ]));
        }

        table.printstd();
    }

    ensure_firmware_status_succeeded(&response.statuses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_firmware_status_returns_a_cli_error_with_the_backend_reason() {
        let statuses = [::rpc::forge::FirmwareUpdateStatus {
            result: Some(::rpc::forge::ComponentResult {
                component_id: Some("rack-1".into()),
                status: ::rpc::forge::ComponentManagerStatusCode::InternalError as i32,
                error: "rack firmware failed".into(),
                mac_address: None,
            }),
            state: ::rpc::forge::FirmwareUpdateState::FwStateFailed as i32,
            target_version: "fw-42".into(),
            updated_at: None,
        }];

        let error = ensure_firmware_status_succeeded(&statuses)
            .expect_err("a failed component result must make the command fail");

        let CarbideCliError::GenericError(message) = error else {
            panic!("firmware status failures must return GenericError");
        };

        assert_eq!(
            message,
            "1 component firmware status result(s) failed: rack-1=internal-error(2): rack firmware failed"
        );
    }
}
