/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, SecureBoot, SecureBootStatus};
use nv_redfish::certificate::{CertificateCreate, CertificateType};
use nv_redfish::computer_system::SecureBootCurrentBootType;
use nv_redfish::core::Bmc;
use nv_redfish::schema::secure_boot::SecureBootUpdate;

use crate::secure_boot::support::RedfishSecureBootExt as _;

/// Standard Redfish Secure Boot state and platform-key operations.
pub(crate) struct StandardSecureBoot;

#[async_trait]
impl<B: Bmc> SecureBoot<B> for StandardSecureBoot {
    fn standard(&self) -> &dyn SecureBoot<B> {
        self
    }

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<SecureBootStatus, PlatformError> {
        let secure_boot = cx.secure_boot_resource().await?;
        normalize_status(
            secure_boot.secure_boot_enable(),
            secure_boot.secure_boot_current_boot(),
        )
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        update: &SecureBootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.secure_boot_resource()
            .await?
            .update(update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn has_platform_key(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(!cx
            .platform_key_certificates()
            .await?
            .raw()
            .members
            .is_empty())
    }

    async fn add_platform_key(
        &self,
        cx: &OpCx<'_, B>,
        pem: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        if pem.trim().is_empty() {
            return Err(PlatformError::InvalidResponse {
                message: "platform key PEM is empty".to_string(),
            });
        }
        cx.platform_key_certificates()
            .await?
            .create(&CertificateCreate::builder(pem.to_string(), CertificateType::Pem).build())
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }
}

fn normalize_status(
    configured: Option<bool>,
    active: Option<SecureBootCurrentBootType>,
) -> Result<SecureBootStatus, PlatformError> {
    let Some(configured) = configured else {
        return Err(PlatformError::InvalidResponse {
            message: "SecureBootEnable is absent".to_string(),
        });
    };
    let active = match active {
        Some(SecureBootCurrentBootType::Enabled) => Some(true),
        Some(SecureBootCurrentBootType::Disabled) => Some(false),
        Some(SecureBootCurrentBootType::UnsupportedValue) => {
            return Err(PlatformError::InvalidResponse {
                message: "SecureBootCurrentBoot has an unsupported value".to_string(),
            });
        }
        None => None,
    };
    Ok(match active {
        Some(active) if active != configured => SecureBootStatus::Pending,
        _ if configured => SecureBootStatus::Enabled,
        _ => SecureBootStatus::Disabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_state_that_differs_from_active_is_pending() {
        assert_eq!(
            normalize_status(Some(true), Some(SecureBootCurrentBootType::Disabled)).unwrap(),
            SecureBootStatus::Pending
        );
    }
}
