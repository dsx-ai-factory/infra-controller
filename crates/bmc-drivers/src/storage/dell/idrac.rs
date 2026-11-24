/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Storage};
use nv_redfish::core::{ActionError, Bmc, Reference};
use nv_redfish::oem::dell::OperationApplyTime;
use nv_redfish::schema::volume::{LinksCreate, RaidType, VolumeCreate};

use crate::dell;
use crate::storage::support::RedfishStorageExt as _;

/// Dell iDRAC boot-storage driver; the Lifecycle Controller must be ready
/// before the BOSS controller is reconfigured.
pub(crate) struct IdracBossStorage;

fn raid_type(drive_count: usize) -> Result<RaidType, PlatformError> {
    match drive_count {
        1 => Ok(RaidType::Raid0),
        2 => Ok(RaidType::Raid1),
        count => Err(PlatformError::InvalidResponse {
            message: format!("Dell BOSS requires one or two drives, found {count}"),
        }),
    }
}

#[async_trait]
impl<B: Bmc> Storage<B> for IdracBossStorage
where
    B::Error: ActionError,
{
    async fn boot_controller(&self, cx: &OpCx<'_, B>) -> Result<Option<String>, PlatformError> {
        let controllers = cx.storage_controllers().await?;
        for controller in controllers {
            let raw = controller.raw();
            let id = &raw.odata_id;
            if id.to_string().contains("BOSS") {
                return id
                    .last_segment()
                    .map(str::to_owned)
                    .map(Some)
                    .ok_or_else(|| PlatformError::InvalidResponse {
                        message: format!("BOSS controller URI has no identifier: {id}"),
                    });
            }
        }

        Ok(None)
    }

    async fn decommission_controller(
        &self,
        cx: &OpCx<'_, B>,
        controller_id: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        dell::require_lifecycle_controller_ready(cx).await?;
        let response = cx
            .storage_controller(controller_id)
            .await?
            .oem_dell_actions()
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .decommission_controller_drives(Some(OperationApplyTime::Immediate))
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        dell::job_outcome(cx, response).await
    }

    async fn create_volume(
        &self,
        cx: &OpCx<'_, B>,
        controller_id: &str,
        volume_name: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        if volume_name.is_empty() || volume_name.len() > 15 {
            return Err(PlatformError::InvalidResponse {
                message: "Dell volume name must contain 1 through 15 bytes".to_string(),
            });
        }

        dell::require_lifecycle_controller_ready(cx).await?;
        let controller = cx.storage_controller(controller_id).await?;
        let raw = controller.raw();
        let drive_refs = raw
            .drives
            .as_ref()
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("controller {controller_id} does not report its drives"),
            })?;
        let request = VolumeCreate::builder()
            .with_name(volume_name.to_string())
            .with_raid_type(raid_type(drive_refs.len())?)
            .with_links(
                LinksCreate::builder()
                    .with_drives(drive_refs.iter().map(Reference::from).collect())
                    .build(),
            )
            .build();
        let response = controller
            .volumes()
            .ok_or(PlatformError::Unsupported)?
            .create(&request)
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        dell::job_outcome(cx, response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_drive_count_to_boss_raid_level() {
        assert_eq!(raid_type(1), Ok(RaidType::Raid0));
        assert_eq!(raid_type(2), Ok(RaidType::Raid1));
        assert!(raid_type(0).is_err());
        assert!(raid_type(3).is_err());
    }
}
