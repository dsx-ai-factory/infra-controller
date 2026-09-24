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

use crate::{DriverOutcome, OpCx, PlatformError};

/// BMC reset, factory-default, and time-configuration mutations.
#[async_trait]
pub trait BmcControl<B: Bmc>: Send + Sync {
    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn reset_to_factory_defaults(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn set_ntp_servers(
        &self,
        cx: &OpCx<'_, B>,
        servers: &[String],
    ) -> Result<DriverOutcome, PlatformError>;

    async fn set_utc_timezone(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn ipmi_over_lan_enabled(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError>;

    async fn set_ipmi_over_lan(
        &self,
        cx: &OpCx<'_, B>,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError>;
}
