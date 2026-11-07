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

/// Default BlueField-2 NIC firmware version.
pub const BF2_NIC_VERSION: &str = "24.47.3576";

/// Default BlueField-2 BMC firmware version.
pub const BF2_BMC_VERSION: &str = "BF-25.10-24";

/// Default BlueField-2 CEC firmware version.
pub const BF2_CEC_VERSION: &str = "4-15";

/// Default BlueField-2 UEFI firmware version.
pub const BF2_UEFI_VERSION: &str = "4.13.2-12-g943a91640d";

/// Default BlueField-3 NIC firmware version.
pub const BF3_NIC_VERSION: &str = "32.47.3576";

/// Default BlueField-3 BMC firmware version.
pub const BF3_BMC_VERSION: &str = "BF-25.10-24";

/// Default BlueField-3 CEC firmware version.
pub const BF3_CEC_VERSION: &str = "00.02.0195.0000_n02";

/// Default BlueField-3 UEFI firmware version.
pub const BF3_UEFI_VERSION: &str = "4.13.2-12-g943a91640d";

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum BlueFieldVersion {
        Bf2Nic,
        Bf2Bmc,
        Bf2Cec,
        Bf2Uefi,
        Bf3Nic,
        Bf3Bmc,
        Bf3Cec,
        Bf3Uefi,
    }

    fn default_version(version: BlueFieldVersion) -> &'static str {
        match version {
            BlueFieldVersion::Bf2Nic => BF2_NIC_VERSION,
            BlueFieldVersion::Bf2Bmc => BF2_BMC_VERSION,
            BlueFieldVersion::Bf2Cec => BF2_CEC_VERSION,
            BlueFieldVersion::Bf2Uefi => BF2_UEFI_VERSION,
            BlueFieldVersion::Bf3Nic => BF3_NIC_VERSION,
            BlueFieldVersion::Bf3Bmc => BF3_BMC_VERSION,
            BlueFieldVersion::Bf3Cec => BF3_CEC_VERSION,
            BlueFieldVersion::Bf3Uefi => BF3_UEFI_VERSION,
        }
    }

    #[test]
    fn returns_default_bluefield_versions() {
        // BF2 and BF3 currently share BMC and UEFI payload versions.
        value_scenarios!(default_version:
            "bluefield 2" {
                BlueFieldVersion::Bf2Nic => "24.47.3576",
                BlueFieldVersion::Bf2Bmc => "BF-25.10-24",
                BlueFieldVersion::Bf2Cec => "4-15",
                BlueFieldVersion::Bf2Uefi => "4.13.2-12-g943a91640d",
            }

            "bluefield 3" {
                BlueFieldVersion::Bf3Nic => "32.47.3576",
                BlueFieldVersion::Bf3Bmc => "BF-25.10-24",
                BlueFieldVersion::Bf3Cec => "00.02.0195.0000_n02",
                BlueFieldVersion::Bf3Uefi => "4.13.2-12-g943a91640d",
            }
        );
    }
}
