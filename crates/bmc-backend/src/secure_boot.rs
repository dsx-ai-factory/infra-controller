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
use bmc_platform::{DriverOutcome, PlatformError, SecureBoot, SecureBootStatus};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::Redfish;
use nv_redfish::schema::secure_boot::SecureBootUpdate;

use crate::BackendBmc;
use crate::convert::platform_error;
use crate::status::redfish_task;

const PLATFORM_KEY_DATABASE: &str = "PK";

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::secure_boot`].
    pub fn secure_boot(&self) -> Result<BackendSecureBoot<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendSecureBoot::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.secure_boot().map(BackendSecureBoot::Drivers),
        }
    }
}

/// The secure boot operations of a [`BackendBmc`].
pub enum BackendSecureBoot<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn SecureBoot<RedfishBmc>, RedfishBmc>),
}

impl BackendSecureBoot<'_> {
    /// See [`SecureBoot::status`]. libredfish's status needs both
    /// `SecureBootEnable` and `SecureBootCurrentBoot`.
    pub async fn status(&self) -> Result<SecureBootStatus, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let secure_boot = client.get_secure_boot().await.map_err(platform_error)?;
                status(
                    secure_boot.secure_boot_enable,
                    secure_boot
                        .secure_boot_current_boot
                        .map(|current| current.is_enabled()),
                )
            }
            Self::Drivers(secure_boot) => secure_boot.status().await,
        }
    }

    /// See [`SecureBoot::set`]. libredfish sets only `SecureBootEnable`.
    pub async fn set(&self, update: &SecureBootUpdate) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                match update.secure_boot_enable {
                    Some(true) => client.enable_secure_boot().await,
                    Some(false) => client.disable_secure_boot().await,
                    None => return Err(PlatformError::Unsupported),
                }
                .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(secure_boot) => secure_boot.set(update).await,
        }
    }

    /// See [`SecureBoot::has_platform_key`].
    pub async fn has_platform_key(&self) -> Result<bool, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .get_secure_boot_certificates(PLATFORM_KEY_DATABASE)
                .await
                .map(|certificates| !certificates.is_empty())
                .map_err(platform_error),
            Self::Drivers(secure_boot) => secure_boot.has_platform_key().await,
        }
    }

    /// See [`SecureBoot::add_platform_key`]. libredfish's upload always
    /// returns the Task it started.
    pub async fn add_platform_key(&self, pem: &str) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let task = client
                    .add_secure_boot_certificate(pem, PLATFORM_KEY_DATABASE)
                    .await
                    .map_err(platform_error)?;
                Ok(DriverOutcome::accepted(redfish_task(&task.id)))
            }
            Self::Drivers(secure_boot) => secure_boot.add_platform_key(pem).await,
        }
    }
}

/// The status of secure boot `configured` for the next boot and `active` in
/// the current one. A response missing either is not ready yet.
fn status(
    configured: Option<bool>,
    active: Option<bool>,
) -> Result<SecureBootStatus, PlatformError> {
    let (Some(configured), Some(active)) = (configured, active) else {
        return Err(PlatformError::NotReady {
            message: "SecureBootEnable or SecureBootCurrentBoot is absent".to_string(),
        });
    };
    Ok(if configured != active {
        SecureBootStatus::Pending
    } else if configured {
        SecureBootStatus::Enabled
    } else {
        SecureBootStatus::Disabled
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn libredfish_status_needs_both_secure_boot_fields() {
        let absent = || PlatformError::NotReady {
            message: "SecureBootEnable or SecureBootCurrentBoot is absent".to_string(),
        };
        value_scenarios!(run = |(configured, active)| status(configured, active);
            "settled" {
                (Some(true), Some(true)) => Ok(SecureBootStatus::Enabled),
                (Some(false), Some(false)) => Ok(SecureBootStatus::Disabled),
            }
            "applies on the next boot" {
                (Some(true), Some(false)) => Ok(SecureBootStatus::Pending),
                (Some(false), Some(true)) => Ok(SecureBootStatus::Pending),
            }
            "incomplete response" {
                (None, Some(false)) => Err(absent()),
                (Some(false), None) => Err(absent()),
            }
        );
    }
}
