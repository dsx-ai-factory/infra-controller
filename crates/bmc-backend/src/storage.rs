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
use bmc_platform::{DriverOutcome, PlatformError, Storage};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::Redfish;

use crate::BackendBmc;
use crate::convert::platform_error;
use crate::status::idrac_outcome;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::storage`].
    pub fn storage(&self) -> Result<BackendStorage<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendStorage::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.storage().map(BackendStorage::Drivers),
        }
    }
}

/// The boot-storage operations of a [`BackendBmc`].
pub enum BackendStorage<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Storage<RedfishBmc>, RedfishBmc>),
}

impl BackendStorage<'_> {
    /// See [`Storage::boot_controller`]. libredfish finds only Dell's BOSS
    /// controller.
    pub async fn boot_controller(&self) -> Result<Option<String>, PlatformError> {
        match self {
            Self::Libredfish(client) => client.get_boss_controller().await.map_err(platform_error),
            Self::Drivers(storage) => storage.boot_controller().await,
        }
    }

    /// See [`Storage::decommission_controller`]. libredfish returns the id of
    /// the iDRAC job it started.
    pub async fn decommission_controller(
        &self,
        controller_id: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .decommission_storage_controller(controller_id)
                .await
                .map(idrac_outcome)
                .map_err(platform_error),
            Self::Drivers(storage) => storage.decommission_controller(controller_id).await,
        }
    }

    /// See [`Storage::create_volume`]. libredfish returns the id of the iDRAC
    /// job it started.
    pub async fn create_volume(
        &self,
        controller_id: &str,
        volume_name: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .create_storage_volume(controller_id, volume_name)
                .await
                .map(idrac_outcome)
                .map_err(platform_error),
            Self::Drivers(storage) => storage.create_volume(controller_id, volume_name).await,
        }
    }
}
