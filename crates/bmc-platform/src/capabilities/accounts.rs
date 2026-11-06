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

use std::sync::Arc;

use async_trait::async_trait;
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::core::Bmc;
use nv_redfish::schema::manager_account::ManagerAccount;

use crate::{DriverOutcome, OpCx, PlatformError};

/// BMC local-account and account-policy operations.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait Accounts<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn Accounts<B>;

    async fn list(&self, cx: &OpCx<'_, B>) -> Result<Vec<Arc<ManagerAccount>>, PlatformError> {
        self.standard().list(cx).await
    }

    async fn create(
        &self,
        cx: &OpCx<'_, B>,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().create(cx, request).await
    }

    async fn delete(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().delete(cx, username).await
    }

    /// Changes the named account's password.
    ///
    /// The driver resolves vendor account IDs and handles first-password-change
    /// paths where account lookup is unavailable.
    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .change_password(cx, account_username, password)
            .await
    }

    async fn change_username(
        &self,
        cx: &OpCx<'_, B>,
        old_username: &str,
        new_username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .change_username(cx, old_username, new_username)
            .await
    }

    /// Applies the platform's account policy.
    ///
    /// `account_username` and `password` are the account the operation runs
    /// as and its current password, for platforms whose policy also sets up
    /// that account and needs its password to. A password other than the
    /// current one changes the account's password.
    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .apply_default_policy(cx, account_username, password)
            .await
    }
}
