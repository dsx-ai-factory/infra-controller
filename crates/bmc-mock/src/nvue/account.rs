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

//! The NVOS users the NVUE REST API authenticates.
//!
//! NVOS keeps Linux users; NICo manages one administrator whose password it
//! rotates through RMS. The mock holds that one account, with the password a
//! rotation last set.

use std::sync::Mutex;

use super::UnknownUser;

/// An NVOS user and its password.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Account {
    username: String,
    password: String,
}

impl Account {
    pub(crate) fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The accounts of one NVOS. The first is the administrator.
#[derive(Debug)]
pub(super) struct Accounts {
    accounts: Mutex<Vec<Account>>,
}

impl Accounts {
    pub(super) fn new(admin: Account) -> Self {
        Self {
            accounts: Mutex::new(vec![admin]),
        }
    }

    pub(super) fn is_authorized(&self, username: &str, password: &str) -> bool {
        super::lock(&self.accounts)
            .iter()
            .any(|account| account.username == username && account.password == password)
    }

    pub(super) fn admin_credentials(&self) -> (String, String) {
        let accounts = super::lock(&self.accounts);
        let admin = accounts.first().expect("an NVOS has its administrator");
        (admin.username.clone(), admin.password.clone())
    }

    pub(super) fn reset_to_defaults(&self, admin: &Account) {
        *super::lock(&self.accounts) = vec![admin.clone()];
    }

    pub(super) fn set_password(
        &self,
        username: &str,
        new_password: &str,
    ) -> Result<(), UnknownUser> {
        let mut accounts = super::lock(&self.accounts);
        let Some(account) = accounts
            .iter_mut()
            .find(|account| account.username == username)
        else {
            return Err(UnknownUser(username.to_string()));
        };
        account.password = new_password.to_string();
        Ok(())
    }
}
