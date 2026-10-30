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
use tokio::io::AsyncWriteExt;

use super::common::SwitchIdArgs;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Submit a disruptive SDN reset and retain the returned job ID:
    $ nico-admin-cli component-manager reset-switch-sdn-factory-default --switch-id 12345678-1234-5678-90ab-cdef01234567

This dispatches directly through the configured backend, bypassing state-controller maintenance.
It does not restore SDN configuration. RMS must support the SDN reset RPC.
Acceptance does not mean completion; query get-switch-sdn-factory-reset-status.
Partial submission prints the job ID and submission errors, then exits unsuccessfully.
Do not automatically resubmit after an RPC error: the reset may have started.

")]
pub(crate) struct ResetArgs {
    #[clap(flatten)]
    ids: SwitchIdArgs,
    #[clap(
        long,
        help = "NVL certificate-domain selector; omit to use the RMS configured switch domain"
    )]
    domain_name: Option<String>,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Observe a submitted SDN reset:
    $ nico-admin-cli component-manager get-switch-sdn-factory-reset-status --job-id JOB_ID

Pending means a parent or child is not complete. Completed requires all submitted children;
it does not override submission errors returned when the reset was requested.
Failed prints diagnostics and exits unsuccessfully. RMS restart can lose job state;
an unavailable observation does not establish whether the reset ran.

")]
pub(crate) struct StatusArgs {
    #[clap(
        long,
        help = "Opaque parent job ID returned by the SDN reset submission"
    )]
    job_id: String,
}

impl Run for ResetArgs {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let response = ctx
            .api_client
            .0
            .component_reset_switch_sdn_factory_default(
                rpc::forge::ComponentResetSwitchSdnFactoryDefaultRequest {
                    switch_ids: Some(self.ids.into()),
                    domain_name: self.domain_name,
                },
            )
            .await?;
        if ctx.config.format == OutputFormat::Json {
            ctx.output_file.write_all(format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({ "job_id": response.job_id, "submission_errors": response.submission_errors }))?).as_bytes()).await?;
        } else {
            ctx.output_file
                .write_all(format!("Job ID: {}\n", response.job_id).as_bytes())
                .await?;
            for error in &response.submission_errors {
                ctx.output_file
                    .write_all(format!("Submission error: {error}\n").as_bytes())
                    .await?;
            }
        }
        if !response.submission_errors.is_empty() {
            return Err(CarbideCliError::GenericError(
                "some SDN reset targets were rejected; retain the job ID and inspect accepted jobs"
                    .into(),
            ));
        }
        Ok(())
    }
}

impl Run for StatusArgs {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let response = ctx
            .api_client
            .0
            .get_switch_sdn_factory_reset_status(
                rpc::forge::GetSwitchSdnFactoryResetStatusRequest {
                    job_id: self.job_id,
                },
            )
            .await?;
        let state = rpc::forge::SwitchSdnFactoryResetState::try_from(response.state)
            .map_err(|_| CarbideCliError::GenericError("unknown SDN reset state".into()))?;
        if ctx.config.format == OutputFormat::Json {
            ctx.output_file.write_all(format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({ "state": state.as_str_name(), "error": response.error }))?).as_bytes()).await?;
        } else {
            ctx.output_file
                .write_all(format!("State: {}\n", state.as_str_name()).as_bytes())
                .await?;
            if let Some(error) = &response.error {
                ctx.output_file
                    .write_all(format!("Error: {error}\n").as_bytes())
                    .await?;
            }
        }
        match state {
            rpc::forge::SwitchSdnFactoryResetState::Pending
            | rpc::forge::SwitchSdnFactoryResetState::Completed => Ok(()),
            _ => Err(CarbideCliError::GenericError(
                response
                    .error
                    .unwrap_or_else(|| "SDN reset outcome is unknown".into()),
            )),
        }
    }
}

#[cfg(test)]
mod tests;
