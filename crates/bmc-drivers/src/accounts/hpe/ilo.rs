/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Accounts, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::AccountServiceUpdate;
use nv_redfish::core::Bmc;
use nv_redfish::oem::hpe::{HpeAccountServiceUpdate, HpeAccountServiceUpdateExt};

use crate::accounts::standard::StandardAccounts;
use crate::accounts::support::RedfishAccountsExt as _;

/// HPE iLO: the lockout policy lives under `Oem.Hpe`.
pub(crate) struct IloAccounts;

#[async_trait]
impl<B: Bmc> Accounts<B> for IloAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        _account_username: &str,
        _password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        let policy = AccountServiceUpdate::builder()
            .build()
            .with_oem_hpe(
                HpeAccountServiceUpdate::builder()
                    .with_auth_failure_delay_time_seconds(2)
                    .with_auth_failure_logging_threshold(0)
                    .with_auth_failures_before_delay(0)
                    .with_enforce_password_complexity(false)
                    .build(),
            )
            .map_err(|error| PlatformError::InvalidResponse {
                message: format!("failed to build the HPE account policy: {error}"),
            })?;
        cx.apply_account_policy(policy).await
    }
}
