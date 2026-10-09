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
use carbide_uuid::machine::MachineId;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::health_utils;
use crate::rpc::ApiClient;

pub(super) async fn handle_health_report(
    command: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    match command {
        Args::Show { dpu_id } => {
            ensure_dpu_id(dpu_id)?;
            let response = api_client.machine_list_health_reports(dpu_id).await?;
            health_utils::display_health_reports(response.health_report_entries, output_format)?;
        }
        Args::Add(options) => {
            ensure_dpu_id(options.dpu_id)?;
            let report = health_utils::resolve_health_report(
                options.template,
                options.health_report,
                options.message,
            )?;

            if options.print_only {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }

            api_client
                .machine_insert_health_report_override(
                    &options.dpu_id,
                    report.into(),
                    options.replace,
                )
                .await?;
        }
        Args::Remove {
            dpu_id,
            report_source,
        } => {
            ensure_dpu_id(dpu_id)?;
            api_client
                .machine_remove_health_report(dpu_id, report_source)
                .await?;
        }
        Args::PrintEmptyTemplate => {
            health_utils::print_empty_template();
        }
    }

    Ok(())
}

fn ensure_dpu_id(dpu_id: MachineId) -> CarbideCliResult<()> {
    if dpu_id.machine_type().is_dpu() {
        Ok(())
    } else {
        Err(CarbideCliError::GenericError(format!(
            "{dpu_id} is not a DPU machine ID"
        )))
    }
}
