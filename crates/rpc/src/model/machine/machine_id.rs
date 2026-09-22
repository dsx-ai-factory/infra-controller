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

// use std::fmt::Write;
use std::str::FromStr;

use carbide_uuid::machine::MachineId;

use crate::errors::RpcDataConversionError;

/// Converts a RPC MachineId into the internal data format
pub fn try_parse_machine_id(id: &str) -> Result<MachineId, RpcDataConversionError> {
    MachineId::from_str(id).map_err(|_| RpcDataConversionError::InvalidMachineId(id.to_string()))
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::DpuMachineId;

    use super::*;

    #[test]
    fn validate_remote_id() {
        let dpu_id = DpuMachineId::try_from(
            try_parse_machine_id("fm100dsg4ekcb4sdi6hkqn0iojhj18okrr8vct64luh8957lfe8e69vme20")
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            "d33nk2ne8p59qr988hssbc84gb2b0s34vcq5j7pm5jnrbnhc6880",
            dpu_id.remote_id()
        );
    }
}
