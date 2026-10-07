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

// lockdown/cmds.rs
// Command handlers for lockdown operations.

use libmlx::lockdown::lockdown::StatusReport;
use rpc::admin_cli::OutputFormat;
use rpc::protos::mlx_device as mlx_device_pb;

use super::args::{LockdownLockCommand, LockdownStatusCommand, LockdownUnlockCommand};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

// handle_lock locks a device on a machine.
impl Run for LockdownLockCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminLockdownLockRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_lockdown_lock(request).await?;

        let status_report_pb = response.status_report.ok_or_else(|| {
            CarbideCliError::GenericError("no status report returned".to_string())
        })?;

        print_lockdown_response(status_report_pb.into(), &ctx.config.format)?;
        Ok(())
    }
}

// handle_unlock unlocks a device on a machine.
impl Run for LockdownUnlockCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminLockdownUnlockRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_lockdown_unlock(request).await?;

        let status_report_pb = response.status_report.ok_or_else(|| {
            CarbideCliError::GenericError("no status report returned".to_string())
        })?;

        print_lockdown_response(status_report_pb.into(), &ctx.config.format)?;
        Ok(())
    }
}

// handle_status gets the lock status of a device on a machine.
impl Run for LockdownStatusCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminLockdownStatusRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_lockdown_status(request).await?;

        let status_report_pb = response.status_report.ok_or_else(|| {
            CarbideCliError::GenericError("no status report returned".to_string())
        })?;

        print_lockdown_response(status_report_pb.into(), &ctx.config.format)?;
        Ok(())
    }
}

// print_lockdown_response prints the lockdown response in the specified format.
fn print_lockdown_response(
    status_report: StatusReport,
    format: &OutputFormat,
) -> CarbideCliResult<()> {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&status_report)?);
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&status_report)?);
        }
        OutputFormat::AsciiTable => {
            println!(
                "Device {}: {}",
                status_report.device_id, status_report.status
            );
        }
        OutputFormat::Csv => {
            println!("{},{}", status_report.device_id, status_report.status);
        }
    }
    Ok(())
}
