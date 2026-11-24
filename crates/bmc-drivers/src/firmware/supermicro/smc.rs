/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Firmware, FirmwareComponent, FirmwareUpload, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::resource::OemUpdate;
use nv_redfish::update_service::{
    MultipartUpdateParameters, MultipartUpdateParametersWithApplyTime, OperationApplyTime,
};
use serde_json::{Value, json};

use crate::firmware::standard::StandardFirmware;
use crate::firmware::support::{RedfishFirmwareExt as _, require_multipart_push};
use crate::resources::RedfishResourcesExt as _;

/// Supermicro: the component's resource is the update target, BIOS and BMC
/// images keep their configuration through the Supermicro preservation flags,
/// and the update applies at once.
pub(crate) struct SmcFirmware;

/// The update target for `component` and the Supermicro OEM parameters it needs.
async fn target<B: Bmc>(
    cx: &OpCx<'_, B>,
    component: FirmwareComponent,
) -> Result<(String, Option<Value>), PlatformError> {
    Ok(match component {
        FirmwareComponent::Uefi => (
            cx.bios().await?.raw().odata_id.to_string(),
            Some(json!({"Supermicro": {"BIOS": {
                "PreserveME": true,
                "PreserveNVRAM": true,
                "PreserveSMBIOS": true,
                "BackupBIOS": false,
            }}})),
        ),
        FirmwareComponent::Bmc => (
            cx.manager().await?.raw().odata_id.to_string(),
            Some(json!({"Supermicro": {"BMC": {
                "PreserveCfg": true,
                "PreserveSdr": true,
                "PreserveSsl": true,
                "BackupBMC": true,
            }}})),
        ),
        FirmwareComponent::CpldMb => (inventory_target(cx, "CPLD_Motherboard").await?, None),
        FirmwareComponent::CpldMid => (inventory_target(cx, "CPLD_Backplane_1").await?, None),
        _ => return Err(PlatformError::Unsupported),
    })
}

async fn inventory_target<B: Bmc>(cx: &OpCx<'_, B>, id: &str) -> Result<String, PlatformError> {
    cx.firmware_inventory_targets(&[id])
        .await?
        .pop()
        .ok_or(PlatformError::Unsupported)
}

#[async_trait]
impl<B: Bmc> Firmware<B> for SmcFirmware {
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
        let (target, oem) = target(cx, upload.component).await?;
        let mut parameters = MultipartUpdateParameters::builder().with_targets(vec![target]);
        if let Some(oem) = oem {
            parameters = parameters.with_oem(OemUpdate {
                additional_properties: oem,
            });
        }
        let parameters = MultipartUpdateParametersWithApplyTime {
            parameters: parameters.build(),
            operation_apply_time: Some(OperationApplyTime::Immediate),
        };
        cx.multipart_upload(&service, upload, &parameters, Vec::new())
            .await
    }
}
