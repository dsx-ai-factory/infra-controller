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

//! Taking over a BMC in factory state, as site exploration does: replace
//! the factory password through the accounts driver the ServiceRoot alone
//! selects, since a factory BMC serves nothing else.

use std::time::{Duration, Instant};

use bmc_platform::{AuthError, Capability, PlatformError, PlatformIdentity};

use crate::connect::Target;
use crate::selection::Selection;

/// How long a password may take to be accepted: a new one takes effect a
/// while after it is set, and some BMCs, as HPE iLO does, refuse every login
/// for a few seconds after a failed one.
const PASSWORD_TIMEOUT: Duration = Duration::from_secs(60);
const PASSWORD_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How the factory password was dealt with.
pub(crate) struct Ownership {
    pub(crate) summary: String,
    /// The accounts driver the ServiceRoot alone selected, when the
    /// password was changed.
    pub(crate) accounts_driver: Option<String>,
}

/// Replaces `factory`'s password with `new_password` unless the BMC already
/// accepts the new one, and returns the BMC reached with the new password.
pub(crate) async fn take(
    factory: &Target,
    new_password: &str,
    overrides: &[String],
) -> Result<(Target, Ownership), String> {
    let owned = factory.as_account(factory.username(), new_password);
    match factory.log_in().await {
        Ok(()) | Err(PlatformError::Auth(AuthError::PasswordChangeRequired { .. })) => {}
        Err(PlatformError::Auth(AuthError::InvalidCredentials)) => {
            logged_in_within(async || owned.log_in().await)
                .await
                .map_err(|error| {
                    format!(
                        "the BMC refuses both the factory and the new password, the new one for \
                         {}s: {error}",
                        PASSWORD_TIMEOUT.as_secs()
                    )
                })?;
            let ownership = Ownership {
                summary: "the factory password was already replaced".to_string(),
                accounts_driver: None,
            };
            return Ok((owned, ownership));
        }
        Err(error) => return Err(format!("logging in with the factory password: {error}")),
    }

    let identity = PlatformIdentity {
        service_root: factory.service_root_identity().await?,
        ..PlatformIdentity::default()
    };
    let selection = Selection::resolve(&identity, overrides)?;
    if !selection.is_supported(Capability::Accounts) {
        return Err(format!(
            "no accounts driver for the factory ServiceRoot: {}",
            selection.reason(Capability::Accounts)
        ));
    }
    let bmc = factory
        .connect(selection.endpoint(factory.address(), identity)?)
        .await?;
    let accounts = bmc.accounts().map_err(|error| error.to_string())?;
    let started = Instant::now();
    accounts
        .change_password(factory.username(), new_password)
        .await
        .map_err(|error| format!("change_password: {error}"))?;

    logged_in_within(async || owned.log_in().await)
        .await
        .map_err(|error| {
            format!(
                "the new password is still refused after {}s: {error}",
                PASSWORD_TIMEOUT.as_secs()
            )
        })?;
    match factory.log_in().await {
        Err(PlatformError::Auth(AuthError::InvalidCredentials)) => {}
        Ok(()) => return Err("the new password works, but so does the factory one".to_string()),
        Err(error) => {
            return Err(format!(
                "the new password works, but the factory one is not refused: {error}"
            ));
        }
    }
    let driver = selection.driver(Capability::Accounts);
    let ownership = Ownership {
        summary: format!(
            "replaced the factory password of {} in {:.1}s; the factory password is refused",
            factory.username(),
            started.elapsed().as_secs_f64()
        ),
        accounts_driver: Some(driver),
    };
    Ok((owned, ownership))
}

/// Runs `log_in` until it succeeds, every [`PASSWORD_POLL_INTERVAL`] for up to
/// [`PASSWORD_TIMEOUT`], returning the last refusal when it never does.
async fn logged_in_within(
    log_in: impl AsyncFn() -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    let deadline = tokio::time::Instant::now() + PASSWORD_TIMEOUT;
    loop {
        match log_in().await {
            Ok(()) => return Ok(()),
            Err(error) if tokio::time::Instant::now() + PASSWORD_POLL_INTERVAL > deadline => {
                return Err(error);
            }
            Err(_) => tokio::time::sleep(PASSWORD_POLL_INTERVAL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A refused login is tried again until the BMC accepts it, as HPE iLO
    /// does a few seconds after a failed one, and given up after
    /// `PASSWORD_TIMEOUT`.
    #[tokio::test(start_paused = true)]
    async fn a_refused_login_is_tried_again_for_a_while() {
        let refused = || Err(PlatformError::Auth(AuthError::InvalidCredentials));
        for (scenario, refusals, logged_in, attempts) in [
            ("accepted at once", 0, true, 1),
            ("accepted after two refusals", 2, true, 3),
            ("never accepted", usize::MAX, false, 13),
        ] {
            let made = Cell::new(0);
            let result = logged_in_within(async || {
                made.set(made.get() + 1);
                if made.get() > refusals {
                    Ok(())
                } else {
                    refused()
                }
            })
            .await;
            assert_eq!(result.is_ok(), logged_in, "{scenario}: {result:?}");
            assert_eq!(made.get(), attempts, "{scenario}: attempts");
        }
    }
}
