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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosSettings {
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosDiff {
    pub key: String,
    pub expected: Value,
    pub actual: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosStatus {
    pub is_applied: bool,
    pub differences: Vec<BiosDiff>,
}

/// Current, pending, and desired BIOS configuration operations.
#[async_trait]
pub trait Bios<B: Bmc>: Send + Sync {
    async fn current(&self, cx: &OpCx<'_, B>) -> Result<BiosSettings, PlatformError>;

    async fn pending(&self, cx: &OpCx<'_, B>) -> Result<BiosSettings, PlatformError>;

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        expected: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<BiosStatus, PlatformError>;

    async fn apply(
        &self,
        cx: &OpCx<'_, B>,
        expected: &BiosSettings,
        boot_interface: Option<&BootInterfaceSelector>,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn clear_pending(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn change_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn clear_uefi_password(
        &self,
        cx: &OpCx<'_, B>,
        current_password: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn infinite_boot_enabled(&self, cx: &OpCx<'_, B>) -> Result<Option<bool>, PlatformError>;

    async fn enable_infinite_boot(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn clear_nvram(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;
}
