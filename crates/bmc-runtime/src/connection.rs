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

use std::sync::Arc;

use bmc_platform::{AuthError, IpmiOps, PlatformError};
use carbide_redfish::nv_redfish::{NvRedfishClientPool, NvRedfishConnection, RedfishBmc};
use thiserror::Error;

use crate::observe::observe;
use crate::{
    BmcRef, ConnectError, ConnectedBmc, CredentialRequest, CredentialRequestError, RuntimeAuthMode,
    RuntimeCredentialProvider,
};

/// Constructs and refreshes concrete pooled Redfish connections on behalf of
/// one named caller.
pub struct ConnectionManager {
    pool: Arc<NvRedfishClientPool>,
    credentials: Arc<dyn RuntimeCredentialProvider>,
    caller_identity: String,
    auth_mode: RuntimeAuthMode,
}

impl ConnectionManager {
    /// Creates a manager; `caller_identity` names the service requesting credentials.
    pub fn new(
        pool: Arc<NvRedfishClientPool>,
        credentials: Arc<dyn RuntimeCredentialProvider>,
        caller_identity: String,
    ) -> Result<Self, CredentialRequestError> {
        if caller_identity.trim().is_empty() {
            return Err(CredentialRequestError::EmptyCallerIdentity);
        }
        Ok(Self {
            pool,
            credentials,
            caller_identity,
            auth_mode: RuntimeAuthMode::Basic,
        })
    }

    /// Selects the runtime authentication mechanism. Basic auth is the default.
    pub const fn with_auth_mode(mut self, auth_mode: RuntimeAuthMode) -> Self {
        self.auth_mode = auth_mode;
        self
    }

    /// Acquires credentials and builds a pooled connection.
    pub async fn connect(
        &self,
        endpoint: BmcRef,
        ipmi: Option<Arc<dyn IpmiOps>>,
    ) -> Result<ConnectedBmc<RedfishBmc>, ConnectError> {
        let connection = self.authenticated_connection(&endpoint).await?;
        let selection = endpoint.selection();
        tracing::debug!(
            bmc = %endpoint.address(),
            drivers = %selection.drivers,
            quirks = ?selection.quirks,
            rules_hash = %selection.hash,
            "BMC connected with its selected drivers"
        );
        Ok(ConnectedBmc::new(
            endpoint,
            connection.bmc,
            connection.service_root,
            ipmi,
        ))
    }

    /// Evicts every pooled root for the BMC, re-issues credentials, and rebuilds.
    pub async fn refresh(
        &self,
        connected: &mut ConnectedBmc<RedfishBmc>,
    ) -> Result<(), ConnectError> {
        self.pool
            .invalidate_service_roots_for_bmc(connected.endpoint().address());
        let connection = self.authenticated_connection(connected.endpoint()).await?;
        connected.replace_redfish(connection.bmc, connection.service_root);
        Ok(())
    }

    /// Runs an operation and, in session mode, retries it once after a 401 or
    /// 403 with a freshly issued token.
    ///
    /// In basic mode the provider can only return the same root password, so
    /// a retry would just repeat the failed login against the BMC's lockout
    /// counter; the error is returned directly. Non-authentication errors and
    /// failures from the second attempt are returned without another refresh.
    pub async fn with_auth_retry<T, F>(
        &self,
        connected: &mut ConnectedBmc<RedfishBmc>,
        operation: F,
    ) -> Result<T, AuthRetryError>
    where
        F: AsyncFn(&ConnectedBmc<RedfishBmc>) -> Result<T, PlatformError>,
    {
        match operation(connected).await {
            Ok(value) => Ok(value),
            Err(error) if is_auth_error(&error) && self.auth_mode == RuntimeAuthMode::Session => {
                self.refresh(connected)
                    .await
                    .map_err(AuthRetryError::Refresh)?;
                operation(connected)
                    .await
                    .map_err(AuthRetryError::Operation)
            }
            Err(error) => Err(AuthRetryError::Operation(error)),
        }
    }

    async fn authenticated_connection(
        &self,
        endpoint: &BmcRef,
    ) -> Result<NvRedfishConnection, ConnectError> {
        let request = CredentialRequest::new(
            self.caller_identity.clone(),
            endpoint.mac_address(),
            endpoint.address(),
            self.auth_mode,
        );
        let lease = self
            .credentials
            .issue(&request)
            .await
            .map_err(ConnectError::Credentials)?;
        let (address, credentials) = (endpoint.address(), lease.into_credentials());
        observe(endpoint, None, "create_client", async {
            self.pool
                .connection_with_bmc_credentials(address, credentials)
                .await
                .map_err(PlatformError::from_redfish)
        })
        .await
        .map_err(ConnectError::Transport)
    }
}

/// Failure from the one-shot authenticated operation API.
#[derive(Debug, Error)]
pub enum AuthRetryError {
    /// Credential refresh or connection reconstruction failed.
    #[error("BMC authentication refresh failed: {0}")]
    Refresh(ConnectError),
    /// The operation failed without a retry or after its only retry.
    #[error("BMC operation failed: {0}")]
    Operation(PlatformError),
}

const fn is_auth_error(error: &PlatformError) -> bool {
    matches!(
        error,
        PlatformError::Auth(AuthError::InvalidCredentials | AuthError::InsufficientPrivilege)
    )
}

#[cfg(test)]
mod tests {
    use bmc_platform::{AuthError, PlatformError};
    use carbide_test_support::value_scenarios;

    use super::is_auth_error;

    #[test]
    fn only_unauthorized_and_forbidden_trigger_one_shot_refresh() {
        value_scenarios!(run = |error: PlatformError| is_auth_error(&error);
            "refresh once" {
                PlatformError::Auth(AuthError::InvalidCredentials) => true,
                PlatformError::Auth(AuthError::InsufficientPrivilege) => true,
            }
            "return directly" {
                PlatformError::Auth(AuthError::PasswordChangeRequired { account_uri: None }) => false,
                PlatformError::Unsupported => false,
                PlatformError::Unreachable => false,
                PlatformError::LockedDown => false,
            }
        );
    }
}
