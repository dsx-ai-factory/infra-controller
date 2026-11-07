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

use crate::lockdown::cmd::args::{Cli, Commands, LockdownAction, OutputFormat};
use crate::lockdown::error::{MlxError, MlxResult};
use crate::lockdown::lockdown::{LockStatus, LockdownManager, StatusReport};

// run_cli is the main CLI entry point that dispatches to appropriate handlers.
pub fn run_cli(cli: Cli) -> MlxResult<()> {
    match cli.command {
        Commands::Lockdown { action } => handle_lockdown(action)?,
    }

    Ok(())
}

// handle_lockdown handles lockdown subcommands.
pub fn handle_lockdown(action: LockdownAction) -> MlxResult<()> {
    match action {
        LockdownAction::Lock {
            device_id,
            key,
            format,
            dry_run,
        } => {
            let manager = LockdownManager::with_dry_run(dry_run)?;
            match manager.lock_device(&device_id, &key) {
                Ok(status) => {
                    print_status(&device_id, status, format)?;
                }
                Err(MlxError::AlreadyLocked) => {
                    return Err(MlxError::AlreadyLocked);
                }
                Err(MlxError::DryRun(cmd)) => {
                    println!("Would have executed: {cmd}");
                }
                Err(e) => return Err(e),
            }
        }
        LockdownAction::Unlock {
            device_id,
            key,
            format,
            dry_run,
        } => {
            let manager = LockdownManager::with_dry_run(dry_run)?;
            match manager.unlock_device(&device_id, &key) {
                Ok(status) => {
                    print_status(&device_id, status, format)?;
                }
                Err(MlxError::AlreadyUnlocked) => {
                    return Err(MlxError::AlreadyUnlocked);
                }
                Err(MlxError::DryRun(cmd)) => {
                    println!("Would have executed: {cmd}");
                }
                Err(e) => return Err(e),
            }
        }
        LockdownAction::Status {
            device_id,
            format,
            dry_run,
        } => {
            let manager = LockdownManager::with_dry_run(dry_run)?;
            match manager.get_status(&device_id) {
                Ok(status) => {
                    print_status(&device_id, status, format)?;
                }
                Err(MlxError::DryRun(cmd)) => {
                    println!("Would have executed: {cmd}");
                }
                Err(e) => return Err(e),
            }
        }
        LockdownAction::SetKey {
            device_id,
            key,
            format,
            dry_run,
        } => {
            let manager = LockdownManager::with_dry_run(dry_run)?;
            match manager.set_device_key(&device_id, &key) {
                Ok(()) => {
                    if !dry_run {
                        let status = manager.get_status(&device_id)?;
                        print_status(&device_id, status, format)?;
                    }
                }
                Err(MlxError::DryRun(cmd)) => {
                    println!("Would have executed: {cmd}");
                }
                Err(e) => return Err(e),
            }
        }
    }

    Ok(())
}

// print_status prints the device status in the specified format.
fn print_status(device_id: &str, status: LockStatus, format: OutputFormat) -> MlxResult<()> {
    match format {
        OutputFormat::Text => {
            println!("status: {status}");
        }
        OutputFormat::Json => {
            let report = StatusReport::new(device_id.to_string(), status);
            println!("{}", report.to_json()?);
        }
        OutputFormat::Yaml => {
            let report = StatusReport::new(device_id.to_string(), status);
            println!("{}", report.to_yaml()?);
        }
    }
    Ok(())
}
