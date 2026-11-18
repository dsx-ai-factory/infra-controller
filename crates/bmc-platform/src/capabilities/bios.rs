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

use std::collections::BTreeMap;

use async_trait::async_trait;
use nv_redfish::core::Bmc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BootInterfaceSelector, DriverOutcome, OpCx, PlatformError};

/// BIOS attribute names and values.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosSettings {
    /// BIOS attribute values by attribute name; nulls are omitted.
    pub attributes: BTreeMap<String, Value>,
}

/// One BIOS attribute whose reported value differs from the expectation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosDiff {
    /// Attribute name.
    pub key: String,
    /// Value NICo wants.
    pub expected: Value,
    /// Value the BMC reports; `None` when the BMC does not expose the attribute.
    pub actual: Option<Value>,
}

/// Whether expected BIOS settings are applied, with every difference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosStatus {
    /// True when `differences` is empty.
    pub is_applied: bool,
    /// Expected attributes whose current value differs.
    pub differences: Vec<BiosDiff>,
}

/// BIOS setup for machine provisioning.
///
/// Each driver knows the BIOS settings its platform needs; callers pass only
/// their own profile for the model, which takes precedence. Every operation
/// defaults to delegating to [`Self::standard`], except
/// [`Self::clear_uefi_password`], which goes through the driver's own
/// [`Self::change_uefi_password`]; a driver implements only the operations its
/// platform deviates on.
#[async_trait]
pub trait Bios<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn Bios<B>;

    /// Stages the platform's BIOS settings and `profile` for the next reset,
    /// including infinite boot on platforms that have the setting.
    ///
    /// `boot_interface` is the host interface boot order setup will put first,
    /// or `None` on a host without one; platforms whose boot order reads BIOS
    /// settings stage them here so the reset that applies them precedes boot
    /// order setup.
    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().apply(cx, profile, boot_interface).await
    }

    /// Whether the platform's BIOS settings and `profile` are in effect for
    /// `boot_interface`, with every difference.
    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        profile: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError> {
        self.standard().status(cx, profile, boot_interface).await
    }

    /// Restores the BIOS defaults.
    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().reset(cx).await
    }

    /// Discards settings staged for the next reset.
    async fn clear_pending(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().clear_pending(cx).await
    }

    /// Changes the UEFI administrator password; the driver knows the BIOS's password slot.
    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .change_uefi_password(cx, current_password, new_password)
            .await
    }

    /// Clears the UEFI administrator password by changing it to an empty one.
    async fn clear_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.change_uefi_password(cx, current_password, "").await
    }

    /// Requests that the BIOS clear the TPM on the next boot.
    async fn clear_tpm(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().clear_tpm(cx).await
    }

    /// Requests that the BIOS reset its NVRAM, which takes effect on the next
    /// boot.
    async fn clear_nvram(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().clear_nvram(cx).await
    }

    /// Whether the BIOS retries booting indefinitely; `None` when the platform
    /// has no such setting or the BIOS does not report it.
    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError> {
        self.standard().infinite_boot_enabled(cx).await
    }
}
