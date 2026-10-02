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

use async_trait::async_trait;
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::core::Bmc;
use nv_redfish::schema::manager_account::ManagerAccount;

use crate::{DriverOutcome, OpCx, PlatformError};

/// BMC local-account and account-policy operations.
#[async_trait]
pub trait Accounts<B: Bmc>: Send + Sync {
    async fn list(&self, cx: &OpCx<'_, B>) -> Result<Vec<ManagerAccount>, PlatformError>;

    async fn create(
        &self,
        cx: &OpCx<'_, B>,
        request: &ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn delete(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Changes the named account's password.
    ///
    /// The driver resolves vendor account IDs and handles first-password-change
    /// paths where account lookup is unavailable.
    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn change_username(
        &self,
        cx: &OpCx<'_, B>,
        old_username: &str,
        new_username: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn apply_default_policy(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;
}
