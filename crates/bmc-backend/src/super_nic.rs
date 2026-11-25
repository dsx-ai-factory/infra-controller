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
use bmc_platform::{DriverOutcome, PlatformError, SuperNic};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::Redfish;

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::super_nic`].
    pub fn super_nic(&self) -> Result<BackendSuperNic<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendSuperNic::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.super_nic().map(BackendSuperNic::Drivers),
        }
    }
}

/// The SuperNIC operations of a [`BackendBmc`].
pub enum BackendSuperNic<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn SuperNic<RedfishBmc>, RedfishBmc>),
}

impl BackendSuperNic<'_> {
    /// See [`SuperNic::set_east_west_control`].
    pub async fn set_east_west_control(
        &self,
        index: u8,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .set_spx_nic_east_west_control_enabled(index, enabled)
                .await
                .map(|()| DriverOutcome::complete())
                .map_err(platform_error),
            Self::Drivers(super_nic) => super_nic.set_east_west_control(index, enabled).await,
        }
    }

    /// See [`SuperNic::mac_address`].
    pub async fn mac_address(&self, index: u8) -> Result<Option<String>, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .get_spx_nic_mac_address(index)
                .await
                .map_err(platform_error),
            Self::Drivers(super_nic) => super_nic.mac_address(index).await,
        }
    }
}
