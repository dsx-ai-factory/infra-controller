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
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::{Arc, Mutex, Weak};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{Callbacks, http, redfish};

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
    password_updater: Mutex<Option<Weak<dyn PasswordUpdater>>>,
    update_lock: tokio::sync::Mutex<()>,
}

pub(crate) trait PasswordUpdater: Send + Sync {
    fn update_password<'a>(
        &'a self,
        username: &'a str,
        current_password: &'a str,
        new_password: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
}

impl AccountServiceState {
    pub(crate) fn new(factory_default_account: Account) -> Self {
        Self {
            accounts: Mutex::new(vec![factory_default_account]),
            password_updater: Mutex::new(None),
            update_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) fn persisted_accounts(&self) -> Vec<PersistedAccount> {
        self.accounts().into_iter().map(Into::into).collect()
    }

    pub(crate) fn restore_accounts(&self, accounts: &[PersistedAccount]) {
        *self.accounts.lock().expect("mutex poisoned") =
            accounts.iter().cloned().map(Into::into).collect();
    }

    pub(crate) fn set_password_updater(&self, updater: &Arc<dyn PasswordUpdater>) {
        *self.password_updater.lock().expect("mutex poisoned") = Some(Arc::downgrade(updater));
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
        let _update = self.update_lock.lock().await;
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

/// Full account state, including the baseline used to detect factory-default credentials.
/// Credentials are plaintext; callers must not log serialized snapshots.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedAccount {
    id: String,
    username: String,
    password: String,
    factory_default_password: String,
    role_id: String,
}

impl From<Account> for PersistedAccount {
    fn from(a: Account) -> Self {
        Self {
            id: a.id,
            username: a.username,
            password: a.password,
            factory_default_password: a.factory_default_password,
            role_id: a.role_id,
        }
    }
}

impl From<PersistedAccount> for Account {
    fn from(a: PersistedAccount) -> Self {
        Self {
            id: a.id,
            username: a.username,
            password: a.password,
            factory_default_password: a.factory_default_password,
            role_id: a.role_id,
        }
    }
}

impl PersistedAccount {
    pub(crate) fn validate_all(accounts: &[Self]) -> Result<(), &'static str> {
        if accounts.is_empty() {
            return Err("empty account list");
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for account in accounts {
            if account.id.is_empty() || account.username.is_empty() || account.role_id.is_empty() {
                return Err("empty account identity or role");
            }
            if !ids.insert(&account.id) || !names.insert(&account.username) {
                return Err("duplicate account ID or username");
            }
        }
        Ok(())
    }
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
