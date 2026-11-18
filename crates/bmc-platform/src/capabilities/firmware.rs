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
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nv_redfish::core::{Bmc, DataStream, UploadReader};
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::schema::update_service::UpdateServiceSimpleUpdateAction;

use crate::{DriverOutcome, OpCx, PlatformError};

/// The device a firmware image updates; each driver turns it into the targets
/// and OEM parameters its BMC needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareComponent {
    Bmc,
    Uefi,
    ErotBmc,
    ErotBios,
    CpldMid,
    CpldMb,
    CpldPdb,
    Psu(u32),
    PcieSwitch(u32),
    PcieRetimer(u32),
    HgxBmc,
    /// The image itself names what it updates.
    Unknown,
}

/// A firmware image for a multipart `UpdateService` upload.
pub struct FirmwareUpload {
    /// The image and the file name it is uploaded under.
    pub image: DataStream<Pin<Box<dyn UploadReader>>>,
    pub component: FirmwareComponent,
    /// Whether the image applies once uploaded rather than on the next reset,
    /// on BMCs that let the caller choose.
    pub apply_immediately: bool,
    pub timeout: Duration,
}

/// Firmware inventory and update operations.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait Firmware<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn Firmware<B>;

    async fn inventory(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<Vec<Arc<SoftwareInventory>>, PlatformError> {
        self.standard().inventory(cx).await
    }

    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        upload: FirmwareUpload,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().multipart_update(cx, upload).await
    }

    async fn simple_update(
        &self,
        cx: &OpCx<'_, B>,
        request: &UpdateServiceSimpleUpdateAction,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().simple_update(cx, request).await
    }
}
