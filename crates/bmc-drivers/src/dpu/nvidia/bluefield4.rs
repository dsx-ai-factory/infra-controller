/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Dpu, DpuStatus, DriverOutcome, HostPrivilegeLevel, NicMode, OpCx, PlatformError, RshimState,
};
use nv_redfish::chassis::{NetworkAdapter, NetworkAdapterUpdate};
use nv_redfish::core::Bmc;
use nv_redfish::oem::nvidia::network_adapter::{
    DpuOperationMode, HostPrivilegeLevelInput, NvidiaNetworkAdapter, NvidiaNetworkAdapterUpdate,
    NvidiaNetworkAdapterUpdateExt, PrivilegeModeType,
};

use crate::dpu::nvidia::support::{bmc_rshim_state, enable_bmc_rshim, host_rshim_state};
use crate::resources::RedfishResourcesExt as _;

/// BlueField-4: mode and host privileges live on the network adapter's
/// `Oem.Nvidia` and change through its settings objects. There is no host
/// rshim control, and BMC rshim is available only where the manager links it.
/// A switch to NIC mode waits for an operator while host privilege is
/// Restricted.
pub(crate) struct BlueField4Dpu;

/// The manual step a switch to NIC mode waits on while host privilege is Restricted.
const HOST_PRIVILEGE_RESTRICTED: &str = "dpu-host-privilege-restricted";

fn restricted_host_privilege() -> PlatformError {
    PlatformError::ManualInterventionRequired {
        code: HOST_PRIVILEGE_RESTRICTED.to_string(),
    }
}

/// The first network adapter carrying an `Oem.Nvidia` extension; the
/// controls are unsupported on a BMC without one.
async fn nvidia_adapter<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<(NetworkAdapter<B>, NvidiaNetworkAdapter<B>), PlatformError> {
    for chassis in &cx.all_chassis().await? {
        let adapters = chassis
            .network_adapters()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .unwrap_or_default();
        for adapter in adapters {
            if let Some(nvidia) = adapter
                .oem_nvidia()
                .map_err(|error| cx.map_redfish_error(error))?
            {
                return Ok((adapter, nvidia));
            }
        }
    }
    Err(PlatformError::Unsupported)
}

/// The host privilege level on the adapter's privilege configuration.
async fn host_privilege<B: Bmc>(
    cx: &OpCx<'_, B>,
    nvidia: &NvidiaNetworkAdapter<B>,
) -> Result<Option<HostPrivilegeLevel>, PlatformError> {
    let level = nvidia
        .host_privilege_config()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .and_then(|config| config.host_privilege_level());
    Ok(match level {
        Some(HostPrivilegeLevelInput::Privileged) => Some(HostPrivilegeLevel::Privileged),
        Some(HostPrivilegeLevelInput::Restricted) => Some(HostPrivilegeLevel::Restricted),
        Some(HostPrivilegeLevelInput::UnsupportedValue) | None => None,
    })
}

#[async_trait]
impl<B: Bmc> Dpu<B> for BlueField4Dpu {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<DpuStatus, PlatformError> {
        let (_, nvidia) = nvidia_adapter(cx).await?;
        let nic_mode = match nvidia.dpu_operation_mode() {
            Some(DpuOperationMode::Nic) => Some(NicMode::Nic),
            Some(DpuOperationMode::Dpu) => Some(NicMode::Dpu),
            Some(DpuOperationMode::UnsupportedValue) | None => None,
        };
        let host_rshim = cx
            .system()
            .await?
            .oem_nvidia()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .and_then(|oem| host_rshim_state(&oem));
        Ok(DpuStatus {
            nic_mode,
            host_rshim,
            bmc_rshim: bmc_rshim_state(cx).await?,
            host_privilege: host_privilege(cx, &nvidia).await?,
        })
    }

    async fn set_nic_mode(
        &self,
        cx: &OpCx<'_, B>,
        mode: NicMode,
    ) -> Result<DriverOutcome, PlatformError> {
        let (adapter, nvidia) = nvidia_adapter(cx).await?;
        if mode == NicMode::Nic
            && host_privilege(cx, &nvidia).await? == Some(HostPrivilegeLevel::Restricted)
        {
            return Err(restricted_host_privilege());
        }
        let mode = match mode {
            NicMode::Nic => DpuOperationMode::Nic,
            NicMode::Dpu => DpuOperationMode::Dpu,
        };
        let update = NetworkAdapterUpdate::builder()
            .build()
            .with_oem_nvidia(
                NvidiaNetworkAdapterUpdate::builder()
                    .with_dpu_operation_mode(mode)
                    .build(),
            )
            .map_err(|error| PlatformError::InvalidResponse {
                message: format!("failed to build the DPU operation mode update: {error}"),
            })?;
        adapter
            .settings()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .unwrap_or(adapter)
            .update(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn set_host_rshim(
        &self,
        _cx: &OpCx<'_, B>,
        _state: RshimState,
    ) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    async fn enable_bmc_rshim(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        enable_bmc_rshim(cx).await
    }

    /// Applies the matching privilege preset, since the BMC rejects a change
    /// of `HostPrivilegeLevel` alone.
    async fn set_host_privilege_level(
        &self,
        cx: &OpCx<'_, B>,
        level: HostPrivilegeLevel,
    ) -> Result<DriverOutcome, PlatformError> {
        let mode = match level {
            HostPrivilegeLevel::Privileged => PrivilegeModeType::Privileged,
            HostPrivilegeLevel::Restricted => PrivilegeModeType::Restricted,
        };
        let (_, nvidia) = nvidia_adapter(cx).await?;
        let config = nvidia
            .host_privilege_config()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        config
            .settings()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .unwrap_or(config)
            .set_privilege_mode(mode)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }
}
