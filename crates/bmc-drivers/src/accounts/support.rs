/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Shared Redfish account mechanics on a driver's operation context.

use std::sync::Arc;

use bmc_platform::{AuthError, DriverOutcome, OpCx, PlatformError};
use nv_redfish::account::{
    Account, AccountCollection, AccountService, AccountServiceConfig, AccountServiceUpdate,
    ManagerAccountCreate, ManagerAccountUpdate,
};
use nv_redfish::core::{Bmc, ODataId};
use nv_redfish::schema::manager_account::ManagerAccount;
use serde::de::IgnoredAny;

/// The standard account collection, addressed directly when the BMC refuses
/// the reads that would discover it.
const ACCOUNTS: &str = "/redfish/v1/AccountService/Accounts";

/// Redfish resources and workflows shared by account driver implementations.
///
/// These helpers use the configuration supplied by the driver; they do not
/// select or dispatch a platform's account capability.
pub(super) trait RedfishAccountsExt<B: Bmc> {
    /// Discovers the account collection with the requested listing and slot behavior.
    async fn account_collection(
        &self,
        config: AccountServiceConfig,
    ) -> Result<AccountCollection<B>, PlatformError>;

    /// Finds a username among the accounts visible under `config`.
    async fn account_by_username(
        &self,
        config: AccountServiceConfig,
        username: &str,
    ) -> Result<Account<B>, PlatformError>;

    /// The accounts visible under `config`.
    async fn list_accounts(
        &self,
        config: AccountServiceConfig,
    ) -> Result<Vec<Arc<ManagerAccount>>, PlatformError>;

    /// Creates an account in the collection discovered under `config`.
    async fn create_account(
        &self,
        config: AccountServiceConfig,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Deletes the account named `username` among those visible under `config`.
    async fn delete_account(
        &self,
        config: AccountServiceConfig,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Sets a password using standard lookup, including disabled account slots.
    ///
    /// If lookup fails with `PasswordChangeRequired`, patches the account URI
    /// supplied by the BMC, or uses `factory_account_id` when no URI is supplied.
    /// Without either, returns the password-change-required error.
    async fn set_account_password(
        &self,
        username: &str,
        password: &str,
        factory_account_id: Option<&str>,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Applies the driver's account-service policy payload.
    async fn apply_account_policy(
        &self,
        body: AccountServiceUpdate,
    ) -> Result<DriverOutcome, PlatformError>;
}

impl<B: Bmc> RedfishAccountsExt<B> for OpCx<'_, B> {
    async fn account_collection(
        &self,
        config: AccountServiceConfig,
    ) -> Result<AccountCollection<B>, PlatformError> {
        account_service(self, config)
            .await?
            .accounts()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)
    }

    async fn account_by_username(
        &self,
        config: AccountServiceConfig,
        username: &str,
    ) -> Result<Account<B>, PlatformError> {
        self.account_collection(config)
            .await?
            .all_accounts_data()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .into_iter()
            .find(|account| account.raw().user_name.as_deref() == Some(username))
            .ok_or_else(|| PlatformError::UserNotFound {
                identifier: username.to_string(),
            })
    }

    async fn list_accounts(
        &self,
        config: AccountServiceConfig,
    ) -> Result<Vec<Arc<ManagerAccount>>, PlatformError> {
        let accounts = self
            .account_collection(config)
            .await?
            .all_accounts_data()
            .await
            .map_err(|error| self.map_redfish_error(error))?;
        Ok(accounts.into_iter().map(|account| account.raw()).collect())
    }

    async fn create_account(
        &self,
        config: AccountServiceConfig,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        self.account_collection(config)
            .await?
            .create_account(request)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn delete_account(
        &self,
        config: AccountServiceConfig,
        username: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        self.account_by_username(config, username)
            .await?
            .delete()
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn set_account_password(
        &self,
        username: &str,
        password: &str,
        factory_account_id: Option<&str>,
    ) -> Result<DriverOutcome, PlatformError> {
        match self
            .account_by_username(AccountServiceConfig::standard(), username)
            .await
        {
            Ok(account) => account
                .update_password(password.to_string())
                .await
                .map(DriverOutcome::from)
                .map_err(|error| self.map_redfish_error(error)),
            Err(PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri })) => {
                match account_uri
                    .or_else(|| factory_account_id.map(|id| format!("{ACCOUNTS}/{id}")))
                {
                    Some(account_uri) => set_required_password(self, account_uri, password).await,
                    None => Err(PlatformError::Auth(AuthError::PasswordChangeRequired {
                        account_uri: None,
                    })),
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn apply_account_policy(
        &self,
        body: AccountServiceUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        account_service(self, AccountServiceConfig::standard())
            .await?
            .update(&body)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }
}

async fn account_service<B: Bmc>(
    cx: &OpCx<'_, B>,
    config: AccountServiceConfig,
) -> Result<AccountService<B>, PlatformError> {
    cx.service_root()
        .account_service(config)
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// PATCHes the password straight onto `account_uri`, without the reads a
/// BMC in factory state refuses.
async fn set_required_password<B: Bmc>(
    cx: &OpCx<'_, B>,
    account_uri: String,
    password: &str,
) -> Result<DriverOutcome, PlatformError> {
    cx.bmc()
        .update::<_, IgnoredAny>(
            &ODataId::from(account_uri),
            None,
            &ManagerAccountUpdate::builder()
                .with_password(password.to_string())
                .build(),
        )
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_bmc_error(error))
}
