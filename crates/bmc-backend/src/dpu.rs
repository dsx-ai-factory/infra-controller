/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use bmc_drivers::CatalogError;
use bmc_platform::{Dpu, DriverOutcome, HostPrivilegeLevel, NicMode, PlatformError, RshimState};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::model::oem::nvidia_dpu;
use libredfish::{EnabledDisabled, Redfish};

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::dpu`].
    pub fn dpu(&self) -> Result<BackendDpu<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendDpu::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.dpu().map(BackendDpu::Drivers),
        }
    }
}

/// The DPU operations of a [`BackendBmc`].
pub enum BackendDpu<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Dpu<RedfishBmc>, RedfishBmc>),
}

impl BackendDpu<'_> {
    /// See [`Dpu::set_nic_mode`].
    pub async fn set_nic_mode(&self, mode: NicMode) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let mode = match mode {
                    NicMode::Nic => nvidia_dpu::NicMode::Nic,
                    NicMode::Dpu => nvidia_dpu::NicMode::Dpu,
                };
                client.set_nic_mode(mode).await.map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(dpu) => dpu.set_nic_mode(mode).await,
        }
    }

    /// See [`Dpu::set_host_rshim`].
    pub async fn set_host_rshim(&self, state: RshimState) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let state = match state {
                    RshimState::Enabled => EnabledDisabled::Enabled,
                    RshimState::Disabled => EnabledDisabled::Disabled,
                };
                client.set_host_rshim(state).await.map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(dpu) => dpu.set_host_rshim(state).await,
        }
    }

    /// See [`Dpu::enable_bmc_rshim`].
    pub async fn enable_bmc_rshim(&self) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                client.enable_rshim_bmc().await.map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(dpu) => dpu.enable_bmc_rshim().await,
        }
    }

    /// See [`Dpu::set_host_privilege_level`].
    pub async fn set_host_privilege_level(
        &self,
        level: HostPrivilegeLevel,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let level = match level {
                    HostPrivilegeLevel::Privileged => nvidia_dpu::HostPrivilegeLevel::Privileged,
                    HostPrivilegeLevel::Restricted => nvidia_dpu::HostPrivilegeLevel::Restricted,
                };
                client
                    .set_host_privilege_level(level)
                    .await
                    .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(dpu) => dpu.set_host_privilege_level(level).await,
        }
    }
}
