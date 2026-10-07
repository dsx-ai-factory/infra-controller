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

use std::process::{Command, Stdio};

use tracing;

use crate::firmware::error::{FirmwareError, FirmwareResult};

// DEFAULT_RESET_LEVEL is the default reset level for mlxfwreset, which
// corresponds to a full NIC reset (driver restart + firmware reset).
pub const DEFAULT_RESET_LEVEL: u8 = 3;

// MlxFwResetRunner is a wrapper for executing mlxfwreset commands
// to reset Mellanox NICs after firmware updates.
pub struct MlxFwResetRunner {
    // mlxfwreset_path is the path to the mlxfwreset executable.
    mlxfwreset_path: String,
    // dry_run determines whether to perform dry-run operations.
    dry_run: bool,
}

impl MlxFwResetRunner {
    // new creates a new MlxFwResetRunner instance by discovering
    // the mlxfwreset executable in common locations.
    pub fn new() -> FirmwareResult<Self> {
        let path = Self::find_mlxfwreset()?;
        Ok(Self {
            mlxfwreset_path: path,
            dry_run: false,
        })
    }

    // with_path creates a new MlxFwResetRunner with a custom path
    // to the mlxfwreset executable.
    pub fn with_path(path: impl Into<String>) -> Self {
        Self {
            mlxfwreset_path: path.into(),
            dry_run: false,
        }
    }

    // with_dry_run enables or disables dry-run mode.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    // find_mlxfwreset attempts to find the mlxfwreset executable
    // in common installation locations.
    fn find_mlxfwreset() -> FirmwareResult<String> {
        let common_paths = [
            "mlxfwreset",
            "/usr/bin/mlxfwreset",
            "/usr/local/bin/mlxfwreset",
            "/opt/mellanox/mft/bin/mlxfwreset",
        ];

        for path in &common_paths {
            if let Ok(output) = Command::new(path)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                && output.success()
            {
                return Ok(path.to_string());
            }
        }

        Err(FirmwareError::MlxFwResetNotFound)
    }

    // build_command builds a command string for logging/dry-run purposes.
    fn build_command(&self, args: &[&str]) -> String {
        format!("{} {}", self.mlxfwreset_path, args.join(" "))
    }

    // reset performs a firmware reset on the specified device at the given
    // reset level. The device can be a PCI address (e.g., "4b:00.0") or
    // an MST device path (e.g., "/dev/mst/mt41692_pciconf0"). The level
    // controls the reset severity (3 = full NIC reset).
    // Runs: mlxfwreset --device <dev> --level <n> reset -y
    pub fn reset(&self, device: &str, level: u8) -> FirmwareResult<String> {
        let level_str = level.to_string();
        let args = ["--device", device, "--level", &level_str, "reset", "-y"];

        if self.dry_run {
            return Err(FirmwareError::DryRun(self.build_command(&args)));
        }

        tracing::debug!(command = %self.build_command(&args), "Executing mlxfwreset");

        let output = Command::new(&self.mlxfwreset_path)
            .args(args)
            .output()
            .map_err(|e| {
                FirmwareError::ResetFailed(format!("Failed to execute mlxfwreset: {e}"))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            if stderr.contains("Permission denied") || stdout.contains("Permission denied") {
                return Err(FirmwareError::PermissionDenied);
            }
            if stderr.contains("No such device") || stdout.contains("No such device") {
                return Err(FirmwareError::DeviceNotFound(device.to_string()));
            }
            let error_msg = format!("stdout: {}\nstderr: {}", stdout.trim(), stderr.trim());
            return Err(FirmwareError::ResetFailed(error_msg));
        }

        Ok(stdout)
    }
}

impl Default for MlxFwResetRunner {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self::with_path("mlxfwreset"))
    }
}
