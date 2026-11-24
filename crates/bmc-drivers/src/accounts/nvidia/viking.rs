/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Accounts, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::AccountServiceUpdate;
use nv_redfish::core::Bmc;

use crate::accounts::standard::StandardAccounts;
use crate::accounts::support::RedfishAccountsExt as _;

/// NVIDIA DGX Viking: the firmware rejects a fully disabled lockout, so the
/// policy keeps a short, self-resetting one. Like other AMI firmware, a
/// factory-state demand for a password change without an account means the
/// administrator, account 2.
pub(crate) struct VikingAccounts;

#[async_trait]
impl<B: Bmc> Accounts<B> for VikingAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.set_account_password(username, password, Some("2")).await
    }

    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        _account_username: &str,
        _password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.apply_account_policy(
            AccountServiceUpdate::builder()
                .with_account_lockout_threshold(4)
                .with_account_lockout_duration(20)
                .with_account_lockout_counter_reset_after(20)
                .with_account_lockout_counter_reset_enabled(true)
                .with_auth_failure_logging_threshold(2)
                .build(),
        )
        .await
    }
}
