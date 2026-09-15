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
#[async_trait]
pub trait SecureBoot<B: Bmc>: Send + Sync {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<SecureBootStatus, PlatformError>;

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        update: &SecureBootUpdate,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn has_platform_key(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError>;

    async fn add_platform_key(
        &self,
        cx: &OpCx<'_, B>,
        pem: &str,
    ) -> Result<DriverOutcome, PlatformError>;
}
