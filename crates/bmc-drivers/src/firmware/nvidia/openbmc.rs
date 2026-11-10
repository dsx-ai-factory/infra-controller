/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Firmware, FirmwareComponent, FirmwareUpload, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::update_service::MultipartUpdateParameters;

use crate::firmware::standard::StandardFirmware;
use crate::firmware::support::{RedfishFirmwareExt as _, require_multipart_push};

/// NVIDIA OpenBMC compute trays (GB200/GB300, Vera Rubin): the HGX component
/// is the update target, and `ForceUpdate` keeps the BMC from skipping images
/// that match the installed version.
pub(crate) struct OpenBmcFirmware;

/// The update targets for `component`; `None` leaves the target to the image.
async fn targets<B: Bmc>(
    cx: &OpCx<'_, B>,
    component: FirmwareComponent,
) -> Result<Option<Vec<String>>, PlatformError> {
    Ok(match component {
        FirmwareComponent::Unknown => None,
        FirmwareComponent::Bmc => Some(Vec::new()),
        FirmwareComponent::ErotBmc => {
            Some(vec![cx.firmware_chassis_target("HGX_ERoT_BMC_0").await?])
        }
        FirmwareComponent::ErotBios => Some(cx.firmware_inventory_targets(&["EROT_BIOS_0"]).await?),
        FirmwareComponent::HgxBmc | FirmwareComponent::Uefi => {
            Some(vec![cx.firmware_chassis_target("HGX_Chassis_0").await?])
        }
        _ => return Err(PlatformError::Unsupported),
    })
}

#[async_trait]
impl<B: Bmc> Firmware<B> for OpenBmcFirmware {
    fn standard(&self) -> &dyn Firmware<B> {
        &StandardFirmware
    }

    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        upload: FirmwareUpload,
    ) -> Result<DriverOutcome, PlatformError> {
        let service = cx.update_service().await?;
        require_multipart_push(&service)?;
        let mut parameters = MultipartUpdateParameters::builder().with_force_update(true);
        if let Some(targets) = targets(cx, upload.component).await? {
            parameters = parameters.with_targets(targets);
        }
        cx.multipart_upload(&service, upload, &parameters.build(), Vec::new())
            .await
    }
}
