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
use clap::Parser;
use eyre::WrapErr;
use prettytable::{Cell, Row, Table};

use crate::async_writeln;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::component_manager::common::{timestamp_json, timestamp_string};
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug)]
#[command(
    long_about = "Inspect an RMS job through the RMS backend configured in NICo. This diagnostic command requires the admin CLI role. It prints the requested job and optionally its child records, including state, errors, result JSON, device identity, timestamps, and parent/child references. A successful query exits successfully even when the reported job failed. Job expiry or RMS restart can make records unavailable. Component reset status is normally queried by device using component-manager get-reset-status.",
    after_long_help = "\
EXAMPLES:

Inspect an RMS job through NICo:
    $ nico-admin-cli backend rms get-job-status --job-id 12345678-1234-5678-90ab-cdef01234567

Include child job records as JSON:
    $ nico-admin-cli -f json backend rms get-job-status \
    --job-id 12345678-1234-5678-90ab-cdef01234567 --include-child-job-states

"
)]
pub(crate) struct Args {
    #[clap(long, help = "RMS job identifier to inspect")]
    job_id: String,
    #[clap(
        long,
        help = "Include child job records; omitted returns only the requested job"
    )]
    include_child_job_states: bool,
}

fn state_name(state: i32) -> &'static str {
    rpc::forge::RmsJobExecutionState::try_from(state)
        .map(|state| state.as_str_name())
        .unwrap_or("UNKNOWN")
}

fn error_name(error: i32) -> &'static str {
    rpc::forge::RmsJobError::try_from(error)
        .map(|error| error.as_str_name())
        .unwrap_or("UNKNOWN")
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let response = ctx
            .api_client
            .0
            .get_rms_job_status(rpc::forge::GetRmsJobStatusRequest {
                job_id: self.job_id,
                include_child_job_states: self.include_child_job_states,
            })
            .await
            .context("get RMS job status through NICo")?;
        if ctx.config.format == OutputFormat::Json {
            let jobs: Vec<_> = response
                .job_states
                .iter()
                .map(|job| {
                    serde_json::json!({
                        "job_id": job.job_id,
                        "parent_job_id": job.parent_job_id,
                        "child_job_ids": job.child_job_ids,
                        "execution_state": state_name(job.execution_state),
                        "execution_state_code": job.execution_state,
                        "error_message": job.error_message,
                        "error_code": job.error_code,
                        "error_name": error_name(job.error_code),
                        "result_json": job.result_json,
                        "state_description": job.state_description,
                        "rack_id": job.rack_id,
                        "node_id": job.node_id,
                        "created_at": timestamp_json(job.created_at.as_ref()),
                        "updated_at": timestamp_json(job.updated_at.as_ref()),
                    })
                })
                .collect();
            async_writeln!(
                ctx.output_file,
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"job_states": jobs}))
                    .context("serialize RMS job status")?
            )
            .context("write RMS job status")?;
        } else {
            for job in response.job_states {
                let fields = [
                    ("Job ID", job.job_id),
                    ("Parent Job ID", job.parent_job_id.unwrap_or_default()),
                    ("Child Job IDs", job.child_job_ids.join(", ")),
                    (
                        "State",
                        format!(
                            "{} ({})",
                            state_name(job.execution_state),
                            job.execution_state
                        ),
                    ),
                    ("Description", job.state_description),
                    (
                        "Error Code",
                        format!("{} ({})", error_name(job.error_code), job.error_code),
                    ),
                    ("Error", job.error_message),
                    ("Result JSON", job.result_json),
                    ("Rack ID", job.rack_id.unwrap_or_default()),
                    ("Node ID", job.node_id.unwrap_or_default()),
                    ("Created At", timestamp_string(job.created_at.as_ref())),
                    ("Updated At", timestamp_string(job.updated_at.as_ref())),
                ];
                let mut table = Table::new();
                table.set_titles(Row::new(vec![Cell::new("Field"), Cell::new("Value")]));
                for (field, value) in fields {
                    table.add_row(Row::new(vec![
                        Cell::new(field),
                        Cell::new(if value.is_empty() { "-" } else { &value }),
                    ]));
                }
                async_writeln!(ctx.output_file, "{table}").context("write RMS job status")?;
            }
        }
        Ok(())
    }
}
