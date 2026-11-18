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

/// Boot-storage controller lifecycle operations.
#[async_trait]
pub trait Storage<B: Bmc>: Send + Sync {
    async fn boot_controller(&self, cx: &OpCx<'_, B>) -> Result<Option<String>, PlatformError>;

    async fn decommission_controller(
        &self,
        cx: &OpCx<'_, B>,
        controller_id: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn create_volume(
        &self,
        cx: &OpCx<'_, B>,
        controller_id: &str,
        volume_name: &str,
    ) -> Result<DriverOutcome, PlatformError>;
}
