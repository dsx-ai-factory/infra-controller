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

//! BMC account operations that authenticate with explicit credentials rather
//! than the stored ones: rotation, factory-reset restore, and site-explorer's
//! credential setup and user management all go through these.

use std::net::SocketAddr;

use bmc_backend::{BackendAccountBmc, BmcConnector};
use bmc_platform::{AuthError, PlatformError};
use carbide_secrets::credentials::Credentials;
use carbide_utils::redfish::{BmcAccessInfo, parse_uri_host_ip};
use model::site_explorer::EndpointExplorationReport;

/// The BF4 DPU BMC account whose password [`change_bmc_service_password`] sets.
pub const BMC_SERVICE_USERNAME: &str = "service";

/// Connects to `access` with `credentials` for account operations. The
/// `Accounts` capability is selected by the service root alone, so a BMC
/// exploration has not recorded one for is identified from its live service
/// root.
pub async fn connect_accounts(
    connector: &BmcConnector,
    access: &BmcAccessInfo,
    report: Option<&EndpointExplorationReport>,
    credentials: Credentials,
) -> Result<BackendAccountBmc, PlatformError> {
    let connected = match report.filter(|report| report.service_root.is_some()) {
        Some(report) => {
            connector
                .connect_with_credentials(access, report, credentials)
                .await
        }
        None => {
            connector
                .connect_unexplored_with_credentials(access, credentials)
                .await
        }
    };
    connected.map_err(|error| error.into_platform_error())
}

/// Whether `credentials` authenticate, judged by listing accounts.
///
/// A `PasswordChangeRequired` rejection proves the password was accepted. A
/// 403 does too when `allow_forbidden` is set (the low-privilege BF4 `service`
/// account cannot list accounts); otherwise it means the credentials are not
/// valid for the account operations that follow.
pub async fn probe_bmc_credentials(
    connector: &BmcConnector,
    access: &BmcAccessInfo,
    report: Option<&EndpointExplorationReport>,
    credentials: Credentials,
    allow_forbidden: bool,
) -> Result<bool, PlatformError> {
    let listed = async {
        connect_accounts(connector, access, report, credentials)
            .await?
            .accounts()?
            .list()
            .await
    };
    match listed.await {
        Ok(_) => Ok(true),
        Err(error) => classify_probe_error(error, allow_forbidden),
    }
}

fn classify_probe_error(
    error: PlatformError,
    allow_forbidden: bool,
) -> Result<bool, PlatformError> {
    match error {
        PlatformError::Auth(AuthError::PasswordChangeRequired { .. }) => Ok(true),
        PlatformError::Auth(AuthError::InsufficientPrivilege) if allow_forbidden => Ok(true),
        PlatformError::Auth(AuthError::InvalidCredentials | AuthError::InsufficientPrivilege) => {
            Ok(false)
        }
        other => Err(other),
    }
}

/// Changes the root account's password from `current_credentials` to
/// `new_password`. Callers then apply the default account policy as the new
/// password ([`apply_bmc_root_policy`]); a caller with a stored credential for
/// the BMC stores the new password first, so a failed policy step cannot leave
/// the store behind the BMC.
pub async fn change_bmc_root_password(
    connector: &BmcConnector,
    access: &BmcAccessInfo,
    report: Option<&EndpointExplorationReport>,
    current_credentials: Credentials,
    new_password: &str,
) -> Result<(), PlatformError> {
    let Credentials::UsernamePassword { username, .. } = &current_credentials;
    let username = username.clone();
    connect_accounts(connector, access, report, current_credentials)
        .await?
        .accounts()?
        .change_password(&username, new_password)
        .await?;
    invalidate_bmc(connector, access);
    Ok(())
}

/// Applies the platform's default account policy (including Lenovo IPMI
/// access) as the root account in `credentials`.
pub async fn apply_bmc_root_policy(
    connector: &BmcConnector,
    access: &BmcAccessInfo,
    report: Option<&EndpointExplorationReport>,
    credentials: Credentials,
) -> Result<(), PlatformError> {
    let Credentials::UsernamePassword { username, password } = &credentials;
    connect_accounts(connector, access, report, credentials.clone())
        .await?
        .accounts()?
        .apply_default_policy(username, password)
        .await?;
    Ok(())
}

/// Changes the BF4 DPU BMC `service` account's password, authenticating as
/// root with `root_credentials`.
pub async fn change_bmc_service_password(
    connector: &BmcConnector,
    access: &BmcAccessInfo,
    report: Option<&EndpointExplorationReport>,
    root_credentials: Credentials,
    new_password: &str,
) -> Result<(), PlatformError> {
    connect_accounts(connector, access, report, root_credentials)
        .await?
        .accounts()?
        .change_password(BMC_SERVICE_USERNAME, new_password)
        .await?;
    invalidate_bmc(connector, access);
    Ok(())
}

/// Evicts connections cached for `access` under its previous password after a
/// password change.
fn invalidate_bmc(connector: &BmcConnector, access: &BmcAccessInfo) {
    if let Some(ip) = parse_uri_host_ip(&access.host) {
        connector.invalidate_bmc(SocketAddr::new(ip, access.port.unwrap_or(443)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_probe_classification_distinguishes_root_and_service_roles() {
        let password_change_required =
            || PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri: None });
        let invalid = || PlatformError::Auth(AuthError::InvalidCredentials);
        let forbidden = || PlatformError::Auth(AuthError::InsufficientPrivilege);

        for allow_forbidden in [false, true] {
            assert_eq!(
                classify_probe_error(password_change_required(), allow_forbidden),
                Ok(true)
            );
            assert_eq!(classify_probe_error(invalid(), allow_forbidden), Ok(false));
            assert!(classify_probe_error(PlatformError::Unreachable, allow_forbidden).is_err());
        }
        assert_eq!(classify_probe_error(forbidden(), false), Ok(false));
        assert_eq!(classify_probe_error(forbidden(), true), Ok(true));
    }
}
