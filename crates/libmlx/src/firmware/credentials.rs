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

use rpc::protos::mlx_device::firmware_credentials::CredentialType as CredentialTypePb;
use rpc::protos::mlx_device::{
    BasicAuthCredentials as BasicAuthCredentialsPb,
    BearerTokenCredentials as BearerTokenCredentialsPb,
    FirmwareCredentials as FirmwareCredentialsPb, HeaderCredentials as HeaderCredentialsPb,
    SshAgentCredentials as SshAgentCredentialsPb, SshKeyCredentials as SshKeyCredentialsPb,
};
use serde::{Deserialize, Serialize};

use crate::firmware::error::{FirmwareError, FirmwareResult};

// Credentials represents authentication for firmware downloads and
// transfers. A single type is used for both HTTP and SSH sources;
// validation that the credential type matches the source type
// happens at resolve time.
//
// When used in TOML configuration, the "type" field determines
// which variant is deserialized:
//
//   [firmware_credentials]
//   type = "bearer_token"
//   token = "asjdhkasdlkj..."
//
//   [firmware_credentials]
//   type = "basic_auth"
//   username = "deploy"
//   password = "s3cret"
//
//   [firmware_credentials]
//   type = "ssh_agent"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum Credentials {
    // BearerToken uses an Authorization: Bearer <token> header.
    BearerToken {
        token: String,
    },
    // BasicAuth uses HTTP Basic authentication.
    BasicAuth {
        username: String,
        password: String,
    },
    // Header uses a custom header name and value for authentication.
    Header {
        name: String,
        value: String,
    },
    // SshKey uses a private key file for SSH authentication, with
    // an optional passphrase.
    SshKey {
        path: String,
        #[serde(default)]
        passphrase: Option<String>,
    },
    // SshAgent uses the running SSH agent for authentication. The
    // agent is reached via the SSH_AUTH_SOCK environment variable.
    SshAgent {},
}

impl Credentials {
    // bearer_token creates a BearerToken credential.
    pub fn bearer_token(token: impl Into<String>) -> Self {
        Self::BearerToken {
            token: token.into(),
        }
    }

    // basic_auth creates a BasicAuth credential.
    pub fn basic_auth(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::BasicAuth {
            username: username.into(),
            password: password.into(),
        }
    }

    // header creates a custom Header credential.
    pub fn header(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Header {
            name: name.into(),
            value: value.into(),
        }
    }

    // ssh_key creates an SshKey credential from a private key path.
    pub fn ssh_key(path: impl Into<String>) -> Self {
        Self::SshKey {
            path: path.into(),
            passphrase: None,
        }
    }

    // ssh_key_with_passphrase creates an SshKey credential from a
    // private key path and passphrase.
    pub fn ssh_key_with_passphrase(path: impl Into<String>, passphrase: impl Into<String>) -> Self {
        Self::SshKey {
            path: path.into(),
            passphrase: Some(passphrase.into()),
        }
    }

    // ssh_agent creates an SshAgent credential.
    pub fn ssh_agent() -> Self {
        Self::SshAgent {}
    }

    // type_name returns human-readable details for the credential
    // variant without exposing any underlying secrets. I wanted to
    // be able to just dump it out but realized that would be unsafe,
    // since it would also mean dumping out tokens and passwords.
    pub fn type_name(&self) -> &'static str {
        match self {
            Credentials::BearerToken { .. } => "bearer_token",
            Credentials::BasicAuth { .. } => "basic_auth",
            Credentials::Header { .. } => "header",
            Credentials::SshKey { .. } => "ssh_key",
            Credentials::SshAgent {} => "ssh_agent",
        }
    }

    // validate_http returns an error if this credential type is not
    // compatible with HTTP sources.
    pub fn validate_http(&self) -> FirmwareResult<()> {
        match self {
            Credentials::BearerToken { .. }
            | Credentials::BasicAuth { .. }
            | Credentials::Header { .. } => Ok(()),
            Credentials::SshKey { .. } | Credentials::SshAgent {} => {
                Err(FirmwareError::ConfigError(
                    "SSH credentials cannot be used with HTTP sources".to_string(),
                ))
            }
        }
    }

    // validate_ssh returns an error if this credential type is not
    // compatible with SSH sources.
    pub fn validate_ssh(&self) -> FirmwareResult<()> {
        match self {
            Credentials::SshKey { .. } | Credentials::SshAgent {} => Ok(()),
            Credentials::BearerToken { .. }
            | Credentials::BasicAuth { .. }
            | Credentials::Header { .. } => Err(FirmwareError::ConfigError(
                "HTTP credentials cannot be used with SSH sources".to_string(),
            )),
        }
    }
}

// From implementations for converting Credentials to/from
// a FirmwareCredentialsPb protobuf message and back.
impl From<Credentials> for FirmwareCredentialsPb {
    fn from(cred: Credentials) -> Self {
        let credential_type = match cred {
            Credentials::BearerToken { token } => {
                CredentialTypePb::BearerToken(BearerTokenCredentialsPb { token })
            }
            Credentials::BasicAuth { username, password } => {
                CredentialTypePb::BasicAuth(BasicAuthCredentialsPb { username, password })
            }
            Credentials::Header { name, value } => {
                CredentialTypePb::Header(HeaderCredentialsPb { name, value })
            }
            Credentials::SshKey { path, passphrase } => {
                CredentialTypePb::SshKey(SshKeyCredentialsPb { path, passphrase })
            }
            Credentials::SshAgent {} => CredentialTypePb::SshAgent(SshAgentCredentialsPb {}),
        };
        FirmwareCredentialsPb {
            credential_type: Some(credential_type),
        }
    }
}

impl TryFrom<FirmwareCredentialsPb> for Credentials {
    type Error = String;

    fn try_from(proto: FirmwareCredentialsPb) -> Result<Self, Self::Error> {
        match proto
            .credential_type
            .ok_or("Missing credential_type in FirmwareCredentials")?
        {
            CredentialTypePb::BearerToken(bt) => Ok(Credentials::BearerToken { token: bt.token }),
            CredentialTypePb::BasicAuth(ba) => Ok(Credentials::BasicAuth {
                username: ba.username,
                password: ba.password,
            }),
            CredentialTypePb::Header(h) => Ok(Credentials::Header {
                name: h.name,
                value: h.value,
            }),
            CredentialTypePb::SshKey(sk) => Ok(Credentials::SshKey {
                path: sk.path,
                passphrase: sk.passphrase,
            }),
            CredentialTypePb::SshAgent(_) => Ok(Credentials::SshAgent {}),
        }
    }
}

impl TryFrom<Credentials> for forge_ssh::ssh_client::AuthConfig {
    type Error = FirmwareError;
    fn try_from(value: Credentials) -> Result<Self, Self::Error> {
        use forge_ssh::ssh_client::AuthConfig;
        match value {
            Credentials::SshKey { path, passphrase } => Ok(AuthConfig::SshKey { path, passphrase }),
            Credentials::SshAgent {} => Ok(AuthConfig::SshAgent),
            _ => Err(FirmwareError::ConfigError(
                "HTTP credentials cannot be used with SSH sources".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn test_credentials_bearer_token_roundtrip() {
        let original = Credentials::bearer_token("my-token");
        let proto: FirmwareCredentialsPb = original.clone().into();
        let converted: Credentials = proto.try_into().unwrap();
        match converted {
            Credentials::BearerToken { token } => assert_eq!(token, "my-token"),
            _ => panic!("Expected BearerToken"),
        }
    }

    #[test]
    fn test_credentials_basic_auth_roundtrip() {
        let original = Credentials::basic_auth("user", "pass");
        let proto: FirmwareCredentialsPb = original.clone().into();
        let converted: Credentials = proto.try_into().unwrap();
        match converted {
            Credentials::BasicAuth { username, password } => {
                assert_eq!(username, "user");
                assert_eq!(password, "pass");
            }
            _ => panic!("Expected BasicAuth"),
        }
    }

    #[test]
    fn test_credentials_header_roundtrip() {
        let original = Credentials::header("X-API-Key", "secret");
        let proto: FirmwareCredentialsPb = original.clone().into();
        let converted: Credentials = proto.try_into().unwrap();
        match converted {
            Credentials::Header { name, value } => {
                assert_eq!(name, "X-API-Key");
                assert_eq!(value, "secret");
            }
            _ => panic!("Expected Header"),
        }
    }

    #[test]
    fn test_credentials_ssh_key_roundtrip() {
        let original = Credentials::ssh_key_with_passphrase("/path/to/key", "passphrase");
        let proto: FirmwareCredentialsPb = original.clone().into();
        let converted: Credentials = proto.try_into().unwrap();
        match converted {
            Credentials::SshKey { path, passphrase } => {
                assert_eq!(path, "/path/to/key");
                assert_eq!(passphrase, Some("passphrase".to_string()));
            }
            _ => panic!("Expected SshKey"),
        }
    }

    #[test]
    fn test_credentials_ssh_agent_roundtrip() {
        let original = Credentials::ssh_agent();
        let proto: FirmwareCredentialsPb = original.clone().into();
        let converted: Credentials = proto.try_into().unwrap();
        assert!(matches!(converted, Credentials::SshAgent {}));
    }

    #[test]
    fn ssh_agent_serde_rejects_unknown_fields() {
        scenarios!(
            run = |input| serde_json::from_str::<Credentials>(input)
                .map(|credentials| matches!(credentials, Credentials::SshAgent {}))
                .map_err(drop);

            "valid empty struct variant" {
                r#"{"type":"ssh_agent"}"# => Yields(true),
            }

            "unknown fields are rejected" {
                r#"{"type":"ssh_agent","unexpected":true}"# => Fails,
            }
        );
    }

    #[test]
    fn ssh_agent_json_round_trip_preserves_legacy_shape() {
        let encoded = serde_json::to_value(Credentials::ssh_agent())
            .expect("SSH-agent credential serializes");
        assert_eq!(encoded, serde_json::json!({ "type": "ssh_agent" }));

        let decoded: Credentials =
            serde_json::from_value(encoded).expect("legacy SSH-agent JSON deserializes");
        assert!(matches!(decoded, Credentials::SshAgent {}));
    }
}
