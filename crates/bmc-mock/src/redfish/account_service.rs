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
use std::path::PathBuf;
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
use crate::{Callbacks, http, redfish};

mod persistence;
use persistence::AccountError;

pub(crate) fn resource() -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/AccountService"),
        odata_type: Cow::Borrowed("#AccountService.v1_9_0.AccountService"),
        id: Cow::Borrowed("AccountService"),
        name: Cow::Borrowed("Account Service"),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(&resource().odata_id, get(get_root).patch(patch_root))
        .route(
            &ACCOUNTS_COLLECTION_RESOURCE.odata_id,
            get(get_accounts::<C>).post(create_account),
        )
        .route(
            format!("{}/{{account_id}}", ACCOUNTS_COLLECTION_RESOURCE.odata_id).as_str(),
            get(get_account::<C>).patch(patch_account::<C>),
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
    state_file: Mutex<Option<PathBuf>>,
    update_lock: tokio::sync::Mutex<()>,
    password_updater: Mutex<Option<Weak<dyn PasswordUpdater>>>,
}

pub(crate) trait PasswordUpdater: Send + Sync {
    fn update_password<'a>(
        &'a self,
        username: &'a str,
        current_password: &'a str,
        new_password: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// A snapshot of one BMC account's current password, suitable for durable
/// persistence and later restoration after a mock rebuild.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BmcAccountCredential {
    /// Redfish account id this credential belongs to.
    pub account_id: String,
    /// Account username, used together with `account_id` to match this
    /// credential back onto an account during `restore_credentials`.
    pub username: String,
    /// The account's current password. Sensitive: this snapshot is written
    /// to disk as plaintext for restoration after a mock rebuild.
    pub password: String,
}

impl AccountServiceState {
    pub(crate) fn new(factory_default_account: Account) -> Self {
        Self {
            accounts: Mutex::new(vec![factory_default_account]),
            state_file: Mutex::new(None),
            update_lock: tokio::sync::Mutex::new(()),
            password_updater: Mutex::new(None),
        }
    }

    /// Loads account state from a JSON file, creating it with profile defaults if absent.
    ///
    /// Configure once before serving requests. The parent directory must exist; invalid or
    /// unreadable state fails without resetting credentials. Writes atomically replace the
    /// file with owner-only permissions. Use a separate file per BMC and a single writer.
    /// IPMI synchronization is unsupported. Without this call, accounts remain in memory.
    pub fn enable_persistence(&self, path: PathBuf) -> Result<(), persistence::AccountError> {
        if self
            .password_updater
            .lock()
            .expect("mutex poisoned")
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            return Err(AccountError::Invalid(
                "account persistence is unavailable with IPMI synchronization",
            ));
        }
        let mut accounts = self.accounts.lock().expect("mutex poisoned");
        let mut state_file = self.state_file.lock().expect("mutex poisoned");
        if state_file.is_some() {
            return Err(AccountError::Invalid(
                "account persistence is already configured",
            ));
        }
        let loaded = persistence::load_or_create(&path, &accounts)?;
        *accounts = loaded;
        *state_file = Some(path);
        Ok(())
    }

    fn persist(&self, accounts: &[Account]) -> Result<(), AccountError> {
        if let Some(path) = self.state_file.lock().expect("mutex poisoned").as_ref() {
            persistence::save(path, accounts)?;
        }
        Ok(())
    }

    pub(crate) fn set_password_updater(&self, updater: &Arc<dyn PasswordUpdater>) {
        *self.password_updater.lock().expect("mutex poisoned") = Some(Arc::downgrade(updater));
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
    /// Credentials that don't match any current account (e.g. from an
    /// old/stale snapshot) are silently ignored.
    pub fn restore_credentials(&self, credentials: &[BmcAccountCredential]) {
        let mut accounts = self.accounts.lock().expect("mutex poisoned");
        let mut changed = accounts.clone();
        for credential in credentials {
            if let Some(account) = changed.iter_mut().find(|account| {
                account.id == credential.account_id && account.username == credential.username
            }) {
                account.password = credential.password.clone();
            } else {
                tracing::warn!(
                    account_id = %credential.account_id,
                    "BMC credential snapshot did not match a current account"
                );
            }
        }
        if let Err(error) = self.persist(&changed) {
            tracing::warn!(%error, "BMC credentials were not restored");
            return;
        }
        *accounts = changed;
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

    async fn update_account(
        &self,
        account_id: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<(), AccountError> {
        // Serialize mutations, including the optional asynchronous IPMI synchronization.
        let _update = self.update_lock.lock().await;
        let account = self
            .find(account_id)
            .ok_or_else(|| AccountError::NotFound(account_id.into()))?;
        let updater = self
            .password_updater
            .lock()
            .expect("mutex poisoned")
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(updater) = updater {
            if username.is_some_and(|name| name != account.username)
                || self.state_file.lock().expect("mutex poisoned").is_some()
            {
                return Err(AccountError::Invalid(
                    "account rename and persistence are unavailable with IPMI synchronization",
                ));
            }
            if let Some(password) = password {
                updater
                    .update_password(&account.username, &account.password, password)
                    .await
                    .map_err(|message| {
                        AccountError::Synchronization(persistence::SynchronizationError(message))
                    })?;
            }
        }
        let mut accounts = self.accounts.lock().expect("mutex poisoned");
        if username.is_some_and(|name| {
            accounts
                .iter()
                .any(|a| a.id != account_id && a.username == name)
        }) {
            return Err(AccountError::Invalid("account username is already in use"));
        }
        let mut changed = accounts.clone();
        let account = changed
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| AccountError::NotFound(account_id.into()))?;
        if let Some(username) = username {
            account.username = username.into();
        }
        if let Some(password) = password {
            account.password = password.into();
        }
        self.persist(&changed)?;
        *accounts = changed;
        Ok(())
    }

    /// Rotates every account on its factory default password to `new_password`
    pub fn change_factory_default_password(&self, new_password: impl Into<String>) {
        let new_password = new_password.into();
        let mut accounts = self.accounts.lock().expect("mutex poisoned");
        let mut changed = accounts.clone();
        for account in changed.iter_mut() {
            if account.password == account.factory_default_password {
                account.password = new_password.clone();
            }
        }
        if let Err(error) = self.persist(&changed) {
            tracing::warn!(%error, "factory password rotation was not applied");
            return;
        }
        *accounts = changed;
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

async fn get_accounts<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
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

async fn patch_account<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(account_id): Path<String>,
    Json(patch_account): Json<serde_json::Value>,
) -> Response {
    let fields = match persistence::parse_patch(&patch_account) {
        Ok(fields) => fields,
        Err(error) => return json!(error.to_string()).into_response(StatusCode::BAD_REQUEST),
    };
    match state
        .account_service_state
        .update_account(&account_id, fields.0, fields.1)
        .await
    {
        Ok(()) => http::ok_no_content(),
        Err(AccountError::NotFound(_)) => http::not_found(),
        Err(error @ AccountError::Invalid(_)) => {
            json!(error.to_string()).into_response(StatusCode::BAD_REQUEST)
        }
        Err(error) => {
            tracing::error!(%error, %account_id, "failed to update BMC account");
            json!("Failed to update BMC account").into_response(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_account<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(account_id): Path<String>,
) -> Response {
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

        state
            .update_account("1", None, Some("new-password"))
            .await
            .unwrap();
        assert!(state.is_authorized("root", "new-password"));
    }

    #[tokio::test]
    async fn rotated_password_survives_bmc_rebuild() {
        // Regression test for issue #5966: machine-a-tron loses rotated BMC
        // passwords on pod restart because AccountServiceState is in-memory only.
        let (state, _updater) = state_with_updater(Ok(()));
        state
            .update_account("1", None, Some("rotated-password"))
            .await
            .unwrap();
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
        state
            .update_account("1", None, Some("rotated-password"))
            .await
            .unwrap();
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
    async fn update_password_preserves_redfish_password_when_ipmi_update_fails() {
        let (state, _updater) = state_with_updater(Err("IPMI update failed".to_string()));

        assert!(matches!(
            state.update_account("1", None, Some("new-password")).await,
            Err(super::AccountError::Synchronization(_))
        ));
        assert!(state.is_authorized("root", "old-password"));
        assert!(!state.is_authorized("root", "new-password"));
    }
    fn persistent_router(
        path: &std::path::Path,
    ) -> (
        axum::Router,
        crate::BmcState<crate::test_support::TestCallbacks>,
    ) {
        let (router, state) = crate::machine_router(
            &crate::test_support::host_info(crate::HardwareType::GenericAmi),
            Arc::new(crate::test_support::TestCallbacks::default()),
            "persistence-test".into(),
            true,
            crate::MachineRouterOptions::default(),
        );
        state
            .account_service_state
            .enable_persistence(path.to_owned())
            .unwrap();
        (router, state)
    }

    async fn account_request(
        router: axum::Router,
        method: &str,
        user: &str,
        password: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        use axum_extra::headers::{Authorization, HeaderMapExt};
        use tower::ServiceExt;
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri("/redfish/v1/AccountService/Accounts/2")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        request
            .headers_mut()
            .typed_insert(Authorization::basic(user, password));
        router.oneshot(request).await.unwrap()
    }

    #[tokio::test]
    async fn account_credentials_survive_reconstructed_bmc_router() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        {
            let (router, state) = persistent_router(&path);
            let original = state.account_service_state.accounts()[0].clone();
            let response = account_request(
                router.clone(),
                "PATCH",
                &original.username,
                &original.password,
                serde_json::json!({"Password":"test-password", "UserName":"test-admin"}),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(
                state
                    .account_service_state
                    .is_authorized("test-admin", "test-password")
            );
            assert!(
                !state
                    .account_service_state
                    .is_factory_default_password("test-admin", "test-password")
            );
        }
        let (router, state) = persistent_router(&path);
        assert!(
            state
                .account_service_state
                .is_authorized("test-admin", "test-password")
        );
        assert!(
            !state
                .account_service_state
                .is_authorized("root", "factory_password")
        );
        let response = account_request(
            router.clone(),
            "GET",
            "test-admin",
            "test-password",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let account: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(account["UserName"], "test-admin");
        assert_eq!(account["RoleId"], "Administrator");
        assert!(account.get("Password").is_none());
        assert_eq!(
            account_request(
                router,
                "GET",
                "root",
                "factory_password",
                serde_json::Value::Null
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn partial_updates_survive_reload_and_guests_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        let (_, state) = persistent_router(&first);
        let (_, other) = persistent_router(&second);
        let original = state.account_service_state.accounts()[0].clone();
        state
            .account_service_state
            .update_account("2", None, Some("changed-password"))
            .await
            .unwrap();
        let (_, reloaded) = persistent_router(&first);
        assert!(
            reloaded
                .account_service_state
                .is_authorized(&original.username, "changed-password")
        );
        reloaded
            .account_service_state
            .update_account("2", Some("changed-user"), None)
            .await
            .unwrap();
        let (_, reloaded) = persistent_router(&first);
        assert!(
            reloaded
                .account_service_state
                .is_authorized("changed-user", "changed-password")
        );
        assert!(
            other
                .account_service_state
                .is_authorized(&original.username, &original.password)
        );
    }

    #[tokio::test]
    async fn failed_storage_update_returns_http_error_and_preserves_account() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let (router, state) = persistent_router(&path);
        let original = state.account_service_state.accounts()[0].clone();
        let original_bytes = std::fs::read(&path).unwrap();
        std::fs::rename(&path, dir.path().join("saved.json")).unwrap();
        std::fs::create_dir(&path).unwrap(); // Rename over a directory fails even as root.
        let response = account_request(
            router,
            "PATCH",
            &original.username,
            &original.password,
            serde_json::json!({"Password":"new-password", "UserName":"new-user"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], br#""Failed to update BMC account""#);
        assert_eq!(state.account_service_state.accounts()[0], original);
        assert_eq!(
            std::fs::read(dir.path().join("saved.json")).unwrap(),
            original_bytes
        );
    }
}
