/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{Attestation, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::schema::software_inventory::SoftwareInventory;

use crate::attestation::standard::StandardAttestation;

/// NVIDIA HGX: standard ComponentIntegrity plus the GPU root-of-trust to
/// firmware-inventory mapping.
pub(crate) struct HgxAttestation;

/// Maps `HGX_IRoT_GPU_<n>` to `HGX_FW_GPU_<n>`; other components have no mapping.
fn firmware_inventory_id(component_id: &str) -> Option<String> {
    let gpu = component_id.strip_prefix("HGX_IRoT_GPU_")?;
    (!gpu.is_empty() && gpu.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("HGX_FW_GPU_{gpu}"))
}

#[async_trait]
impl<B: Bmc> Attestation<B> for HgxAttestation {
    fn standard(&self) -> &dyn Attestation<B> {
        &StandardAttestation
    }

    async fn firmware_for_component(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
    ) -> Result<Arc<SoftwareInventory>, PlatformError> {
        let inventory_id = firmware_inventory_id(component_id).ok_or(PlatformError::Unsupported)?;
        cx.service_root()
            .update_service()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .firmware_inventories()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .into_iter()
            .find(|inventory| inventory.raw().id == inventory_id)
            .map(|inventory| inventory.raw())
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("firmware inventory {inventory_id} was not found"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hgx_gpu_component_to_firmware_inventory() {
        assert_eq!(
            firmware_inventory_id("HGX_IRoT_GPU_12").as_deref(),
            Some("HGX_FW_GPU_12")
        );
        assert_eq!(firmware_inventory_id("HGX_IRoT_NVSwitch_0"), None);
        assert_eq!(firmware_inventory_id("prefix_HGX_IRoT_GPU_0"), None);
    }
}
