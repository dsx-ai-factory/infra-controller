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

// src/applier.rs
// Handles mlxconfig apply and reset operations that work at the device
// level rather than individual variables. This is separate from
// MlxConfigRunner because these commands don't require a variable
// registry.

use std::path::Path;

use crate::runner::command_builder::CommandSpec;
use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::ExecOptions;
use crate::runner::executor::CommandExecutor;

// MlxConfigApplier handles mlxconfig apply and reset operations.
pub struct MlxConfigApplier {
    // device is the device identifier (e.g., "4b:00.0").
    device: String,
    // options contains the execution options controlling retry,
    // timeout, dry-run, and verbose behavior.
    options: ExecOptions,
}

impl MlxConfigApplier {
    // new creates a new MlxConfigApplier for the specified device
    // with default execution options.
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            options: ExecOptions::default(),
        }
    }

    // with_options creates a new MlxConfigApplier for the specified
    // device with custom execution options.
    pub fn with_options(device: impl Into<String>, options: ExecOptions) -> Self {
        Self {
            device: device.into(),
            options,
        }
    }

    // apply applies a binary configuration file to the device. This is
    // used for operations like applying debug tokens before flashing
    // debug firmware. The config file must have been created via
    // `mlxconfig create_conf`. Runs: mlxconfig -d <dev> --yes apply <file>
    pub fn apply(&self, config_file: &Path) -> Result<(), MlxRunnerError> {
        if !config_file.exists() {
            return Err(MlxRunnerError::GenericError(format!(
                "Configuration file does not exist: {}",
                config_file.display()
            )));
        }

        let spec = self.apply_command(config_file);

        let executor = CommandExecutor {
            options: &self.options,
        };

        if self.options.verbose {
            println!("[applier] {spec}");
        }

        if executor.is_dry_run() {
            executor.execute_dry_run(&spec, "apply");
            return Ok(());
        }

        executor.execute_with_retry(&spec)?;
        Ok(())
    }

    pub(crate) fn apply_command(&self, config_file: &Path) -> CommandSpec {
        CommandSpec::new("mlxconfig")
            .arg("-d")
            .arg(&self.device)
            .arg("--yes")
            .arg("apply")
            .arg(config_file.to_string_lossy())
    }

    // reset_config resets all mlxconfig configurations on the device
    // to their default values. This is a factory reset of NV configuration
    // parameters, NOT a device reset (use mlxfwreset for that).
    // Runs: mlxconfig -d <dev> --yes reset
    pub fn reset_config(&self) -> Result<(), MlxRunnerError> {
        let spec = CommandSpec::new("mlxconfig")
            .arg("-d")
            .arg(&self.device)
            .arg("--yes")
            .arg("reset");

        let executor = CommandExecutor {
            options: &self.options,
        };

        if self.options.verbose {
            println!("[applier] {spec}");
        }

        if executor.is_dry_run() {
            executor.execute_dry_run(&spec, "reset");
            return Ok(());
        }

        executor.execute_with_retry(&spec)?;
        Ok(())
    }
}
