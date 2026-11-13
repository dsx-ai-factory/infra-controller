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
use bmc_platform::{BmcControl, DriverOutcome, PlatformError};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::{EnabledDisabled, Redfish};

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::bmc_control`].
    pub fn bmc_control(&self) -> Result<BackendBmcControl<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendBmcControl::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.bmc_control().map(BackendBmcControl::Drivers),
        }
    }
}

/// The manager-control operations of a [`BackendBmc`].
pub enum BackendBmcControl<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn BmcControl<RedfishBmc>, RedfishBmc>),
}

impl BackendBmcControl<'_> {
    /// See [`BmcControl::reset`].
    pub async fn reset(&self) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                client.bmc_reset(None).await.map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(bmc_control) => bmc_control.reset().await,
        }
    }

    /// See [`BmcControl::reset_to_factory_defaults`].
    pub async fn reset_to_factory_defaults(&self) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                client
                    .bmc_reset_to_defaults()
                    .await
                    .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(bmc_control) => bmc_control.reset_to_factory_defaults().await,
        }
    }

    /// See [`BmcControl::ipmi_over_lan_enabled`].
    pub async fn ipmi_over_lan_enabled(&self) -> Result<bool, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .is_ipmi_over_lan_enabled()
                .await
                .map_err(platform_error),
            Self::Drivers(bmc_control) => bmc_control.ipmi_over_lan_enabled().await,
        }
    }

    /// See [`BmcControl::set_ipmi_over_lan`].
    pub async fn set_ipmi_over_lan(&self, enabled: bool) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let target = if enabled {
                    EnabledDisabled::Enabled
                } else {
                    EnabledDisabled::Disabled
                };
                client
                    .enable_ipmi_over_lan(target)
                    .await
                    .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(bmc_control) => bmc_control.set_ipmi_over_lan(enabled).await,
        }
    }
}
