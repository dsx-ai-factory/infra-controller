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

use serde::{Deserialize, Serialize};

/// Power action shared across Switch (NVSwitch) and PowerShelf backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerAction {
    On,
    GracefulShutdown,
    ForceOff,
    GracefulRestart,
    ForceRestart,
    AcPowercycle,
}

/// Firmware update lifecycle state shared across Switch and PowerShelf backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareState {
    Unknown,
    Queued,
    InProgress,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

/// Switch certificate configuration job lifecycle state returned by component-manager backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigureSwitchCertificateState {
    Started,
    InProgress,
    Completed,
    Failed,
}

/// Backend diagnostics for a failed certificate submission or completed job.
/// An empty failure list means RMS supplied no node-specific failures, not success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchCertificateFailure {
    /// Parent (or single-switch child) job ID, absent when submission returned none.
    pub job_id: Option<String>,
    /// Reported node failures and missing child statuses, ordered by node ID and child job ID.
    pub node_failures: Vec<SwitchCertificateNodeFailure>,
}

/// A failed node operation or missing child status; multiple child jobs may refer to the same node.
/// A missing status records unavailable diagnostics, not a confirmed child failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchCertificateNodeFailure {
    /// RMS node identity; an empty value means RMS did not identify the node.
    pub node_id: String,
    /// Child job ID, absent for a rejection before child-job creation.
    pub job_id: Option<String>,
    /// RMS JobError numeric value, including unknown values; absent on submission or missing status.
    pub error_code: Option<i32>,
    /// Error message returned by RMS verbatim, or an explanation that the child status is missing.
    pub message: String,
}

impl std::fmt::Display for SwitchCertificateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "switch certificate failure: job {}",
            self.job_id.as_deref().unwrap_or("unavailable")
        )?;
        if self.node_failures.is_empty() {
            write!(f, "; node failure details unavailable")?;
        }
        for failure in &self.node_failures {
            write!(f, "; node {}", failure.node_id)?;
            if let Some(job_id) = &failure.job_id {
                write!(f, " (job {job_id})")?;
            }
            if let Some(code) = failure.error_code {
                write!(f, " [RMS error {code}]")?;
            }
            write!(f, ": {}", failure.message)?;
        }
        Ok(())
    }
}

impl ConfigureSwitchCertificateState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl std::fmt::Display for ConfigureSwitchCertificateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Started => write!(f, "Started"),
            Self::InProgress => write!(f, "InProgress"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

/// Updatable components of an Switch tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NvSwitchComponent {
    Bmc,
    Cpld,
    Bios,
    Nvos,
}

impl std::fmt::Display for NvSwitchComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bmc => f.write_str("BMC"),
            Self::Cpld => f.write_str("CPLD"),
            Self::Bios => f.write_str("BIOS"),
            Self::Nvos => f.write_str("NVOS"),
        }
    }
}

/// Updatable components of a PowerShelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerShelfComponent {
    Pmc,
    Psu,
}

impl std::fmt::Display for PowerShelfComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pmc => f.write_str("PMC"),
            Self::Psu => f.write_str("PSU"),
        }
    }
}

/// Updatable components of a ComputeTray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputeTrayComponent {
    Bmc,
    Cpld,
    Bios,
    Cx7,
}

impl std::fmt::Display for ComputeTrayComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bmc => f.write_str("BMC"),
            Self::Cpld => f.write_str("CPLD"),
            Self::Bios => f.write_str("BIOS"),
            Self::Cx7 => f.write_str("CX7"),
        }
    }
}
