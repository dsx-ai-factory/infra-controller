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
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;

use carbide_uuid::instance::InstanceId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{FromRow, Row};

use crate::metadata::Metadata;

pub mod identity_config_policy;

pub mod identity_config;

pub use identity_config::{
    EncryptedSigningPrivateKey, EncryptedTokenDelegationAuthConfig, EncryptionKeyId,
    EncryptionKeyIdTag, EnvelopeCiphertext, InvalidIssuer, InvalidNonEmptyStr, Issuer, KeyId,
    NonEmptyStr, SigningKeyPublicV1, SigningPublicKeyPem, TenantIdentityCurrentSigningKeySlot,
    TenantIdentitySigningKeyIdTag, TenantSigningPrivateKeyCiphertextTag,
    TenantSigningPublicKeyPemTag, TokenDelegationEncryptedAuthConfigTag,
};
pub use identity_config_policy::{
    validate_token_endpoint_domain_allowlist_patterns, validate_trust_domain_allowlist_patterns,
};

#[derive(Clone, Debug, Default)]
pub struct TenantSearchFilter {
    pub tenant_organization_name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TenantKeysetSearchFilter {
    pub tenant_org_id: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum TenantError {
    #[error("publickey validation fail for instance {0}, key {1}")]
    PublickeyValidationFailed(InstanceId, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant {
    pub organization_id: TenantOrganizationId,
    pub routing_profile_type: Option<String>,
    pub metadata: Metadata,
    pub version: ConfigVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantKeysetIdentifier {
    pub organization_id: TenantOrganizationId,
    pub keyset_id: String,
}

#[allow(rustdoc::invalid_html_tags)]
/// Possible format:
/// 1. <algo> <key> <comment>
/// 2. <algo> <key>
/// 3. <key>
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    pub algo: Option<String>,
    pub key: String,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantPublicKey {
    pub public_key: PublicKey,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantKeysetContent {
    pub public_keys: Vec<TenantPublicKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantKeyset {
    pub keyset_identifier: TenantKeysetIdentifier,
    pub keyset_content: TenantKeysetContent,
    pub version: String,
}

impl Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let algo = if let Some(algo) = self.algo.as_ref() {
            format!("{algo} ")
        } else {
            "".to_string()
        };

        let comment = if let Some(comment) = self.comment.as_ref() {
            format!(" {comment}")
        } else {
            "".to_string()
        };

        write!(f, "{}{}{}", algo, self.key, comment)
    }
}

impl FromStr for PublicKey {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key_parts = s.split(' ').collect_vec();

        // If length is greater than 1, key contains algo and key at least.
        Ok(if key_parts.len() > 1 {
            PublicKey {
                algo: Some(key_parts[0].to_string()),
                key: key_parts[1].to_string(),
                comment: key_parts.get(2).map(|x| x.to_string()),
            }
        } else {
            PublicKey {
                algo: None,
                key: s.to_string(),
                comment: None,
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTenantKeyset {
    pub keyset_identifier: TenantKeysetIdentifier,
    pub keyset_content: TenantKeysetContent,
    pub version: String,
    pub if_version_match: Option<String>,
}

/// Identifies a forge tenant
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantOrganizationId(String);

impl std::fmt::Debug for TenantOrganizationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for TenantOrganizationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TenantOrganizationId {
    /// Returns a String representation of the Tenant Org
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A string is not a valid Tenant ID
#[derive(thiserror::Error, Debug)]
#[error("ID {0} is not a valid tenant organization ID")]
pub struct InvalidTenantOrg(String);

impl TryFrom<String> for TenantOrganizationId {
    type Error = InvalidTenantOrg;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if id.is_empty() {
            return Err(InvalidTenantOrg(id));
        }

        for &ch in id.as_bytes() {
            if !(ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-') {
                return Err(InvalidTenantOrg(id));
            }
        }

        Ok(Self(id))
    }
}

impl FromStr for TenantOrganizationId {
    type Err = InvalidTenantOrg;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_string())
    }
}

impl sqlx::Type<sqlx::Postgres> for TenantOrganizationId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for TenantOrganizationId {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for TenantOrganizationId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(s).map_err(|e| sqlx::Error::Decode(Box::new(e)).into())
    }
}

/// Database row for tenant_identity_config table.
/// Persisted identity config with signing keys and token delegation.
#[derive(Debug, sqlx::FromRow)]
pub struct TenantIdentityConfig {
    pub organization_id: TenantOrganizationId,
    pub issuer: Issuer,
    pub default_audience: String,
    pub allowed_audiences: Json<Vec<String>>,
    pub token_ttl_sec: i32,
    pub subject_prefix: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub encrypted_signing_key_1: Option<EncryptedSigningPrivateKey>,
    pub encrypted_signing_key_2: Option<EncryptedSigningPrivateKey>,
    pub signing_key_public_1: Option<Json<identity_config::SigningKeyPublicV1>>,
    pub signing_key_public_2: Option<Json<identity_config::SigningKeyPublicV1>>,
    pub current_signing_key_slot: identity_config::TenantIdentityCurrentSigningKeySlot,
    pub non_active_slot_expires_at: Option<DateTime<Utc>>,
    // Token delegation (optional)
    pub token_endpoint: Option<String>,
    pub auth_method: Option<TokenDelegationAuthMethod>,
    /// Token delegation auth method secrets, **encrypted at rest**: standard base64 of JSON envelope v1
    /// (`key_encryption::encrypt`) over JSON (e.g. client_id and client_secret). Loaded from DB as
    /// ciphertext only; plaintext for gRPC mapping lives on [`TenantIdentityConfigDecrypted::auth_method_config`].
    pub encrypted_auth_method_config: Option<EncryptedTokenDelegationAuthConfig>,
    pub subject_token_audience: Option<String>,
    pub token_delegation_created_at: Option<DateTime<Utc>>,
}

impl TenantIdentityConfig {
    /// Signing public JSON for [`Self::current_signing_key_slot`].
    pub fn current_signing_public(
        &self,
    ) -> Result<&identity_config::SigningKeyPublicV1, &'static str> {
        match self.current_signing_key_slot {
            identity_config::TenantIdentityCurrentSigningKeySlot::SigningKey1 => self
                .signing_key_public_1
                .as_ref()
                .map(|j| &j.0)
                .ok_or("missing signing_key_public_1 for current_signing_key_slot"),
            identity_config::TenantIdentityCurrentSigningKeySlot::SigningKey2 => self
                .signing_key_public_2
                .as_ref()
                .map(|j| &j.0)
                .ok_or("missing signing_key_public_2 for current_signing_key_slot"),
        }
    }

    /// Encrypted private PEM for [`Self::current_signing_key_slot`].
    pub fn current_encrypted_signing_key(
        &self,
    ) -> Result<&EncryptedSigningPrivateKey, &'static str> {
        match self.current_signing_key_slot {
            identity_config::TenantIdentityCurrentSigningKeySlot::SigningKey1 => self
                .encrypted_signing_key_1
                .as_ref()
                .ok_or("missing encrypted_signing_key_1 for current_signing_key_slot"),
            identity_config::TenantIdentityCurrentSigningKeySlot::SigningKey2 => self
                .encrypted_signing_key_2
                .as_ref()
                .ok_or("missing encrypted_signing_key_2 for current_signing_key_slot"),
        }
    }

    /// Value for `TenantIdentityConfig.rotate_key` on **Get/Set responses**: `true` while an
    /// active JWKS overlap window is in progress (both published public slots are present and
    /// `non_active_slot_expires_at` is still in the future). `false` otherwise, including
    /// single-key configs and post-overlap rows (after GC clears the inactive slot).
    #[must_use]
    pub fn response_rotate_key(&self) -> bool {
        let Some(expires) = self.non_active_slot_expires_at else {
            return false;
        };
        if expires <= Utc::now() {
            return false;
        }
        self.signing_key_public_1.is_some() && self.signing_key_public_2.is_some()
    }
}

/// [`TenantIdentityConfig`] row plus decrypted token-delegation JSON for handlers / `TryInto` RPC.
/// `row.encrypted_auth_method_config` stays ciphertext from the database; plaintext is only in
/// `auth_method_config`. Do not log.
#[derive(Debug)]
pub struct TenantIdentityConfigDecrypted {
    pub row: TenantIdentityConfig,
    /// UTF-8 JSON from `TokenDelegation::to_db_format` after `key_encryption::decrypt`.
    pub auth_method_config: Option<String>,
}

/// Key material for a new or rotated signing key (caller-generated pair + encrypted private PEM).
///
/// [`Self::key_id`] is the same JWKS `kid` as in the persisted [`SigningKeyPublicV1`] built from
/// [`Self::signing_key_public`]; it is not stored as a separate DB column. Kept for handler/logging
/// and tests alongside the PEM-backed document.
#[derive(Clone, Debug)]
pub struct SigningKeyMaterial {
    pub key_id: KeyId,
    pub encrypted_signing_key: EncryptedSigningPrivateKey,
    pub signing_key_public: SigningPublicKeyPem,
}

/// Settable fields for tenant identity config (SPIFFE JWT-SVID).
/// Used as input to set identity configuration.
#[derive(Debug, Clone)]
pub struct IdentityConfig {
    pub issuer: Issuer,
    pub default_audience: String,
    pub allowed_audiences: Vec<String>,
    pub token_ttl_sec: u32,
    pub subject_prefix: String,
    pub enabled: bool,
    pub rotate_key: bool,
    pub algorithm: identity_config::SigningAlgorithm,
    pub encryption_key_id: EncryptionKeyId,
    /// Seconds to keep the previous verification key in JWKS after `rotate_key` (required when rotating).
    pub signing_key_overlap_sec: Option<i32>,
}

/// Validation bounds for IdentityConfig. Passed from site config (machine_identity).
#[derive(Debug, Clone)]
pub struct IdentityConfigValidationBounds {
    pub token_ttl_min_sec: u32,
    pub token_ttl_max_sec: u32,
    pub algorithm: identity_config::SigningAlgorithm,
    pub encryption_key_id: EncryptionKeyId,
    /// Site policy: JWT issuer trust domain must match at least one entry. Empty = no extra check.
    pub trust_domain_allowlist: Vec<String>,
    /// Max allowed `signing_key_overlap_sec` (seconds) on rotate.
    pub signing_key_overlap_max_sec: u32,
}

#[derive(thiserror::Error, Debug)]
#[error("{0}")]
pub struct IdentityConfigValidationError(pub String);

/// Token delegation config for external IdP token exchange (RFC 8693).
/// Used as input to set token delegation.
#[derive(Debug, Clone)]
pub struct TokenDelegation {
    pub token_endpoint: String,
    pub subject_token_audience: String,
    pub auth_method_config: TokenDelegationAuthMethodConfig,
}

/// Auth method for token delegation. Matches proto oneof.
#[derive(Debug, Clone)]
pub enum TokenDelegationAuthMethodConfig {
    None,
    ClientSecretBasic {
        client_id: String,
        client_secret: String,
    },
}

/// Database enum for token_delegation_auth_method_t. Maps to auth_method column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "token_delegation_auth_method_t")]
#[sqlx(rename_all = "snake_case")]
pub enum TokenDelegationAuthMethod {
    None,
    ClientSecretBasic,
}

impl TokenDelegationAuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ClientSecretBasic => "client_secret_basic",
        }
    }
}

/// Computes SHA256 hash of client_secret for display (e.g. in get_token_delegation response).
pub fn compute_client_secret_hash(client_secret: &str) -> String {
    let h = sha2::Sha256::digest(client_secret.as_bytes());
    format!("sha256:{}", hex::encode(h))
}

/// Hex chars to show in get_token_delegation response (8 chars + ".." suffix).
const HASH_DISPLAY_HEX_LEN: usize = 8;

/// Truncates hash for display in get_token_delegation: algorithm-prefix:XXXXXXXX..
pub fn truncate_hash_for_display(full_hash: &str) -> String {
    full_hash
        .split_once(':')
        .map(|(prefix, rest)| {
            format!(
                "{}:{}..",
                prefix,
                rest.chars().take(HASH_DISPLAY_HEX_LEN).collect::<String>()
            )
        })
        .unwrap_or_else(|| full_hash.to_string())
}

#[derive(thiserror::Error, Debug)]
#[error("{0}")]
pub struct TokenDelegationValidationError(pub String);

/// Site policy for [`TokenDelegation`]: allowlist on `token_endpoint` URL host / domain name (same pattern language as trust-domain allowlist).
#[derive(Debug, Clone, Default)]
pub struct TokenDelegationValidationBounds {
    pub token_endpoint_domain_allowlist: Vec<String>,
}

impl TokenDelegation {
    /// Returns (auth_method, config_json) for DB storage.
    pub fn to_db_format(&self) -> (TokenDelegationAuthMethod, String) {
        match &self.auth_method_config {
            TokenDelegationAuthMethodConfig::None => {
                (TokenDelegationAuthMethod::None, "{}".to_string())
            }
            TokenDelegationAuthMethodConfig::ClientSecretBasic {
                client_id,
                client_secret,
            } => {
                let stored = ClientSecretBasic {
                    client_id: client_id.clone(),
                    client_secret: client_secret.clone(),
                };
                let config_json =
                    serde_json::to_string(&stored).unwrap_or_else(|_| "{}".to_string());
                (TokenDelegationAuthMethod::ClientSecretBasic, config_json)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ClientSecretBasic {
    pub client_id: String,
    pub client_secret: String,
}

pub struct TenantPublicKeyValidationRequest {
    pub instance_id: InstanceId,
    pub public_key: String,
}

impl TenantPublicKeyValidationRequest {
    pub fn validate_key(&self, keysets: Vec<TenantKeyset>) -> Result<(), TenantError> {
        // Validate with all available keysets
        for keyset in keysets {
            for key in keyset.keyset_content.public_keys {
                if key.public_key.key == self.public_key {
                    return Ok(());
                }
            }
        }

        Err(TenantError::PublickeyValidationFailed(
            self.instance_id,
            self.public_key.clone(),
        ))
    }
}

// simplified tenant keyset id struct with tenant_org_id and keyset_id both as string
// used in find_ids and find_by_ids
#[derive(Debug, Clone, FromRow)]
pub struct TenantKeysetId {
    pub organization_id: String,
    pub keyset_id: String,
}

impl<'r> sqlx::FromRow<'r, PgRow> for Tenant {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let organization_id: String = row.try_get("organization_id")?;
        let name: String = row.try_get("organization_name")?;
        let routing_profile_type: Option<String> = row.try_get("routing_profile_type")?;
        Ok(Self {
            routing_profile_type,
            organization_id: organization_id
                .try_into()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            metadata: Metadata {
                name,
                description: String::new(), // We're using metadata for consistency,
                labels: HashMap::new(), // but description and labels might never be used for Tenant
            },
            version: row.try_get("version")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for TenantKeyset {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let tenant_keyset_content: sqlx::types::Json<TenantKeysetContent> =
            row.try_get("content")?;

        let organization_id: String = row.try_get("organization_id")?;
        Ok(Self {
            version: row.try_get("version")?,
            keyset_content: tenant_keyset_content.0,
            keyset_identifier: TenantKeysetIdentifier {
                organization_id: organization_id
                    .try_into()
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                keyset_id: row.try_get("keyset_id")?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn test_truncate_hash_for_display() {
        assert_eq!(
            truncate_hash_for_display("sha256:abcd1234567890abcdef"),
            "sha256:abcd1234.."
        );
        assert_eq!(truncate_hash_for_display("sha512:xyz"), "sha512:xyz..");
        assert_eq!(truncate_hash_for_display("no-colon"), "no-colon");
    }

    // Parsing a tenant-org id, via both `TryFrom<String>` and `FromStr` (which
    // delegates to the same validation). Valid input yields the round-tripped
    // string; invalid input is rejected. `InvalidTenantOrg` is not PartialEq and
    // the original only checked `is_err()`, so failing rows assert only that it
    // errors. The closure exercises both entry points and confirms they agree.
    #[test]
    fn parse_tenant_org() {
        scenarios!(
            run = |s| {
                let via_try_from = TenantOrganizationId::try_from(s.to_string());
                let via_parse = s.parse::<TenantOrganizationId>();
                // Both entry points must agree on success/failure.
                assert_eq!(via_try_from.is_ok(), via_parse.is_ok(), "{s}");
                via_try_from
                    .map(|org| org.as_str().to_string())
                    .map_err(drop)
            };
            "alphabetic" {
                "TenantA" => Yields("TenantA".to_string()),
            }

            "with underscore" {
                "Tenant_B" => Yields("Tenant_B".to_string()),
            }

            "with dashes and underscores" {
                "Tenant-C-_And_D_" => Yields("Tenant-C-_And_D_".to_string()),
            }

            "empty" {
                "" => Fails,
            }

            "leading space" {
                " Tenant_B" => Fails,
            }

            "trailing space" {
                "Tenant_C " => Fails,
            }

            "internal space" {
                "Tenant D" => Fails,
            }

            "disallowed punctuation" {
                "Tenant!A" => Fails,
            }
        );
    }

    #[test]
    fn tenant_org_formatting() {
        let tenant = TenantOrganizationId::try_from("TenantA".to_string()).unwrap();
        assert_eq!(format!("{tenant}"), "TenantA");
        assert_eq!(format!("{tenant:?}"), "\"TenantA\"");
        assert_eq!(serde_json::to_string(&tenant).unwrap(), "\"TenantA\"");
    }

    #[test]
    fn public_key_formatting() {
        let pub_key = PublicKey {
            algo: Some("ssh-rsa".to_string()),
            key: "randomkey123".to_string(),
            comment: Some("test@myorg".to_string()),
        };

        assert_eq!("ssh-rsa randomkey123 test@myorg", pub_key.to_string());
    }

    #[test]
    fn public_key_formatting_no_comment() {
        let pub_key = PublicKey {
            algo: Some("ssh-rsa".to_string()),
            key: "randomkey123".to_string(),
            comment: None,
        };

        assert_eq!("ssh-rsa randomkey123", pub_key.to_string());
    }

    #[test]
    fn public_key_formatting_only_key() {
        let pub_key = PublicKey {
            algo: None,
            key: "randomkey123".to_string(),
            comment: None,
        };

        assert_eq!("randomkey123", pub_key.to_string());
    }

    #[test]
    fn token_delegation_to_db_format_none() {
        let config = TokenDelegation {
            token_endpoint: "https://auth.example.com/token".to_string(),
            subject_token_audience: "https://api.example.com".to_string(),
            auth_method_config: TokenDelegationAuthMethodConfig::None,
        };
        let (auth_method, config_json) = config.to_db_format();
        assert_eq!(auth_method, TokenDelegationAuthMethod::None);
        assert_eq!(config_json, "{}");
    }

    // Parsing a JWT signing algorithm: only "ES256" is supported. The error
    // (`UnsupportedTenantSigningAlgorithm`) was only checked with `is_err()`, so
    // the failing row asserts only that it errors.
    #[test]
    fn tenant_identity_signing_algorithm_from_str_rejects_unknown() {
        scenarios!(
            run = |s| s.parse::<identity_config::SigningAlgorithm>().map_err(drop);
            "supported ES256" {
                "ES256" => Yields(identity_config::SigningAlgorithm::Es256),
            }

            "unsupported RS256" {
                "RS256" => Fails,
            }
        );
    }
}
