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

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bmc_platform::{AuthError, PlatformError};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use mac_address::MacAddress;
use nv_redfish::bmc_http::BmcCredentials;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Authentication mechanism used for runtime Redfish connections.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAuthMode {
    /// Read the stored BMC username/password directly.
    #[default]
    Basic,
    /// Ask the existing session manager to issue an `X-Auth-Token`.
    Session,
}

/// Secret-free metadata required for a runtime credential request.
///
/// Runtime credentials are always the per-BMC root credentials keyed by the
/// BMC MAC address, which [`crate::BmcRef`] has already validated.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialRequest {
    caller_identity: String,
    bmc_mac_address: MacAddress,
    bmc_address: SocketAddr,
    auth_mode: RuntimeAuthMode,
}

impl CredentialRequest {
    /// Creates request metadata for one caller and BMC endpoint.
    ///
    /// The caller identity was validated once by the connection manager.
    pub(crate) const fn new(
        caller_identity: String,
        bmc_mac_address: MacAddress,
        bmc_address: SocketAddr,
        auth_mode: RuntimeAuthMode,
    ) -> Self {
        Self {
            caller_identity,
            bmc_mac_address,
            bmc_address,
            auth_mode,
        }
    }

    /// Returns the authenticated service or controller requesting credentials.
    pub fn caller_identity(&self) -> &str {
        &self.caller_identity
    }

    /// Returns the BMC MAC address used for secret lookup.
    pub const fn bmc_mac_address(&self) -> MacAddress {
        self.bmc_mac_address
    }

    /// Returns the concrete BMC socket address used by the client pool.
    pub const fn bmc_address(&self) -> SocketAddr {
        self.bmc_address
    }

    /// Returns the requested authentication mechanism.
    pub const fn auth_mode(&self) -> RuntimeAuthMode {
        self.auth_mode
    }
}

/// Invalid caller identity supplied to the connection manager.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CredentialRequestError {
    /// The caller identity is empty or whitespace-only.
    #[error("credential request caller identity must not be empty")]
    EmptyCallerIdentity,
}

/// A short-lived, non-serializable lease containing credentials for one BMC.
///
/// The lease deliberately exposes no secret-bearing `Debug`, `Display`, or
/// serde representation.
pub struct CredentialLease {
    credentials: BmcCredentials,
}

impl CredentialLease {
    /// Creates a transient credential lease.
    pub const fn new(credentials: BmcCredentials) -> Self {
        Self { credentials }
    }

    /// Hands over the credentials for constructing or refreshing a transport.
    pub fn into_credentials(self) -> BmcCredentials {
        self.credentials
    }
}

impl fmt::Debug for CredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialLease")
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

/// Issues transient credentials for runtime BMC connections.
///
/// The same call serves first connection and refresh; the connection manager
/// is what evicts pooled roots before asking again.
#[async_trait]
pub trait RuntimeCredentialProvider: Send + Sync {
    async fn issue(&self, request: &CredentialRequest) -> Result<CredentialLease, PlatformError>;
}

/// Issues no Redfish credentials: nico-bmc-proxy authenticates upstream and
/// strips any it receives, so Redfish connections never send the password.
pub(crate) struct ProxyAuthenticated;

#[async_trait]
impl RuntimeCredentialProvider for ProxyAuthenticated {
    async fn issue(&self, _request: &CredentialRequest) -> Result<CredentialLease, PlatformError> {
        Ok(CredentialLease::new(BmcCredentials::anonymous()))
    }
}

/// Issues the stored per-BMC root credentials for basic-auth connections.
pub struct StoredRootCredentials {
    credential_reader: Arc<dyn CredentialReader>,
}

impl StoredRootCredentials {
    pub fn new(credential_reader: Arc<dyn CredentialReader>) -> Self {
        Self { credential_reader }
    }
}

#[async_trait]
impl RuntimeCredentialProvider for StoredRootCredentials {
    async fn issue(&self, request: &CredentialRequest) -> Result<CredentialLease, PlatformError> {
        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: request.bmc_mac_address(),
            },
        };
        match self.credential_reader.get_credentials(&key).await {
            Ok(Some(Credentials::UsernamePassword { username, password }))
                if !password.is_empty() =>
            {
                Ok(CredentialLease::new(BmcCredentials::new(
                    username, password,
                )))
            }
            Ok(_) => Err(PlatformError::Auth(AuthError::CredentialsUnavailable)),
            Err(error) => {
                tracing::warn!(
                    bmc = %request.bmc_address(),
                    caller = request.caller_identity(),
                    error = %error,
                    "BMC root credentials could not be read"
                );
                Err(PlatformError::Auth(AuthError::CredentialsUnavailable))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RecordingStore;

    #[test]
    fn credential_lease_debug_is_fully_redacted() {
        let lease = CredentialLease::new(BmcCredentials::new(
            "operator".to_string(),
            "secret-value".to_string(),
        ));

        let debug = format!("{lease:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("operator"));
        assert!(!debug.contains("secret-value"));
    }

    #[tokio::test]
    async fn stored_root_credentials_require_a_non_empty_password() {
        let request = CredentialRequest::new(
            "machine-controller".to_string(),
            MacAddress::new([2, 0, 0, 0, 0, 1]),
            "192.0.2.10:443".parse().expect("valid socket address"),
            RuntimeAuthMode::Basic,
        );
        let unavailable = Err(PlatformError::Auth(AuthError::CredentialsUnavailable));
        let cases = [
            (
                "stored",
                Ok(Some(Credentials::new("root", "secret"))),
                Ok(()),
            ),
            ("missing", Ok(None), unavailable.clone()),
            (
                "empty password",
                Ok(Some(Credentials::new("root", ""))),
                unavailable.clone(),
            ),
            ("unreadable", Err(()), unavailable),
        ];
        for (name, stored, expected) in cases {
            let issued = StoredRootCredentials::new(Arc::new(RecordingStore::new(stored)))
                .issue(&request)
                .await
                .map(drop);
            assert_eq!(issued, expected, "{name}");
        }
    }

    #[test]
    fn credential_request_contains_explicit_secret_free_metadata() {
        let request = CredentialRequest::new(
            "machine-controller".to_string(),
            MacAddress::new([2, 0, 0, 0, 0, 1]),
            "192.0.2.10:443".parse().expect("valid socket address"),
            RuntimeAuthMode::Basic,
        );

        let encoded = serde_json::to_string(&request).expect("request serializes");
        assert!(encoded.contains("machine-controller"));
        assert!(encoded.contains("192.0.2.10:443"));
        assert!(encoded.contains("02:00:00:00:00:01"));
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("token"));
    }
}
