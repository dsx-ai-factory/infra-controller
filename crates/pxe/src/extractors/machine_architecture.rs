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

use ::rpc::forge as rpc;
use serde::{Deserialize, Serialize};

use crate::rpc_error::PxeRequestError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum MachineArchitecture {
    Arm = 0,
    X86 = 1,
}

impl TryFrom<&str> for MachineArchitecture {
    type Error = PxeRequestError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "arm64" => Ok(MachineArchitecture::Arm),
            "x86_64" => Ok(MachineArchitecture::X86),
            x if x == (MachineArchitecture::Arm as u64).to_string().as_str() => {
                Ok(MachineArchitecture::Arm)
            }
            x if x == (MachineArchitecture::X86 as u64).to_string().as_str() => {
                Ok(MachineArchitecture::X86)
            }
            _ => Err(PxeRequestError::MalformedBuildArch(format!(
                "Not a valid architecture identifier: {value}"
            ))),
        }
    }
}

impl From<MachineArchitecture> for rpc::MachineArchitecture {
    fn from(arch: MachineArchitecture) -> rpc::MachineArchitecture {
        match arch {
            MachineArchitecture::X86 => rpc::MachineArchitecture::X86,
            MachineArchitecture::Arm => rpc::MachineArchitecture::Arm,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    fn parse_arch(value: &str) -> Result<&'static str, &'static str> {
        MachineArchitecture::try_from(value)
            .map(|arch| match arch {
                MachineArchitecture::Arm => "arm",
                MachineArchitecture::X86 => "x86",
            })
            .map_err(|error| match error {
                PxeRequestError::MalformedBuildArch(_) => "malformed-build-arch",
                _ => "unexpected",
            })
    }

    #[test]
    fn parses_machine_architecture_identifiers() {
        scenarios!(parse_arch:
            "named identifiers" {
                "arm64" => Yields("arm"),
                "x86_64" => Yields("x86"),
            }

            "numeric identifiers" {
                "0" => Yields("arm"),
                "1" => Yields("x86"),
            }

            "invalid identifiers" {
                "" => FailsWith("malformed-build-arch"),
                "amd64" => FailsWith("malformed-build-arch"),
                "2" => FailsWith("malformed-build-arch"),
            }
        );
    }

    #[test]
    fn converts_machine_architecture_to_rpc_enum() {
        value_scenarios!(
            run = |arch| rpc::MachineArchitecture::from(arch) as i32;
            "rpc enum" {
                MachineArchitecture::Arm => rpc::MachineArchitecture::Arm as i32,
                MachineArchitecture::X86 => rpc::MachineArchitecture::X86 as i32,
            }
        );
    }
}
