/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Accounts, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::core::Bmc;

use crate::accounts::standard::StandardAccounts;
use crate::accounts::support::RedfishAccountsExt as _;

/// Generic AMI MegaRAC: a factory-state BMC that demands a password change
/// without naming the account means the administrator, account 2, and a new
/// account is disabled and must change its password unless created otherwise.
pub(crate) struct MegaRacAccounts;

#[async_trait]
impl<B: Bmc> Accounts<B> for MegaRacAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    async fn create(
        &self,
        cx: &OpCx<'_, B>,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        let request = ManagerAccountCreate {
            enabled: Some(true),
            password_change_required: Some(false),
            ..request
        };
        self.standard().create(cx, request).await
    }

    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.set_account_password(username, password, Some("2")).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const ACCOUNTS: &str = "/redfish/v1/AccountService/Accounts";

    #[tokio::test]
    async fn new_accounts_are_enabled_without_a_forced_password_change() {
        let bmc = Fixture::new("Lenovo", "Lenovo Redfish Server", "Self", "Self")
            .document(
                "/redfish/v1",
                json!({
                    "@odata.id": "/redfish/v1", "Id": "RootService", "Name": "Root Service",
                    "AccountService": {"@odata.id": "/redfish/v1/AccountService"},
                    "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                }),
            )
            .document(
                "/redfish/v1/AccountService",
                json!({"@odata.id": "/redfish/v1/AccountService", "Id": "AccountService",
                       "Name": "Account Service", "Accounts": {"@odata.id": ACCOUNTS}}),
            )
            .document(
                ACCOUNTS,
                json!({"@odata.id": ACCOUNTS,
                       "@odata.type": "#ManagerAccountCollection.ManagerAccountCollection",
                       "Name": "Accounts", "Members": []}),
            )
            .build()
            .await;
        let cx = bmc.lazy_cx();

        assert_eq!(
            MegaRacAccounts
                .create(
                    &cx,
                    ManagerAccountCreate::builder(
                        "password".to_string(),
                        "nicoval".to_string(),
                        "Administrator".to_string(),
                    ),
                )
                .await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(writes.iter().map(path).collect::<Vec<_>>(), [ACCOUNTS]);
        let created = body(&writes[0]);
        assert_eq!(created["Enabled"], json!(true));
        assert_eq!(created["PasswordChangeRequired"], json!(false));
        assert_eq!(created["UserName"], json!("nicoval"));
    }
}
