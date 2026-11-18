/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Dpu, DpuStatus, DriverOutcome, HostPrivilegeLevel, NicMode, OpCx, PlatformError, Quirk,
    RshimState,
};
use nv_redfish::core::Bmc;
use serde_json::json;

use crate::dpu::nvidia::support::{
    bios_host_privilege_level, bios_nic_mode, bmc_rshim_state, enable_bmc_rshim, nic_mode_value,
    set_bios_host_privilege_level, write_bios,
};
use crate::resources::attribute_map;

/// BlueField-2: NIC mode is a BIOS attribute and there is no host rshim control.
pub(crate) struct BlueField2Dpu;

#[async_trait]
impl<B: Bmc> Dpu<B> for BlueField2Dpu {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<DpuStatus, PlatformError> {
        let nic_mode = if cx.has_quirk(Quirk::BlueFieldNicModeUnreadable) {
            None
        } else {
            Some(bios_nic_mode(cx).await?)
        };
        Ok(DpuStatus {
            nic_mode,
            host_rshim: None,
            bmc_rshim: bmc_rshim_state(cx).await?,
            host_privilege: bios_host_privilege_level(cx).await?,
        })
    }

    async fn set_nic_mode(
        &self,
        cx: &OpCx<'_, B>,
        mode: NicMode,
    ) -> Result<DriverOutcome, PlatformError> {
        if cx.has_quirk(Quirk::BlueFieldNicModeUnreadable) {
            return Err(PlatformError::Unsupported);
        }
        write_bios(
            cx,
            &attribute_map([("NicMode", json!(nic_mode_value(mode)))]),
        )
        .await
    }

    /// There is nothing to change, so every requested state is complete.
    async fn set_host_rshim(
        &self,
        _cx: &OpCx<'_, B>,
        _state: RshimState,
    ) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
    }

    async fn enable_bmc_rshim(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        enable_bmc_rshim(cx).await
    }

    async fn set_host_privilege_level(
        &self,
        cx: &OpCx<'_, B>,
        level: HostPrivilegeLevel,
    ) -> Result<DriverOutcome, PlatformError> {
        set_bios_host_privilege_level(cx, level).await
    }
}
