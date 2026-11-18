/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Storage resources reached from the selected ComputerSystem.

use bmc_platform::{OpCx, PlatformError};
use nv_redfish::computer_system::Storage;
use nv_redfish::core::Bmc;

/// Storage-controller discovery shared by storage operations.
pub(super) trait RedfishStorageExt<B: Bmc> {
    /// The selected system's controllers; empty when none are advertised.
    async fn storage_controllers(&self) -> Result<Vec<Storage<B>>, PlatformError>;

    /// Finds a controller by its URI identifier; a missing id is an invalid response.
    async fn storage_controller(&self, controller_id: &str) -> Result<Storage<B>, PlatformError>;
}

impl<B: Bmc> RedfishStorageExt<B> for OpCx<'_, B> {
    async fn storage_controllers(&self) -> Result<Vec<Storage<B>>, PlatformError> {
        Ok(self
            .system()
            .await?
            .storage_controllers()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .unwrap_or_default())
    }

    async fn storage_controller(&self, controller_id: &str) -> Result<Storage<B>, PlatformError> {
        let controllers = self.storage_controllers().await?;
        if let Some(controller) = controllers
            .into_iter()
            .find(|controller| controller.raw().odata_id.last_segment() == Some(controller_id))
        {
            return Ok(controller);
        }

        Err(PlatformError::InvalidResponse {
            message: format!("storage controller {controller_id} was not found"),
        })
    }
}
