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

//! An account's lifecycle: created, logged in with, deleted, and refused.

use bmc_platform::{DriverOutcome, PlatformError};
use carbide_secrets::credentials::Credentials;
use nv_redfish::account::ManagerAccountCreate;

use crate::checks::context::CheckCx;
use crate::checks::errors::failed;
use crate::checks::model::{Outcome, Step};
use crate::checks::ops::{Attempt, IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL, how, poll};
use crate::connect::Target;

/// Creates a temporary account, logs in with it, deletes it, and confirms
/// the login stops working; the run's own account is never changed.
pub(super) async fn account_lifecycle(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let accounts = bmc.accounts()?;
    let username = format!("nicoval{:04}", rand::random::<u16>() % 10_000);
    let password = Credentials::generate_password();
    cx.secret(&password);
    let temporary = cx.target().as_account(&username, &password);

    if listed(cx, &username).await? {
        return Err(Outcome::Skipped(format!(
            "an account named {username} already exists"
        )));
    }
    let created = accounts
        .create(ManagerAccountCreate::builder(
            password.clone(),
            username.clone(),
            "Administrator".to_string(),
        ))
        .await;
    match &created {
        Ok(outcome) => cx.trace(format!("create {username}: {}", how(outcome))),
        Err(error) => cx.trace(format!("create {username}: {error}")),
    }
    let verified = match created {
        Ok(outcome) => verify_new_account(cx, &outcome, &username, &temporary).await,
        Err(error) => Err(failed("create")(error)),
    };
    if !listed(cx, &username).await.unwrap_or(true) {
        return verified;
    }

    let deleted = accounts.delete(&username).await.map_err(failed("delete"));
    if let Ok(outcome) = &deleted {
        cx.trace(format!("delete {username}: {}", how(outcome)));
    }
    let gone = match deleted {
        Ok(outcome) => verify_account_gone(cx, &outcome, &username, &temporary).await,
        Err(outcome) => Err(outcome),
    };
    if gone.is_err() {
        cx.not_restored(format!("temporary account {username} may still exist"));
    }
    Ok(format!("{}; {}", verified?, gone?))
}

async fn verify_new_account(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
    username: &str,
    temporary: &Target,
) -> Step {
    cx.wait_for_operations(outcome).await?;
    if !listed(cx, username).await? {
        return Err(Outcome::Fail(format!(
            "created {username}, but it is not listed"
        )));
    }
    poll(
        cx,
        IMMEDIATE_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match temporary.log_in().await {
            Ok(()) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(format!("logging in as {username} fails: {error}")),
        },
    )
    .await?;
    cx.trace(format!("logged in as {username}"));
    Ok(format!("created {username} and logged in with it"))
}

async fn verify_account_gone(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
    username: &str,
    temporary: &Target,
) -> Step {
    cx.wait_for_operations(outcome).await?;
    if listed(cx, username).await? {
        return Err(Outcome::Fail(format!(
            "deleted {username}, but it is still listed"
        )));
    }
    poll(
        cx,
        IMMEDIATE_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match temporary.log_in().await {
            Err(PlatformError::Auth(_)) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(format!(
                "logging in as {username} fails with {error}, not an auth error"
            )),
            Ok(()) => Attempt::NotYet(format!("{username} can still log in")),
        },
    )
    .await?;
    Ok("deleted it, and its login is refused".to_string())
}

async fn listed(cx: &CheckCx<'_>, username: &str) -> Result<bool, Outcome> {
    let accounts = cx.bmc().accounts()?.list().await.map_err(failed("list"))?;
    Ok(accounts
        .iter()
        .any(|account| account.user_name.as_deref() == Some(username)))
}
