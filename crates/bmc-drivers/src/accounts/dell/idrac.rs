/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{Accounts, AuthError, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::{AccountServiceConfig, ManagerAccountCreate};
use nv_redfish::core::{Bmc, ODataId};
use nv_redfish::oem::dell::IdracVersion;
use nv_redfish::schema::manager_account::ManagerAccount;
use serde::de::IgnoredAny;
use serde_json::json;

use crate::accounts::standard::StandardAccounts;
use crate::accounts::support::RedfishAccountsExt as _;

/// Dell iDRAC account behavior.
///
/// iDRAC8 and iDRAC9 accounts are fixed slots: creation enables a free slot
/// rather than POSTing to the collection, deletion disables the slot, and
/// listing hides disabled slots. Every password-policy property is read-only.
pub(crate) struct IdracAccounts;

/// The account slot of iDRAC's factory `root` account.
const FACTORY_ACCOUNT_SLOT: u8 = 2;

/// Sets the factory account's password through the iDRAC attributes resource
/// at `attributes_uri`, which takes it as the slot's `Users.<slot>.Password`
/// attribute rather than an account's `Password`.
async fn set_factory_password<B: Bmc>(
    cx: &OpCx<'_, B>,
    attributes_uri: String,
    password: &str,
) -> Result<DriverOutcome, PlatformError> {
    let body = json!({"Attributes": {
        format!("Users.{FACTORY_ACCOUNT_SLOT}.Password"): password,
    }});
    cx.bmc()
        .update::<_, IgnoredAny>(&ODataId::from(attributes_uri), None, &body)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_bmc_error(error))
}

#[async_trait]
impl<B: Bmc> Accounts<B> for IdracAccounts {
    fn standard(&self) -> &dyn Accounts<B> {
        &StandardAccounts
    }

    /// In factory state iDRAC refuses every read until the factory password
    /// changes, and names its attributes resource as where to change it.
    async fn change_password(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match cx
            .account_by_username(AccountServiceConfig::standard(), username)
            .await
        {
            Err(PlatformError::Auth(AuthError::PasswordChangeRequired {
                account_uri: Some(uri),
            })) if uri.contains("/DellAttributes/") => {
                set_factory_password(cx, uri, password).await
            }
            _ => {
                self.standard()
                    .change_password(cx, username, password)
                    .await
            }
        }
    }

    async fn list(&self, cx: &OpCx<'_, B>) -> Result<Vec<Arc<ManagerAccount>>, PlatformError> {
        cx.list_accounts(slot_config(cx).await?).await
    }

    async fn create(
        &self,
        cx: &OpCx<'_, B>,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.create_account(slot_config(cx).await?, request).await
    }

    async fn delete(
        &self,
        cx: &OpCx<'_, B>,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        cx.delete_account(slot_config(cx).await?, username).await
    }

    async fn apply_default_policy(
        &self,
        _cx: &OpCx<'_, B>,
        _account_username: &str,
        _password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
    }
}

/// The account behavior of this iDRAC's version; an unrecognized manager
/// model keeps the iDRAC9 fixed slots.
async fn slot_config<B: Bmc>(cx: &OpCx<'_, B>) -> Result<AccountServiceConfig, PlatformError> {
    Ok(IdracVersion::from_manager(cx.manager().await?)
        .unwrap_or(IdracVersion::IDRAC9)
        .account_service_config())
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const ATTRIBUTES: &str =
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1";

    /// A factory iDRAC refuses reads and names its attributes resource, which
    /// takes the password as the factory slot's attribute.
    #[tokio::test]
    async fn a_factory_password_changes_through_the_named_attributes() {
        let password_change_required = json!({"error": {
            "code": "Base.1.12.GeneralError",
            "@Message.ExtendedInfo": [{
                "MessageId": "Base.1.12.PasswordChangeRequired",
                "MessageArgs": [ATTRIBUTES],
                "Severity": "Critical",
            }]
        }});
        let bmc = Fixture::new(
            "Dell",
            "Integrated Dell Remote Access Controller",
            "System.Embedded.1",
            "iDRAC.Embedded.1",
        )
        .document(
            "/redfish/v1",
            json!({
                "@odata.id": "/redfish/v1",
                "Id": "RootService",
                "Name": "Root Service",
                "Vendor": "Dell",
                "AccountService": {"@odata.id": "/redfish/v1/AccountService"},
                "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
            }),
        )
        .respond(
            Method::GET,
            "/redfish/v1/AccountService",
            StatusCode::FORBIDDEN,
            Some(password_change_required),
        )
        .build()
        .await;
        let cx = bmc.lazy_cx();

        assert_eq!(
            IdracAccounts
                .change_password(&cx, "root", "new-password")
                .await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].method, Method::PATCH);
        assert_eq!(path(&writes[0]), ATTRIBUTES);
        assert_eq!(
            body(&writes[0]),
            json!({"Attributes": {"Users.2.Password": "new-password"}})
        );
    }
}
