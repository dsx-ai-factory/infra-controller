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

use std::str::FromStr;

#[derive(Debug)]
pub struct UnsupportedCpuArchitecture(pub String);

impl std::error::Error for UnsupportedCpuArchitecture {}

impl std::fmt::Display for UnsupportedCpuArchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "CPU architecture '{}' is not supported", self.0)
    }
}

#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum CpuArchitecture {
    Aarch64,
    X86_64,
    // For predicated hosts we don't know yet
    #[default]
    #[serde(rename = "")]
    Unknown,
}

impl std::fmt::Display for CpuArchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use CpuArchitecture::*;
        let s = match self {
            Aarch64 => "aarch64",
            X86_64 => "x86_64",
            _ => "",
        };
        write!(f, "{s}")
    }
}

impl FromStr for CpuArchitecture {
    type Err = UnsupportedCpuArchitecture;

    // Convert from `uname` output
    // Not used to convert from DB or JSON. That's the derived serde::Deserialize.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let arch = match s {
            "aarch64" => Ok(CpuArchitecture::Aarch64),
            "x86_64" => Ok(CpuArchitecture::X86_64),
            "" => Ok(CpuArchitecture::Unknown), // Predicted hosts
            _ => Err(UnsupportedCpuArchitecture(s.to_string())),
        }?;
        Ok(arch)
    }
}
