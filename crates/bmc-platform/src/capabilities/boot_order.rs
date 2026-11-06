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

/// Identifies the host interface whose boot option should be first.
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

/// Whether the boot order is configured for the selected interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootOrderStatus {
    /// The selected interface's HTTP boot option is first in the boot order.
    pub boot_interface_first: bool,
    /// The disks remain in the boot order; `None` on platforms whose boot
    /// order setup does not manage disks.
    pub disk_enabled: Option<bool>,
    /// The other network boot options are disabled; `None` on platforms whose
    /// boot order setup leaves them alone.
    pub other_network_options_disabled: Option<bool>,
}

impl BootOrderStatus {
    /// True when the boot interface is first and no managed condition fails.
    pub const fn is_configured(self) -> bool {
        self.boot_interface_first
            && !matches!(self.disk_enabled, Some(false))
            && !matches!(self.other_network_options_disabled, Some(false))
    }
}

/// One-time boot override and persistent boot-order policy.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait BootOrder<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn BootOrder<B>;

    /// Whether the selected host interface boots first.
    ///
    /// The interface may be a DPU, a DPU in NIC mode, or a conventional NIC.
    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        boot_interface_selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        self.standard().status(cx, boot_interface_selector).await
    }

    /// Boots the override target once (`Once`) or from now on (`Continuous`).
    ///
    /// A `Continuous` network, HTTP or disk target with no mode or HTTP boot
    /// URI moves that device first in the persistent boot order on platforms
    /// that order boot devices rather than honor a continuous override. An
    /// HTTP boot URI pins where a UEFI HTTP boot loads from.
    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().set_override(cx, override_setting).await
    }

    /// Moves the selected host interface's HTTP boot option first in the
    /// persistent boot order.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        boot_interface_selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().configure(cx, boot_interface_selector).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_order_is_configured_unless_a_managed_check_fails() {
        let status = |first, disk, network| BootOrderStatus {
            boot_interface_first: first,
            disk_enabled: disk,
            other_network_options_disabled: network,
        };
        let cases = [
            (status(true, Some(true), Some(true)), true),
            (status(true, None, None), true),
            (status(false, None, None), false),
            (status(true, Some(false), None), false),
            (status(true, None, Some(false)), false),
        ];

        for (status, expected) in cases {
            assert_eq!(status.is_configured(), expected, "{status:?}");
        }
    }
}
