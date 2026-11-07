/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Accounts, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::{
    AccountServiceConfig, AccountServiceUpdate, AccountTypes, ManagerAccountUpdate,
};
use nv_redfish::core::Bmc;
use nv_redfish::oem::lenovo::account_service::{
    LenovoAccountServiceUpdate, LenovoAccountServiceUpdateExt,
};

use crate::accounts::standard::StandardAccounts;
use crate::accounts::support::RedfishAccountsExt as _;

/// Lenovo XCC rejects a zero lockout duration and enforces password rotation
/// through `Oem.Lenovo`, which the default policy disables. A factory-state
/// demand for a password change without an account means the factory
/// administrator, account 1.
///
/// XCC leaves IPMI out of an account's interfaces by default, a factory reset
/// included, and adds it only in a request carrying the account's password,
/// which a factory BMC refuses alongside its first password change. NICo uses
/// IPMI on every XCC, so the default policy, which runs as the account once
/// its password has changed, sets that password again with IPMI. XCC refuses
/// IPMI for a password longer than 20 characters.
pub(crate) struct XccAccounts;

/// Adds IPMI to `username`'s interfaces, sending `password` with it. An
/// account that already has IPMI is left as it is, as is one that reports no
/// interfaces, since writing IPMI alone would replace them.
async fn grant_ipmi<B: Bmc>(
    cx: &OpCx<'_, B>,
    username: &str,
    password: &str,
) -> Result<DriverOutcome, PlatformError> {
    let account = cx
        .account_by_username(AccountServiceConfig::standard(), username)
        .await?;
    let Some(mut types) = account.raw().account_types.clone() else {
        return Ok(DriverOutcome::complete());
    };
    if types.contains(&AccountTypes::Ipmi) {
        return Ok(DriverOutcome::complete());
    }
    types.push(AccountTypes::Ipmi);
    let update = ManagerAccountUpdate::builder()
        .with_password(password.to_string())
        .with_account_types(types)
        .build();
    account
        .update(&update)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Accounts<B> for XccAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.set_account_password(username, password, Some("1")).await
    }

    async fn apply_default_policy(
        &self,
        cx: &OpCx<'_, B>,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        let policy = AccountServiceUpdate::builder()
            .with_account_lockout_threshold(0)
            .with_account_lockout_duration(60)
            .build()
            .with_oem_lenovo(
                LenovoAccountServiceUpdate::builder()
                    .with_password_expiration_period_days(0)
                    .with_password_change_on_first_access(false)
                    .with_minimum_password_change_interval_hours(0)
                    .with_minimum_password_reuse_cycle(0)
                    .with_password_expiration_warning_period(0)
                    .build(),
            )
            .map_err(|error| PlatformError::InvalidResponse {
                message: format!("failed to build the Lenovo account policy: {error}"),
            })?;
        let applied = cx.apply_account_policy(policy).await?;
        let granted = grant_ipmi(cx, account_username, password).await?;
        Ok(applied.merge(granted))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{Fixture, FixtureBmc, body, path};

    const SERVICE: &str = "/redfish/v1/AccountService";
    const ACCOUNTS: &str = "/redfish/v1/AccountService/Accounts";
    const FACTORY_ACCOUNT: &str = "/redfish/v1/AccountService/Accounts/1";

    /// An XCC whose factory administrator has `interfaces`.
    async fn xcc(interfaces: Value) -> FixtureBmc {
        Fixture::new("Lenovo", "", "1", "1")
            .document(
                "/redfish/v1",
                json!({"@odata.id": "/redfish/v1", "Id": "RootService", "Name": "Root Service",
                       "Vendor": "Lenovo",
                       "Systems": {"@odata.id": "/redfish/v1/Systems"},
                       "Managers": {"@odata.id": "/redfish/v1/Managers"},
                       "AccountService": {"@odata.id": SERVICE},
                       "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}}}),
            )
            .document(
                SERVICE,
                json!({"@odata.id": SERVICE, "@odata.etag": "W/\"1\"", "Id": "AccountService",
                       "Name": "Account Service", "Accounts": {"@odata.id": ACCOUNTS}}),
            )
            .document(
                ACCOUNTS,
                json!({"@odata.id": ACCOUNTS, "Name": "Accounts",
                       "@odata.type": "#ManagerAccountCollection.ManagerAccountCollection",
                       "Members": [{"@odata.id": FACTORY_ACCOUNT}]}),
            )
            .document(
                FACTORY_ACCOUNT,
                json!({"@odata.id": FACTORY_ACCOUNT, "@odata.etag": "W/\"2\"",
                       "@odata.type": "#ManagerAccount.v1_8_0.ManagerAccount",
                       "Id": "1", "Name": "User Account", "UserName": "USERID",
                       "RoleId": "Administrator", "Enabled": true,
                       "AccountTypes": interfaces}),
            )
            .build()
            .await
    }

    /// XCC 3 IHX438R refuses `0.0` for its rotation periods with
    /// `PropertyValueTypeError`, so they are written as integers. It adds
    /// IPMI to an account only with the account's password, so the policy
    /// then sets the password again with IPMI, unless the account already
    /// has IPMI or reports no interfaces.
    #[tokio::test]
    async fn the_policy_writes_integer_periods_and_grants_ipmi() {
        let xcc3 = || json!(["Redfish", "ManagerConsole", "WebUI"]);
        let with_ipmi = || json!(["Redfish", "ManagerConsole", "WebUI", "IPMI"]);
        for (scenario, interfaces, grant) in [
            (
                "IPMI missing",
                xcc3(),
                Some(json!({"Password": "password", "AccountTypes": with_ipmi()})),
            ),
            ("IPMI already granted", with_ipmi(), None),
            ("no reported interfaces", Value::Null, None),
        ] {
            let bmc = xcc(interfaces).await;
            let cx = bmc.cx().await;
            XccAccounts
                .apply_default_policy(&cx, "USERID", "password")
                .await
                .expect("the policy is written");
            let writes = bmc.writes();
            assert_eq!(path(&writes[0]), SERVICE, "{scenario}");
            assert_eq!(
                body(&writes[0])["Oem"]["Lenovo"],
                json!({
                    "PasswordExpirationPeriodDays": 0,
                    "PasswordChangeOnFirstAccess": false,
                    "MinimumPasswordChangeIntervalHours": 0,
                    "MinimumPasswordReuseCycle": 0,
                    "PasswordExpirationWarningPeriod": 0,
                }),
                "{scenario}"
            );
            let granted: Vec<_> = writes[1..]
                .iter()
                .map(|write| (path(write).to_string(), body(write)))
                .collect();
            let expected: Vec<_> = grant
                .into_iter()
                .map(|body| (FACTORY_ACCOUNT.to_string(), body))
                .collect();
            assert_eq!(granted, expected, "{scenario}");
        }
    }
}
