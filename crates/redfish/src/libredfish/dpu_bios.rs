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

use libredfish::RedfishError;

const DPU_BIOS_ATTRIBUTE_KEYS: [&str; 4] = [
    "HostPrivilegeLevel",
    "Host Privilege Level",
    "InternalCPUModel",
    "Internal CPU Model",
];

/// Returns true when a DPU BMC BIOS response is missing attributes that are expected
/// once UEFI POST has finished. This is a known race between UEFI POST and the BMC;
/// force-restarting the DPU usually resolves it.
pub fn is_dpu_bios_attributes_not_ready(error: &RedfishError) -> bool {
    match error {
        RedfishError::MissingKey { key, url } => {
            url.contains("Bios") && DPU_BIOS_ATTRIBUTE_KEYS.contains(&key.as_str())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_host_privilege_level() {
        let error = RedfishError::MissingKey {
            key: "HostPrivilegeLevel".to_string(),
            url: "Systems/{}/Bios".to_string(),
        };
        assert!(is_dpu_bios_attributes_not_ready(&error));
    }

    #[test]
    fn ignores_missing_keys_outside_bios() {
        let error = RedfishError::MissingKey {
            key: "HostPrivilegeLevel".to_string(),
            url: "Systems/{}/BootOptions".to_string(),
        };
        assert!(!is_dpu_bios_attributes_not_ready(&error));
    }
}
