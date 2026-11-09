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
use nv_redfish::core::Bmc;
use nv_redfish::schema::secure_boot::SecureBootUpdate;
use serde::{Deserialize, Serialize};

use crate::{DriverOutcome, OpCx, PlatformError};

/// NICo's normalized view of configured and active Secure Boot state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecureBootStatus {
    Enabled,
    Disabled,
    Pending,
}

/// Secure Boot status, enablement, and certificate operations.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait SecureBoot<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn SecureBoot<B>;

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<SecureBootStatus, PlatformError> {
        self.standard().status(cx).await
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        update: &SecureBootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().set(cx, update).await
    }

    async fn has_platform_key(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        self.standard().has_platform_key(cx).await
    }

    async fn add_platform_key(
        &self,
        cx: &OpCx<'_, B>,
        pem: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().add_platform_key(cx, pem).await
    }
}
