/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, Firmware, FirmwareUpload, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::update_service::{
    MultipartUpdateParameters, MultipartUpdateParametersWithApplyTime, OperationApplyTime,
};

use crate::firmware::standard::StandardFirmware;
use crate::firmware::support::{RedfishFirmwareExt as _, require_multipart_push};

/// Lenovo XCC: the image picks its own target, and the update is forced and
/// applies at once.
pub(crate) struct XccFirmware;

#[async_trait]
impl<B: Bmc> Firmware<B> for XccFirmware {
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
        let parameters = MultipartUpdateParametersWithApplyTime {
            parameters: MultipartUpdateParameters::builder()
                .with_targets(Vec::new())
                .with_force_update(true)
                .build(),
            operation_apply_time: Some(OperationApplyTime::Immediate),
        };
        cx.multipart_upload(&service, upload, &parameters, Vec::new())
            .await
    }
}
