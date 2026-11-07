/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish firmware operations.

use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, Firmware, FirmwareUpload, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::schema::update_service::UpdateServiceSimpleUpdateAction;
use nv_redfish::update_service::MultipartUpdateParameters;

use crate::firmware::support::{RedfishFirmwareExt as _, require_multipart_push};

/// Standard Redfish inventory, SimpleUpdate, and multipart upload.
pub(crate) struct StandardFirmware;

#[async_trait]
impl<B: Bmc> Firmware<B> for StandardFirmware {
    fn standard(&self) -> &dyn Firmware<B> {
        self
    }

    async fn inventory(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<Vec<Arc<SoftwareInventory>>, PlatformError> {
        let inventories = cx
            .update_service()
            .await?
            .firmware_inventories()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        Ok(inventories
            .into_iter()
            .map(|inventory| inventory.raw())
            .collect())
    }

    /// Uploads through the advertised `MultipartHttpPushUri` with empty
    /// parameters, leaving the target to the image.
    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        upload: FirmwareUpload,
    ) -> Result<DriverOutcome, PlatformError> {
        let service = cx.update_service().await?;
        require_multipart_push(&service)?;
        cx.multipart_upload(
            &service,
            upload,
            &MultipartUpdateParameters::default(),
            Vec::new(),
        )
        .await
    }

    /// Runs the advertised `UpdateService.SimpleUpdate` action.
    async fn simple_update(
        &self,
        cx: &OpCx<'_, B>,
        request: &UpdateServiceSimpleUpdateAction,
    ) -> Result<DriverOutcome, PlatformError> {
        let service = cx.update_service().await?.raw();
        let action = service
            .actions
            .as_ref()
            .and_then(|actions| actions.simple_update.as_ref())
            .ok_or(PlatformError::Unsupported)?;
        cx.action(action, request).await
    }
}
