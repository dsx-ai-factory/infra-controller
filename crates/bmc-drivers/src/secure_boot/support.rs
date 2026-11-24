/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Secure Boot resource discovery shared by its operations.

use bmc_platform::{OpCx, PlatformError};
use nv_redfish::certificate::CertificateCollection;
use nv_redfish::computer_system::SecureBoot as SecureBootResource;
use nv_redfish::core::Bmc;

const PLATFORM_KEY_DATABASE: &str = "PK";

/// Secure Boot resource and platform-key discovery.
pub(super) trait RedfishSecureBootExt<B: Bmc> {
    /// The selected system's Secure Boot resource.
    async fn secure_boot_resource(&self) -> Result<SecureBootResource<B>, PlatformError>;

    /// The certificates of the `PK` Secure Boot database.
    async fn platform_key_certificates(&self) -> Result<CertificateCollection<B>, PlatformError>;
}

impl<B: Bmc> RedfishSecureBootExt<B> for OpCx<'_, B> {
    async fn secure_boot_resource(&self) -> Result<SecureBootResource<B>, PlatformError> {
        self.system()
            .await?
            .secure_boot()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)
    }

    async fn platform_key_certificates(&self) -> Result<CertificateCollection<B>, PlatformError> {
        let databases = self
            .secure_boot_resource()
            .await?
            .databases()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))?;
        databases
            .iter()
            .find(|database| database.raw().id == PLATFORM_KEY_DATABASE)
            .ok_or(PlatformError::Unsupported)?
            .certificates()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)
    }
}
