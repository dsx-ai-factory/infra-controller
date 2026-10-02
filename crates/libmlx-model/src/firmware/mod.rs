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

//! Firmware target identity and operation results.

use serde::{Deserialize, Serialize};

pub mod result;

/// Identifies a firmware target by hardware identity and firmware version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareSpec {
    /// Manufacturer part number the firmware is built for, such as `900-9D3B4-00CV-TA0`.
    pub part_number: String,
    /// Parameter-Set IDentification of the firmware configuration, such as `MT_0000000884`.
    pub psid: String,
    /// Target firmware version, such as `32.43.1014`.
    pub version: String,
}

impl FirmwareSpec {
    /// Returns the hardware identity key in `part_number:psid` format.
    pub fn map_key(&self) -> String {
        format!("{}:{}", self.part_number, self.psid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_firmware_spec_map_key() {
        let spec = FirmwareSpec {
            part_number: "900-9D3B4-00CV-TA0".to_string(),
            psid: "MT_0000000884".to_string(),
            version: "32.43.1014".to_string(),
        };
        assert_eq!(spec.map_key(), "900-9D3B4-00CV-TA0:MT_0000000884");
    }
}
