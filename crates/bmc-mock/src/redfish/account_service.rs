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

use std::borrow::Cow;
use std::fmt::Display;
use std::sync::{Arc, Mutex, Weak};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures::future::BoxFuture;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{http, redfish};

pub(crate) fn resource() -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/AccountService"),
        odata_type: Cow::Borrowed("#AccountService.v1_9_0.AccountService"),
        id: Cow::Borrowed("AccountService"),
        name: Cow::Borrowed("Account Service"),
    }
}

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    r.route(&resource().odata_id, get(get_root).patch(patch_root))
        .route(
            &ACCOUNTS_COLLECTION_RESOURCE.odata_id,
            get(get_accounts).post(create_account),
        )
        .route(
            format!("{}/{{account_id}}", ACCOUNTS_COLLECTION_RESOURCE.odata_id).as_str(),
            get(get_account).patch(patch_account),
        )
}

const ACCOUNTS_COLLECTION_RESOURCE: redfish::Collection<'static> = redfish::Collection {
    odata_id: Cow::Borrowed("/redfish/v1/AccountService/Accounts"),
    odata_type: Cow::Borrowed("#ManagerAccountCollection.ManagerAccountCollection"),
    name: Cow::Borrowed("Accounts Collection"),
};
const ADMINISTRATOR_ROLE_ID: &str = "Administrator";

#[derive(Debug)]
pub struct AccountServiceState {
    accounts: Mutex<Vec<Account>>,
    password_updater: Mutex<Option<Weak<dyn PasswordUpdater>>>,
    credentials_changed_notifier: Mutex<Option<Arc<dyn CredentialsChangedNotifier>>>,
}

pub(crate) trait PasswordUpdater: Send + Sync {
    fn update_password<'a>(
        &'a self,
        username: &'a str,
        current_password: &'a str,
        new_password: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// Notified with a full credential export after every effective password
/// change, so callers can persist credentials across restarts (issue #5966).
pub trait CredentialsChangedNotifier: std::fmt::Debug + Send + Sync {
    fn credentials_changed(&self, credentials: Vec<BmcAccountCredential>);
}

/// A snapshot of one BMC account's current password, suitable for durable
/// persistence and later restoration after a mock rebuild.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BmcAccountCredential {
    pub account_id: String,
    pub username: String,
    pub password: String,
}

impl AccountServiceState {
    pub(crate) fn new(factory_default_account: Account) -> Self {
        Self {
            accounts: Mutex::new(vec![factory_default_account]),
            password_updater: Mutex::new(None),
            credentials_changed_notifier: Mutex::new(None),
        }
    }

    pub(crate) fn set_password_updater(&self, updater: &Arc<dyn PasswordUpdater>) {
        *self.password_updater.lock().expect("mutex poisoned") = Some(Arc::downgrade(updater));
    }

    pub fn set_credentials_changed_notifier(&self, notifier: Arc<dyn CredentialsChangedNotifier>) {
        *self
            .credentials_changed_notifier
            .lock()
            .expect("mutex poisoned") = Some(notifier);
    }

    /// Exports the current password of every account for durable persistence.
    pub fn export_credentials(&self) -> Vec<BmcAccountCredential> {
        self.accounts
            .lock()
            .expect("mutex poisoned")
            .iter()
            .map(|account| BmcAccountCredential {
                account_id: account.id.clone(),
                username: account.username.clone(),
                password: account.password.clone(),
            })
            .collect()
    }

    /// Restores previously exported passwords onto matching accounts. Accounts
    /// are matched by id and username; the factory-default password recorded at
    /// construction is left untouched so factory-default detection still works.
    pub fn restore_credentials(&self, credentials: &[BmcAccountCredential]) {
        let mut accounts = self.accounts.lock().expect("mutex poisoned");
        for credential in credentials {
            if let Some(account) = accounts.iter_mut().find(|account| {
                account.id == credential.account_id && account.username == credential.username
            }) {
                account.password = credential.password.clone();
            }
        }
    }

    /// Must be called WITHOUT holding the `accounts` lock: the notifier is
    /// invoked synchronously and `export_credentials` retakes that lock.
    fn notify_credentials_changed(&self) {
        let notifier = self
            .credentials_changed_notifier
            .lock()
            .expect("mutex poisoned")
            .clone();
        if let Some(notifier) = notifier {
            notifier.credentials_changed(self.export_credentials());
        }
    }

    pub(crate) fn accounts(&self) -> Vec<Account> {
        self.accounts.lock().expect("mutex poisoned").clone()
    }

    pub(crate) fn find(&self, account_id: &str) -> Option<Account> {
        self.accounts
            .lock()
            .expect("mutex poisoned")
            .iter()
            .find(|account| account.id == account_id)
            .cloned()
    }

    pub(crate) fn administrator_credentials(&self) -> Option<(String, String)> {
        self.accounts
            .lock()
            .expect("mutex poisoned")
            .iter()
            .find(|account| account.role_id == ADMINISTRATOR_ROLE_ID)
            .map(|account| (account.username.clone(), account.password.clone()))
    }

    pub(crate) fn is_authorized(&self, username: &str, password: &str) -> bool {
        self.accounts
            .lock()
            .expect("mutex poisoned")
            .iter()
            .any(|account| account.matches(username, password))
    }

    pub(crate) fn is_factory_default_password(&self, username: &str, password: &str) -> bool {
        self.accounts
            .lock()
            .expect("mutex poisoned")
            .iter()
            .any(|account| account.matches_factory_default_password(username, password))
    }

    pub(crate) async fn update_password(
        &self,
        account_id: &str,
        password: impl Into<String>,
    ) -> Result<bool, String> {
        let password = password.into();
        let account = self.find(account_id);
        let Some(account) = account else {
            return Ok(false);
        };
        let updater = self
            .password_updater
            .lock()
            .expect("mutex poisoned")
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(updater) = updater {
            updater
                .update_password(&account.username, &account.password, &password)
                .await?;
        }

        {
            let mut accounts = self.accounts.lock().expect("mutex poisoned");
            let account = accounts
                .iter_mut()
                .find(|candidate| candidate.id == account_id)
                .expect("account existed before password synchronization");
            account.password = password;
        }
        self.notify_credentials_changed();
        Ok(true)
    }

    /// Rotates every account on its factory default password to `new_password`
    pub fn change_factory_default_password(&self, new_password: impl Into<String>) {
        let new_password = new_password.into();
        {
            let mut accounts = self.accounts.lock().expect("mutex poisoned");
            for account in accounts.iter_mut() {
                if account.password == account.factory_default_password {
                    account.password = new_password.clone();
                }
            }
        }
        self.notify_credentials_changed();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Account {
    id: String,
    username: String,
    password: String,
    factory_default_password: String,
    role_id: String,
}

impl Account {
    pub(crate) fn administrator(
        id: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let password = password.into();
        Self {
            id: id.into(),
            username: username.into(),
            password: password.clone(),
            factory_default_password: password,
            role_id: ADMINISTRATOR_ROLE_ID.into(),
        }
    }

    fn matches(&self, username: &str, password: &str) -> bool {
        self.username == username && self.password == password
    }

    fn matches_factory_default_password(&self, username: &str, password: &str) -> bool {
        self.matches(username, password) && self.password == self.factory_default_password
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "UserName": self.username,
            "RoleId": self.role_id,
            "AccountTypes": ["Redfish"]
        })
        .patch(account_resource(&self.id))
    }
}

async fn get_root() -> Response {
    let service_attrs = json!({
        "AccountLockoutCounterResetAfter": 0,
        "AccountLockoutDuration": 0,
        "AccountLockoutThreshold": 0,
        "AuthFailureLoggingThreshold": 2,
        "LocalAccountAuth": "Fallback",
        "MaxPasswordLength": 40,
        "MinPasswordLength": 0,
    });
    service_attrs
        .patch(resource())
        .patch(ACCOUNTS_COLLECTION_RESOURCE.nav_property("Accounts"))
        .into_ok_response()
}

async fn patch_root() -> Response {
    http::ok_no_content()
}

fn account_resource(id: impl Display) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("{}/{id}", ACCOUNTS_COLLECTION_RESOURCE.odata_id)),
        odata_type: Cow::Borrowed("#ManagerAccount.v1_8_0.ManagerAccount"),
        name: Cow::Borrowed("User Account"),
        id: Cow::Owned(id.to_string()),
    }
}

async fn get_accounts(State(state): State<BmcState>) -> Response {
    let members = state
        .account_service_state
        .accounts()
        .iter()
        .map(|account| account_resource(&account.id).entity_ref())
        .collect::<Vec<_>>();
    ACCOUNTS_COLLECTION_RESOURCE
        .with_members(&members)
        .into_ok_response()
}

async fn create_account() -> Response {
    json!({}).into_ok_response()
}

async fn patch_account(
    State(state): State<BmcState>,
    Path(account_id): Path<String>,
    Json(patch_account): Json<serde_json::Value>,
) -> Response {
    let Some(password) = patch_account
        .get("Password")
        .and_then(serde_json::Value::as_str)
    else {
        return json!("Password must be a string").into_response(StatusCode::BAD_REQUEST);
    };

    match state
        .account_service_state
        .update_password(&account_id, password)
        .await
    {
        Ok(true) => http::ok_no_content(),
        Ok(false) => http::not_found(),
        Err(error) => {
            tracing::error!(%error, %account_id, "failed to synchronize BMC account password");
            json!("Failed to synchronize BMC account password")
                .into_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_account(State(state): State<BmcState>, Path(account_id): Path<String>) -> Response {
    state
        .account_service_state
        .find(&account_id)
        .map(|account| account.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::{Account, AccountServiceState, PasswordUpdater};

    struct TestPasswordUpdater {
        result: Result<(), String>,
    }

    impl PasswordUpdater for TestPasswordUpdater {
        fn update_password<'a>(
            &'a self,
            _username: &'a str,
            _current_password: &'a str,
            _new_password: &'a str,
        ) -> BoxFuture<'a, Result<(), String>> {
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }

    fn state_with_updater(
        result: Result<(), String>,
    ) -> (AccountServiceState, Arc<dyn PasswordUpdater>) {
        let state = AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        let updater: Arc<dyn PasswordUpdater> = Arc::new(TestPasswordUpdater { result });
        state.set_password_updater(&updater);
        (state, updater)
    }

    #[tokio::test]
    async fn update_password_commits_after_ipmi_update_succeeds() {
        let (state, _updater) = state_with_updater(Ok(()));

        assert_eq!(state.update_password("1", "new-password").await, Ok(true));
        assert!(state.is_authorized("root", "new-password"));
    }

    #[tokio::test]
    async fn rotated_password_survives_bmc_rebuild() {
        // Regression test for issue #5966: machine-a-tron loses rotated BMC
        // passwords on pod restart because AccountServiceState is in-memory only.
        let (state, _updater) = state_with_updater(Ok(()));
        assert_eq!(
            state.update_password("1", "rotated-password").await,
            Ok(true)
        );
        assert!(state.is_authorized("root", "rotated-password"));

        // The credentials exported on each password change are persisted in
        // the machine-a-tron device snapshot.
        let exported = state.export_credentials();

        // Simulate a machine-a-tron pod restart: run_bmc_mock rebuilds every
        // BMC from its factory-default configuration, then restores the
        // snapshot-saved credentials.
        let restarted =
            AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        restarted.restore_credentials(&exported);

        assert!(
            restarted.is_authorized("root", "rotated-password"),
            "rotated password must survive a BMC mock rebuild (issue #5966)"
        );
        assert!(
            !restarted.is_authorized("root", "old-password"),
            "factory-default password must stay rejected after rotation (issue #5966)"
        );
    }

    #[tokio::test]
    async fn restore_credentials_preserves_factory_default_detection() {
        let (state, _updater) = state_with_updater(Ok(()));
        assert_eq!(
            state.update_password("1", "rotated-password").await,
            Ok(true)
        );
        let exported = state.export_credentials();

        let restarted =
            AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        restarted.restore_credentials(&exported);

        // The restored password is not the factory default, and the factory
        // default recorded at construction must remain intact underneath.
        assert!(!restarted.is_factory_default_password("root", "rotated-password"));
        assert!(!restarted.is_factory_default_password("root", "old-password"));

        // A never-rotated export restores onto a fresh state as still-factory.
        let untouched =
            AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        let untouched_export = untouched.export_credentials();
        let restored_untouched =
            AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        restored_untouched.restore_credentials(&untouched_export);
        assert!(restored_untouched.is_factory_default_password("root", "old-password"));
    }

    #[tokio::test]
    async fn restore_credentials_ignores_unknown_accounts() {
        let state = AccountServiceState::new(Account::administrator("1", "root", "old-password"));
        state.restore_credentials(&[super::BmcAccountCredential {
            account_id: "2".to_string(),
            username: "other".to_string(),
            password: "whatever".to_string(),
        }]);
        assert!(state.is_authorized("root", "old-password"));
        assert!(!state.is_authorized("other", "whatever"));
    }

    #[tokio::test]
    async fn notifier_fires_with_exported_credentials_on_password_change() {
        #[derive(Debug, Default)]
        struct RecordingNotifier {
            calls: std::sync::Mutex<Vec<Vec<super::BmcAccountCredential>>>,
        }
        impl super::CredentialsChangedNotifier for RecordingNotifier {
            fn credentials_changed(&self, credentials: Vec<super::BmcAccountCredential>) {
                self.calls.lock().unwrap().push(credentials);
            }
        }

        let (state, _updater) = state_with_updater(Ok(()));
        let notifier = Arc::new(RecordingNotifier::default());
        state.set_credentials_changed_notifier(notifier.clone());

        assert_eq!(
            state.update_password("1", "rotated-password").await,
            Ok(true)
        );
        {
            let calls = notifier.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0][0].password, "rotated-password");
            assert_eq!(calls[0][0].username, "root");
        }

        state.change_factory_default_password("ignored-not-on-factory-default");
        // Fires again (rotation attempt), but the already-rotated password is untouched.
        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1][0].password, "rotated-password");
    }

    #[tokio::test]
    async fn notifier_does_not_fire_when_ipmi_update_fails() {
        #[derive(Debug, Default)]
        struct CountingNotifier {
            count: std::sync::atomic::AtomicUsize,
        }
        impl super::CredentialsChangedNotifier for CountingNotifier {
            fn credentials_changed(&self, _credentials: Vec<super::BmcAccountCredential>) {
                self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let (state, _updater) = state_with_updater(Err("IPMI update failed".to_string()));
        let notifier = Arc::new(CountingNotifier::default());
        state.set_credentials_changed_notifier(notifier.clone());

        assert!(state.update_password("1", "new-password").await.is_err());
        assert_eq!(notifier.count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn update_password_preserves_redfish_password_when_ipmi_update_fails() {
        let (state, _updater) = state_with_updater(Err("IPMI update failed".to_string()));

        assert_eq!(
            state.update_password("1", "new-password").await,
            Err("IPMI update failed".to_string())
        );
        assert!(state.is_authorized("root", "old-password"));
        assert!(!state.is_authorized("root", "new-password"));
    }
}
