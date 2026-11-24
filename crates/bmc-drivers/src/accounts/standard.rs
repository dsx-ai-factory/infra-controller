/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish account operations.

use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{Accounts, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::{AccountServiceConfig, AccountServiceUpdate, ManagerAccountCreate};
use nv_redfish::core::Bmc;
use nv_redfish::schema::manager_account::ManagerAccount;

use crate::accounts::support::RedfishAccountsExt as _;

/// Redfish-standard account operations.
pub(crate) struct StandardAccounts;

#[async_trait]
impl<B: Bmc> Accounts<B> for StandardAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        self
    }

    async fn list(&self, cx: &OpCx<'_, B>) -> Result<Vec<Arc<ManagerAccount>>, PlatformError> {
        cx.list_accounts(AccountServiceConfig::standard()).await
    }

    async fn create(
        &self,
        cx: &OpCx<'_, B>,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.create_account(AccountServiceConfig::standard(), request)
            .await
    }

    async fn delete(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.delete_account(AccountServiceConfig::standard(), username)
            .await
    }

    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.set_account_password(username, password, None).await
    }

    async fn change_username(
        &self,
        cx: &OpCx<'_, B>,
        old_username: &str,
        new_username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.account_by_username(AccountServiceConfig::standard(), old_username)
            .await?
            .update_user_name(new_username.to_string())
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    /// Disables account lockout so NICo cannot lock itself out during automation.
    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        _account_username: &str,
        _password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.apply_account_policy(
            AccountServiceUpdate::builder()
                .with_account_lockout_threshold(0)
                .with_account_lockout_duration(0)
                .with_account_lockout_counter_reset_after(0)
                .build(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, FixtureBmc, body, path};

    #[tokio::test]
    async fn factory_password_change_patches_the_account_the_bmc_names() {
        let bmc = factory_bmc().await;
        let cx = bmc.lazy_cx();

        assert_eq!(
            StandardAccounts
                .change_password(&cx, "root", "new-password")
                .await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].method, Method::PATCH);
        assert_eq!(path(&writes[0]), ACCOUNT);
        assert_eq!(body(&writes[0]), json!({"Password": "new-password"}));
        // AMI firmware rejects the first password change without `If-Match: *`.
        assert_eq!(
            writes[0]
                .headers
                .get("If-Match")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
    }

    const ACCOUNT: &str = "/redfish/v1/AccountService/Accounts/root";

    /// A BMC in factory state: every read but the service root answers
    /// `PasswordChangeRequired` naming the account to change.
    async fn factory_bmc() -> FixtureBmc {
        let password_change_required = json!({"error": {
            "code": "Base.1.18.1.GeneralError",
            "@Message.ExtendedInfo": [{
                "MessageId": "Base.1.18.1.PasswordChangeRequired",
                "MessageArgs": [ACCOUNT]
            }]
        }});
        Fixture::new("NVIDIA", "GB200 NVL", "System_0", "BMC_0")
            .document(
                "/redfish/v1",
                json!({
                    "@odata.id": "/redfish/v1",
                    "Id": "RootService",
                    "Name": "Root Service",
                    "Systems": {"@odata.id": "/redfish/v1/Systems"},
                    "AccountService": {"@odata.id": "/redfish/v1/AccountService"},
                    "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                }),
            )
            .respond(
                Method::GET,
                "/redfish/v1/Systems",
                StatusCode::FORBIDDEN,
                Some(password_change_required.clone()),
            )
            .respond(
                Method::GET,
                "/redfish/v1/AccountService",
                StatusCode::FORBIDDEN,
                Some(password_change_required),
            )
            .build()
            .await
    }
}
