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

use async_trait::async_trait;
use mac_address::MacAddress;
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::BootUpdate;
use serde::{Deserialize, Serialize};

use crate::{DriverOutcome, OpCx, PlatformError};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BootInterfaceSelector {
    Mac(MacAddress),
    InterfaceId(String),
    Pair {
        mac_address: MacAddress,
        interface_id: String,
    },
}

/// NICo's normalized boot-order policy status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootOrderStatus {
    pub boot_interface_first: bool,
    pub disk_enabled: bool,
    pub other_network_options_disabled: bool,
}

impl BootOrderStatus {
    pub const fn is_configured(self) -> bool {
        self.boot_interface_first && self.disk_enabled && self.other_network_options_disabled
    }
}

/// One-time boot override and persistent boot-order policy.
#[async_trait]
pub trait BootOrder<B: Bmc>: Send + Sync {
    /// Evaluates the complete boot-order policy for the selected host interface.
    ///
    /// The interface may be a DPU, a DPU in NIC mode, or a conventional NIC.
    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        boot_interface_selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError>;

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Applies the complete boot-order policy for the selected host interface.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        boot_interface_selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_order_is_configured_only_when_every_policy_check_passes() {
        let cases = [
            (
                BootOrderStatus {
                    boot_interface_first: true,
                    disk_enabled: true,
                    other_network_options_disabled: true,
                },
                true,
            ),
            (
                BootOrderStatus {
                    boot_interface_first: false,
                    disk_enabled: true,
                    other_network_options_disabled: true,
                },
                false,
            ),
            (
                BootOrderStatus {
                    boot_interface_first: true,
                    disk_enabled: false,
                    other_network_options_disabled: true,
                },
                false,
            ),
            (
                BootOrderStatus {
                    boot_interface_first: true,
                    disk_enabled: true,
                    other_network_options_disabled: false,
                },
                false,
            ),
        ];

        for (status, expected) in cases {
            assert_eq!(status.is_configured(), expected);
        }
    }
}
