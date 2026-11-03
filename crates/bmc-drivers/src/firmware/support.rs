/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Firmware mechanics shared by several vendor drivers.

use std::pin::Pin;

use bmc_platform::{DriverOutcome, FirmwareUpload, OpCx, OperationReference, PlatformError};
use nv_redfish::core::{
    Bmc, EntityTypeRef, ModificationResponse, MultipartUpdateRequest, ODataId, OemMultipartPart,
    UploadReader,
};
use nv_redfish::schema::message::Message;
use nv_redfish::update_service::UpdateService;
use serde::{Deserialize, Serialize};

use crate::resources::RedfishResourcesExt as _;

/// Firmware discovery and upload mechanics shared by firmware drivers.
pub(super) trait RedfishFirmwareExt<B: Bmc> {
    /// The `@odata.id` of each firmware inventory entry in `ids`, for update
    /// targets; `Unsupported` when the BMC lists one of them under no such id.
    async fn firmware_inventory_targets(&self, ids: &[&str]) -> Result<Vec<String>, PlatformError>;

    /// The `@odata.id` of the chassis `id`, for update targets.
    async fn firmware_chassis_target(&self, id: &str) -> Result<String, PlatformError>;

    /// The service root's update service.
    async fn update_service(&self) -> Result<UpdateService<B>, PlatformError>;

    /// Uploads `upload` with `parameters` and any OEM parts to the
    /// `MultipartHttpPushUri` `service` advertises; a 404 means the BMC does
    /// not implement multipart push.
    async fn multipart_upload<V>(
        &self,
        service: &UpdateService<B>,
        upload: FirmwareUpload,
        parameters: &V,
        oem_parts: Vec<OemMultipartPart>,
    ) -> Result<DriverOutcome, PlatformError>
    where
        V: Serialize + Send + Sync;
}

impl<B: Bmc> RedfishFirmwareExt<B> for OpCx<'_, B> {
    async fn firmware_inventory_targets(&self, ids: &[&str]) -> Result<Vec<String>, PlatformError> {
        let inventories = self
            .update_service()
            .await?
            .firmware_inventories()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        ids.iter()
            .map(|id| {
                inventories
                    .iter()
                    .map(|inventory| inventory.raw())
                    .find(|inventory| inventory.id == *id)
                    .map(|inventory| inventory.odata_id().to_string())
                    .ok_or(PlatformError::Unsupported)
            })
            .collect()
    }

    async fn firmware_chassis_target(&self, id: &str) -> Result<String, PlatformError> {
        self.chassis_by_id(id)
            .await?
            .map(|chassis| chassis.raw().odata_id().to_string())
            .ok_or(PlatformError::Unsupported)
    }

    async fn update_service(&self) -> Result<UpdateService<B>, PlatformError> {
        self.service_root()
            .update_service()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)
    }

    async fn multipart_upload<V>(
        &self,
        service: &UpdateService<B>,
        upload: FirmwareUpload,
        parameters: &V,
        oem_parts: Vec<OemMultipartPart>,
    ) -> Result<DriverOutcome, PlatformError>
    where
        V: Serialize + Send + Sync,
    {
        let request: MultipartUpdateRequest<'_, Pin<Box<dyn UploadReader>>, V> =
            MultipartUpdateRequest {
                update_parameters: parameters,
                update_stream: upload.image,
                oem_parts,
                upload_timeout: upload.timeout,
            };
        service
            .multipart_update::<_, _, UploadResponse>(request)
            .await
            .map(upload_outcome)
            .map_err(|error| match self.map_redfish_error(error) {
                PlatformError::Bmc { status: 404, .. } => PlatformError::Unsupported,
                other => other,
            })
    }
}

/// `Unsupported` unless the update service advertises a non-empty
/// `MultipartHttpPushUri`.
pub(super) fn require_multipart_push<B: Bmc>(
    service: &UpdateService<B>,
) -> Result<(), PlatformError> {
    service
        .raw()
        .multipart_http_push_uri
        .as_deref()
        .filter(|uri| !uri.trim().is_empty())
        .map(drop)
        .ok_or(PlatformError::Unsupported)
}

/// The message announcing the task an upload started.
const TASK_STARTED: &str = "Task.1.0.New";

/// An upload answered without `202 Accepted`: the started task is announced
/// in `Messages`, or the body is the task itself.
#[derive(Deserialize)]
pub(super) struct UploadResponse {
    #[serde(rename = "@odata.id")]
    odata_id: Option<ODataId>,
    #[serde(rename = "@odata.type")]
    odata_type: Option<String>,
    #[serde(rename = "Messages", default)]
    messages: Vec<Message>,
}

/// The task an upload started, or completion when it started none.
pub(super) fn upload_outcome(response: ModificationResponse<UploadResponse>) -> DriverOutcome {
    let ModificationResponse::Entity(body) = response else {
        return DriverOutcome::from(response);
    };
    let announced = body.messages.iter().find_map(|message| {
        (message.message_id == TASK_STARTED)
            .then(|| message.message_args.as_ref()?.first().cloned())
            .flatten()
    });
    let task = body
        .odata_type
        .as_deref()
        .is_some_and(|odata_type| odata_type.starts_with("#Task."));
    match announced
        .map(ODataId::from)
        .or(body.odata_id.filter(|_| task))
    {
        Some(uri) => DriverOutcome::accepted(OperationReference::RedfishTask {
            uri,
            retry_after_seconds: None,
            task: None,
        }),
        None => DriverOutcome::complete(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_upload_answered_with_a_task_message_is_tracked_as_that_task() {
        let entity = |body| {
            ModificationResponse::Entity(
                serde_json::from_value::<UploadResponse>(body).expect("upload response"),
            )
        };
        assert_eq!(
            upload_outcome(entity(json!({"Messages": [{
                "MessageId": TASK_STARTED,
                "MessageArgs": ["/redfish/v1/TaskService/Tasks/3"],
            }]}))),
            DriverOutcome::accepted(OperationReference::RedfishTask {
                uri: "/redfish/v1/TaskService/Tasks/3".to_string().into(),
                retry_after_seconds: None,
                task: None,
            })
        );
        assert_eq!(
            upload_outcome(entity(json!({"Messages": []}))),
            DriverOutcome::complete()
        );
    }
}
