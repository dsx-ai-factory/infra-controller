/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, Firmware, FirmwareUpload, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::schema::resource::OemUpdate;
use nv_redfish::update_service::{
    MultipartUpdateParameters, MultipartUpdateParametersWithApplyTime, OperationApplyTime,
};
use serde_json::json;

use crate::firmware::standard::StandardFirmware;
use crate::firmware::support::{RedfishFirmwareExt as _, require_multipart_push};

/// Dell iDRAC: the image picks its own target and applies at once or on the
/// next reset as the caller asks. Older iDRAC firmware that does not advertise
/// `MultipartHttpPushUri` has no multipart upload.
pub(crate) struct IdracFirmware;

fn update_parameters(apply_immediately: bool) -> MultipartUpdateParametersWithApplyTime {
    MultipartUpdateParametersWithApplyTime {
        parameters: MultipartUpdateParameters::builder()
            .with_targets(Vec::new())
            .with_oem(OemUpdate {
                additional_properties: json!({}),
            })
            .build(),
        operation_apply_time: Some(if apply_immediately {
            OperationApplyTime::Immediate
        } else {
            OperationApplyTime::OnReset
        }),
    }
}

#[async_trait]
impl<B: Bmc> Firmware<B> for IdracFirmware {
    fn standard(&self) -> &dyn Firmware<B> {
        &StandardFirmware
    }

    async fn multipart_update(
        &self,
        cx: &OpCx<'_, B>,
        upload: FirmwareUpload,
    ) -> Result<DriverOutcome, PlatformError> {
        let parameters = update_parameters(upload.apply_immediately);
        let service = cx.update_service().await?;
        require_multipart_push(&service)?;
        cx.multipart_upload(&service, upload, &parameters, Vec::new())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_parameters_name_no_targets_and_the_requested_apply_time() {
        for (apply_immediately, apply_time) in [(true, "Immediate"), (false, "OnReset")] {
            assert_eq!(
                serde_json::to_value(update_parameters(apply_immediately)).expect("serializes"),
                json!({"Targets": [], "Oem": {}, "@Redfish.OperationApplyTime": apply_time})
            );
        }
    }
}
