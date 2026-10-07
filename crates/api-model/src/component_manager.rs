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
