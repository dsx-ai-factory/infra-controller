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

/// NVIDIA DGX Viking: uploads target the firmware inventory entry of the
/// component, through the `UpdateService/upload` URI nv-redfish supplies for
/// this firmware, which does not advertise one.
pub(crate) struct VikingFirmware;

/// The firmware inventory entry `component` updates; `None` leaves the target
/// to the image.
fn inventory_id(component: FirmwareComponent) -> Result<Option<String>, PlatformError> {
    Ok(Some(match component {
        FirmwareComponent::Unknown => return Ok(None),
        FirmwareComponent::Bmc => "HostBMC_0".to_string(),
        FirmwareComponent::Uefi => "HostBIOS_0".to_string(),
        FirmwareComponent::ErotBmc => "EROT_BMC_0".to_string(),
        FirmwareComponent::ErotBios => "EROT_BIOS_0".to_string(),
        FirmwareComponent::CpldMid => "CPLDMID_0".to_string(),
        FirmwareComponent::CpldMb => "CPLDMB_0".to_string(),
        FirmwareComponent::Psu(index) => format!("PSU_{index}"),
        FirmwareComponent::PcieSwitch(index) => format!("PCIeSwitch_{index}"),
        FirmwareComponent::PcieRetimer(index) => format!("PCIeRetimer_{index}"),
        FirmwareComponent::HgxBmc => "HGX_FW_BMC_0".to_string(),
        FirmwareComponent::CpldPdb => return Err(PlatformError::Unsupported),
    }))
}

#[async_trait]
impl<B: Bmc> Firmware<B> for VikingFirmware {
    fn standard(&self) -> &dyn Firmware<B> {
        &StandardFirmware
    }

    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        upload: FirmwareUpload,
    ) -> Result<DriverOutcome, PlatformError> {
        let mut parameters = MultipartUpdateParameters::builder();
        if let Some(id) = inventory_id(upload.component)? {
            parameters =
                parameters.with_targets(cx.firmware_inventory_targets(&[id.as_str()]).await?);
        }
        let service = cx.update_service().await?;
        require_multipart_push(&service)?;
        cx.multipart_upload(&service, upload, &parameters.build(), Vec::new())
            .await
    }
}
