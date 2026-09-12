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

use std::pin::Pin;

use async_trait::async_trait;
use nv_redfish::core::{Bmc, MultipartUpdateRequest, UploadReader};
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::schema::update_service::UpdateServiceSimpleUpdateAction;
use nv_redfish::update_service::MultipartUpdateParameters;

use crate::{DriverOutcome, OpCx, PlatformError};

/// Firmware inventory and update operations.
#[async_trait]
pub trait Firmware<B: Bmc>: Send + Sync {
    async fn inventory(&self, cx: &OpCx<'_, B>) -> Result<Vec<SoftwareInventory>, PlatformError>;

    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        request: MultipartUpdateRequest<'_, Pin<Box<dyn UploadReader>>, MultipartUpdateParameters>,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn simple_update(
        &self,
        cx: &OpCx<'_, B>,
        request: &UpdateServiceSimpleUpdateAction,
    ) -> Result<DriverOutcome, PlatformError>;
}
