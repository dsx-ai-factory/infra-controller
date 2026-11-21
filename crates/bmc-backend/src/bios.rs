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
use bmc_platform::{Bios, DriverOutcome, PlatformError};
use bmc_runtime::BoundDriver;
use carbide_redfish::libredfish::redact_passwords;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::Redfish;

use crate::BackendBmc;
use crate::convert::platform_error;
use crate::status::idrac_outcome;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::bios`].
    pub fn bios(&self) -> Result<BackendBios<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendBios::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.bios().map(BackendBios::Drivers),
        }
    }
}

/// The BIOS operations of a [`BackendBmc`].
pub enum BackendBios<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Bios<RedfishBmc>, RedfishBmc>),
}

impl BackendBios<'_> {
    /// See [`Bios::change_uefi_password`]. Redacts both passwords from any
    /// returned error.
    pub async fn change_uefi_password(
        &self,
        current_password: &str,
        new_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .change_uefi_password(current_password, new_password)
                .await
                .map(idrac_outcome)
                .map_err(|err| {
                    platform_error(redact_passwords(err, &[current_password, new_password]))
                }),
            Self::Drivers(bios) => {
                bios.change_uefi_password(current_password, new_password)
                    .await
            }
        }
    }

    /// See [`Bios::clear_uefi_password`]. Redacts `current_password` from any
    /// returned error.
    pub async fn clear_uefi_password(
        &self,
        current_password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .clear_uefi_password(current_password)
                .await
                .map(idrac_outcome)
                .map_err(|err| platform_error(redact_passwords(err, &[current_password]))),
            Self::Drivers(bios) => bios.clear_uefi_password(current_password).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_redfish::libredfish::RedfishClientPool;
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_utils::redfish::BmcAccessInfo;
    use mac_address::MacAddress;

    use super::*;

    #[tokio::test]
    async fn libredfish_bios_change_and_clear_uefi_password_complete() {
        let sim = Arc::new(RedfishSim::default());
        let access = BmcAccessInfo {
            host: "127.0.0.1".to_string(),
            port: Some(443),
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 2]),
        };
        let bmc = BackendBmc::from(
            sim.client_by_info(&access)
                .await
                .expect("sim client builds"),
        );

        let changed = bmc
            .bios()
            .expect("bios")
            .change_uefi_password("old-uefi", "new-uefi")
            .await
            .expect("change_uefi_password succeeds");
        assert_eq!(changed, DriverOutcome::complete());

        let cleared = bmc
            .bios()
            .expect("bios")
            .clear_uefi_password("new-uefi")
            .await
            .expect("clear_uefi_password succeeds");
        assert_eq!(cleared, DriverOutcome::complete());
    }
}
