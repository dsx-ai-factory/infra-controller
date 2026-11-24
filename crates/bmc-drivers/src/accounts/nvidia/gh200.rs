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

/// NVIDIA GH200 trays: the lockout threshold is disabled with a ten-minute
/// lockout duration. OpenBMC account ids are usernames.
pub(crate) struct Gh200Accounts;

#[async_trait]
impl<B: Bmc> Accounts<B> for Gh200Accounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.set_account_password(username, password, Some(username))
            .await
    }

    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        _account_username: &str,
        _password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.apply_account_policy(
            AccountServiceUpdate::builder()
                .with_account_lockout_threshold(0)
                .with_account_lockout_duration(600)
                .build(),
        )
        .await
    }
}
