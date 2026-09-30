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
use std::str::FromStr;

use carbide_uuid::UuidConversionError;
use carbide_uuid::instance::InstanceId;
use config_version::ConfigVersion;
use model::tenant::{
    ClientSecretBasic, IdentityConfig, IdentityConfigValidationBounds,
    IdentityConfigValidationError, PublicKey, Tenant, TenantIdentityConfigDecrypted, TenantKeyset,
    TenantKeysetContent, TenantKeysetId, TenantKeysetIdentifier, TenantKeysetSearchFilter,
    TenantPublicKey, TenantPublicKeyValidationRequest, TenantSearchFilter, TokenDelegation,
    TokenDelegationAuthMethod, TokenDelegationAuthMethodConfig, TokenDelegationValidationBounds,
    TokenDelegationValidationError, UpdateTenantKeyset, compute_client_secret_hash,
    truncate_hash_for_display,
};

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::forge as rpc_forge;

impl From<rpc::forge::TenantSearchFilter> for TenantSearchFilter {
    fn from(filter: rpc::forge::TenantSearchFilter) -> Self {
        TenantSearchFilter {
            tenant_organization_name: filter.tenant_organization_name,
        }
    }
}

impl From<rpc::forge::TenantKeysetSearchFilter> for TenantKeysetSearchFilter {
    fn from(filter: rpc::forge::TenantKeysetSearchFilter) -> Self {
        TenantKeysetSearchFilter {
            tenant_org_id: filter.tenant_org_id,
        }
    }
}

impl TryFrom<Tenant> for rpc::forge::Tenant {
    type Error = RpcDataConversionError;

    fn try_from(src: Tenant) -> Result<Self, Self::Error> {
        Ok(Self {
            organization_id: src.organization_id.to_string(),
            metadata: Some(src.metadata.into()),
            version: src.version.version_string(),
            routing_profile_type: src.routing_profile_type,
        })
    }
}

impl TryFrom<Tenant> for rpc::forge::CreateTenantResponse {
    type Error = RpcDataConversionError;

    fn try_from(value: Tenant) -> Result<Self, Self::Error> {
        Ok(rpc::forge::CreateTenantResponse {
            tenant: Some(value.try_into()?),
        })
    }
}

impl TryFrom<Tenant> for rpc::forge::FindTenantResponse {
    type Error = RpcDataConversionError;

    fn try_from(value: Tenant) -> Result<Self, Self::Error> {
        Ok(rpc::forge::FindTenantResponse {
            tenant: Some(value.try_into()?),
            permitted_routing_profile_types: vec![],
        })
    }
}

impl TryFrom<Tenant> for rpc::forge::UpdateTenantResponse {
    type Error = RpcDataConversionError;

    fn try_from(value: Tenant) -> Result<Self, Self::Error> {
        Ok(rpc::forge::UpdateTenantResponse {
            tenant: Some(value.try_into()?),
        })
    }
}

impl TryFrom<rpc::forge::Tenant> for Tenant {
    type Error = RpcDataConversionError;

    fn try_from(src: rpc::forge::Tenant) -> Result<Self, Self::Error> {
        let metadata = src
            .metadata
            .ok_or(RpcDataConversionError::MissingArgument("metadata"))?;
        let version = src
            .version
            .parse::<ConfigVersion>()
            .map_err(|_| RpcDataConversionError::InvalidConfigVersion(src.version))?;
        let organization_id = src
            .organization_id
            .clone()
            .try_into()
            .map_err(|_| RpcDataConversionError::InvalidTenantOrg(src.organization_id))?;

        Ok(Self {
            organization_id,
            metadata: metadata.try_into()?,
            routing_profile_type: src.routing_profile_type,
            version,
        })
    }
}

impl From<rpc::forge::TenantPublicKey> for TenantPublicKey {
    fn from(src: rpc::forge::TenantPublicKey) -> Self {
        let public_key: PublicKey = src.public_key.parse().expect("Key parsing can never fail.");
        Self {
            public_key,
            comment: src.comment,
        }
    }
}

impl From<TenantPublicKey> for rpc::forge::TenantPublicKey {
    fn from(src: TenantPublicKey) -> Self {
        Self {
            public_key: src.public_key.to_string(),
            comment: src.comment,
        }
    }
}

impl From<rpc::forge::TenantKeysetContent> for TenantKeysetContent {
    fn from(src: rpc::forge::TenantKeysetContent) -> Self {
        Self {
            public_keys: src.public_keys.into_iter().map(|x| x.into()).collect(),
        }
    }
}

impl From<TenantKeysetContent> for rpc::forge::TenantKeysetContent {
    fn from(src: TenantKeysetContent) -> Self {
        Self {
            public_keys: src.public_keys.into_iter().map(|x| x.into()).collect(),
        }
    }
}

impl TryFrom<rpc::forge::TenantKeysetIdentifier> for TenantKeysetIdentifier {
    type Error = RpcDataConversionError;

    fn try_from(src: rpc::forge::TenantKeysetIdentifier) -> Result<Self, Self::Error> {
        Ok(Self {
            organization_id: src
                .organization_id
                .clone()
                .try_into()
                .map_err(|_| RpcDataConversionError::InvalidTenantOrg(src.organization_id))?,
            keyset_id: src.keyset_id,
        })
    }
}

impl From<TenantKeysetIdentifier> for rpc::forge::TenantKeysetIdentifier {
    fn from(src: TenantKeysetIdentifier) -> Self {
        Self {
            organization_id: src.organization_id.to_string(),
            keyset_id: src.keyset_id,
        }
    }
}

impl TryFrom<rpc::forge::TenantKeyset> for TenantKeyset {
    type Error = RpcDataConversionError;

    fn try_from(src: rpc::forge::TenantKeyset) -> Result<Self, Self::Error> {
        let keyset_identifier: TenantKeysetIdentifier = src
            .keyset_identifier
            .ok_or(RpcDataConversionError::MissingArgument(
                "tenant keyset identifier",
            ))?
            .try_into()?;

        let keyset_content: TenantKeysetContent = src
            .keyset_content
            .ok_or(RpcDataConversionError::MissingArgument(
                "tenant keyset content",
            ))?
            .into();
        let version = src.version;

        Ok(Self {
            keyset_content,
            keyset_identifier,
            version,
        })
    }
}

impl From<TenantKeyset> for rpc::forge::TenantKeyset {
    fn from(src: TenantKeyset) -> Self {
        Self {
            keyset_identifier: Some(src.keyset_identifier.into()),
            keyset_content: Some(src.keyset_content.into()),
            version: src.version,
        }
    }
}

impl TryFrom<rpc::forge::CreateTenantKeysetRequest> for TenantKeyset {
    type Error = RpcDataConversionError;

    fn try_from(src: rpc::forge::CreateTenantKeysetRequest) -> Result<Self, Self::Error> {
        let keyset_identifier: TenantKeysetIdentifier = src
            .keyset_identifier
            .ok_or(RpcDataConversionError::MissingArgument(
                "tenant keyset identifier",
            ))?
            .try_into()?;

        let keyset_content: TenantKeysetContent =
            src.keyset_content
                .map(|x| x.into())
                .unwrap_or(TenantKeysetContent {
                    public_keys: vec![],
                });

        let version = src.version;

        Ok(Self {
            keyset_content,
            keyset_identifier,
            version,
        })
    }
}

impl TryFrom<rpc::forge::UpdateTenantKeysetRequest> for UpdateTenantKeyset {
    type Error = RpcDataConversionError;

    fn try_from(src: rpc::forge::UpdateTenantKeysetRequest) -> Result<Self, Self::Error> {
        let keyset_identifier: TenantKeysetIdentifier = src
            .keyset_identifier
            .ok_or(RpcDataConversionError::MissingArgument(
                "tenant keyset identifier",
            ))?
            .try_into()?;

        let keyset_content: TenantKeysetContent =
            src.keyset_content
                .map(|x| x.into())
                .unwrap_or(TenantKeysetContent {
                    public_keys: vec![],
                });

        Ok(Self {
            keyset_content,
            keyset_identifier,
            version: src.version,
            if_version_match: src.if_version_match,
        })
    }
}

/// Converts stored config to response oneof. Truncates hashes for display.
/// Only used when auth_method is ClientSecretBasic; for None the oneof is omitted.
pub fn stored_to_response_auth_config(
    auth_method: TokenDelegationAuthMethod,
    stored: Option<ClientSecretBasic>,
) -> Option<rpc_forge::token_delegation_response::AuthMethodConfig> {
    match auth_method {
        TokenDelegationAuthMethod::ClientSecretBasic => {
            stored.filter(|s| !s.client_secret.is_empty()).map(|s| {
                let hash = compute_client_secret_hash(&s.client_secret);
                rpc_forge::token_delegation_response::AuthMethodConfig::ClientSecretBasic(
                    rpc_forge::ClientSecretBasicResponse {
                        client_id: s.client_id,
                        client_secret_hash: truncate_hash_for_display(&hash),
                    },
                )
            })
        }
        TokenDelegationAuthMethod::None => None,
    }
}

/// Validates gRPC `TokenDelegation` and converts, including optional `token_endpoint` domain allowlist.
/// When the allowlist is non-empty, `token_endpoint` must be an **`http://` or `https://` URL** with a DNS hostname (not an IP literal).
pub fn token_delegation_try_from_proto(
    value: rpc_forge::TokenDelegation,
    bounds: &TokenDelegationValidationBounds,
) -> Result<TokenDelegation, TokenDelegationValidationError> {
    if value.token_endpoint.is_empty() {
        return Err(TokenDelegationValidationError(
            "token_endpoint is required".to_string(),
        ));
    }
    if value.subject_token_audience.is_empty() {
        return Err(TokenDelegationValidationError(
            "subject_token_audience is required".to_string(),
        ));
    }
    if !bounds.token_endpoint_domain_allowlist.is_empty() {
        let host = model::tenant::identity_config_policy::registered_host_for_token_endpoint(
            &value.token_endpoint,
        )
        .map_err(TokenDelegationValidationError)?;
        model::tenant::identity_config_policy::token_endpoint_domain_matches_allowlist(
            &host,
            &bounds.token_endpoint_domain_allowlist,
        )
        .map_err(TokenDelegationValidationError)?;
    }
    let auth_method_config = match value.auth_method_config {
        None => TokenDelegationAuthMethodConfig::None,
        Some(rpc_forge::token_delegation::AuthMethodConfig::ClientSecretBasic(c)) => {
            if c.client_id.is_empty() {
                return Err(TokenDelegationValidationError(
                    "client_id is required".to_string(),
                ));
            }
            if c.client_secret.is_empty() {
                return Err(TokenDelegationValidationError(
                    "client_secret is required".to_string(),
                ));
            }
            TokenDelegationAuthMethodConfig::ClientSecretBasic {
                client_id: c.client_id,
                client_secret: c.client_secret,
            }
        }
    };
    Ok(TokenDelegation {
        token_endpoint: value.token_endpoint,
        subject_token_audience: value.subject_token_audience,
        auth_method_config,
    })
}

impl TryFrom<rpc_forge::TokenDelegation> for TokenDelegation {
    type Error = TokenDelegationValidationError;

    fn try_from(value: rpc_forge::TokenDelegation) -> Result<Self, Self::Error> {
        token_delegation_try_from_proto(value, &TokenDelegationValidationBounds::default())
    }
}

impl TryFrom<TenantIdentityConfigDecrypted> for rpc_forge::TokenDelegationResponse {
    type Error = RpcDataConversionError;

    fn try_from(value: TenantIdentityConfigDecrypted) -> Result<Self, Self::Error> {
        let row = value.row;
        let token_endpoint = row
            .token_endpoint
            .ok_or(RpcDataConversionError::MissingArgument("token_delegation"))?;
        let auth_method = row
            .auth_method
            .ok_or(RpcDataConversionError::MissingArgument("token_delegation"))?;

        let stored: Option<ClientSecretBasic> = value
            .auth_method_config
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let auth_method_config = match auth_method {
            TokenDelegationAuthMethod::None => None,
            TokenDelegationAuthMethod::ClientSecretBasic => Some(
                stored_to_response_auth_config(auth_method, stored).ok_or_else(|| {
                    RpcDataConversionError::InvalidArgument(
                        "Stored auth_method_config does not match auth_method".to_string(),
                    )
                })?,
            ),
        };

        let created_at = row.token_delegation_created_at.map(rpc::Timestamp::from);

        Ok(rpc_forge::TokenDelegationResponse {
            organization_id: row.organization_id.as_str().to_string(),
            token_endpoint,
            auth_method_config,
            subject_token_audience: row.subject_token_audience.unwrap_or_default(),
            created_at,
            updated_at: Some(rpc::Timestamp::from(row.updated_at)),
        })
    }
}

impl TryFrom<rpc::forge::ValidateTenantPublicKeyRequest> for TenantPublicKeyValidationRequest {
    type Error = UuidConversionError;
    fn try_from(value: rpc::forge::ValidateTenantPublicKeyRequest) -> Result<Self, Self::Error> {
        let instance_id = InstanceId::from_str(&value.instance_id)?;
        Ok(TenantPublicKeyValidationRequest {
            instance_id,
            public_key: value.tenant_public_key,
        })
    }
}

impl From<TenantKeysetId> for rpc::forge::TenantKeysetIdentifier {
    fn from(src: TenantKeysetId) -> Self {
        Self {
            organization_id: src.organization_id,
            keyset_id: src.keyset_id,
        }
    }
}

/// Validates gRPC `TenantIdentityConfig` and converts to `IdentityConfig`, including SPIFFE
/// `subject_prefix` resolution against `issuer` (optional proto field defaults to
/// `spiffe://<trust-domain-from-issuer>`).
pub fn identity_config_try_from_proto(
    value: rpc_forge::TenantIdentityConfig,
    bounds: &IdentityConfigValidationBounds,
) -> Result<IdentityConfig, IdentityConfigValidationError> {
    if value.default_audience.is_empty() {
        return Err(IdentityConfigValidationError(
            "default_audience is required".to_string(),
        ));
    }
    let (issuer, issuer_td) = model::tenant::identity_config::Issuer::parse(&value.issuer)
        .map_err(|e| IdentityConfigValidationError(e.0))?;
    model::tenant::identity_config_policy::trust_domain_matches_allowlist(
        &issuer_td,
        &bounds.trust_domain_allowlist,
    )
    .map_err(IdentityConfigValidationError)?;
    let subject_prefix = model::tenant::identity_config_policy::resolve_subject_prefix(
        &issuer_td,
        value.subject_prefix.as_deref(),
    )
    .map_err(IdentityConfigValidationError)?;
    if value.token_ttl_sec == 0 {
        return Err(IdentityConfigValidationError(format!(
            "token_ttl_sec is required (must be between {} and {} seconds)",
            bounds.token_ttl_min_sec, bounds.token_ttl_max_sec
        )));
    }
    if value.token_ttl_sec < bounds.token_ttl_min_sec
        || value.token_ttl_sec > bounds.token_ttl_max_sec
    {
        return Err(IdentityConfigValidationError(format!(
            "token_ttl_sec must be between {} and {} seconds",
            bounds.token_ttl_min_sec, bounds.token_ttl_max_sec
        )));
    }
    if !value.allowed_audiences.is_empty()
        && !value
            .allowed_audiences
            .iter()
            .any(|a| a == &value.default_audience)
    {
        return Err(IdentityConfigValidationError(
            "default_audience must be in allowed_audiences".to_string(),
        ));
    }
    if let Some(s) = value.signing_key_overlap_sec
        && s > bounds.signing_key_overlap_max_sec
    {
        return Err(IdentityConfigValidationError(format!(
            "signing_key_overlap_sec must not exceed {} seconds",
            bounds.signing_key_overlap_max_sec
        )));
    }
    if !value.rotate_key && value.signing_key_overlap_sec.is_some() {
        return Err(IdentityConfigValidationError(
            "signing_key_overlap_sec may only be set when rotate_key is true".to_string(),
        ));
    }

    let signing_key_overlap_sec = match value.signing_key_overlap_sec {
        None => None,
        Some(s) => Some(i32::try_from(s).map_err(|_| {
            IdentityConfigValidationError("signing_key_overlap_sec out of range".to_string())
        })?),
    };

    Ok(IdentityConfig {
        issuer,
        default_audience: value.default_audience,
        allowed_audiences: value.allowed_audiences,
        token_ttl_sec: value.token_ttl_sec,
        subject_prefix,
        enabled: value.enabled,
        rotate_key: value.rotate_key,
        algorithm: bounds.algorithm,
        encryption_key_id: bounds.encryption_key_id.clone(),
        signing_key_overlap_sec,
    })
}

/// Ensures rotation requests carry overlap at least [`IdentityConfig::token_ttl_sec`] (see docs).
pub fn validate_identity_overlap_for_rotation(
    config: &IdentityConfig,
) -> Result<(), IdentityConfigValidationError> {
    if !config.rotate_key {
        return Ok(());
    }
    let Some(overlap) = config.signing_key_overlap_sec else {
        return Err(IdentityConfigValidationError(
            "signing_key_overlap_sec is required when rotate_key is true".to_string(),
        ));
    };
    let overlap_u32 = u32::try_from(overlap).map_err(|_| {
        IdentityConfigValidationError("signing_key_overlap_sec out of range".to_string())
    })?;
    if overlap_u32 < config.token_ttl_sec {
        return Err(IdentityConfigValidationError(format!(
            "signing_key_overlap_sec ({overlap_u32}) must be at least token_ttl_sec ({}) \
             so JWTs signed with the previous key stay verifiable until they expire",
            config.token_ttl_sec
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{
        Case, Check, check_cases, check_values, scenarios, value_scenarios,
    };
    use model::tenant::{identity_config, validate_trust_domain_allowlist_patterns};

    use super::*;
    use crate::forge as rpc_forge;
    use crate::forge::token_delegation_response::AuthMethodConfig;

    /// Builds identity-config validation bounds, varying only the two fields the
    /// per-case tables exercise (trust-domain allowlist and master key id).
    fn identity_bounds(
        trust_domain_allowlist: Vec<String>,
        encryption_key_id: &str,
    ) -> IdentityConfigValidationBounds {
        IdentityConfigValidationBounds {
            token_ttl_min_sec: 60,
            token_ttl_max_sec: 86400,
            algorithm: identity_config::SigningAlgorithm::Es256,
            encryption_key_id: encryption_key_id.parse().unwrap(),
            trust_domain_allowlist,
            signing_key_overlap_max_sec: 604_800,
        }
    }

    /// Projects a stored auth config to the fields the originals asserted:
    /// the client id and three display-hash properties.
    fn project_auth_config(out: Option<AuthMethodConfig>) -> Option<(String, bool, bool, bool)> {
        out.map(|AuthMethodConfig::ClientSecretBasic(c)| {
            (
                c.client_id,
                c.client_secret_hash.starts_with("sha256:"),
                c.client_secret_hash.ends_with(".."),
                !c.client_secret_hash.is_empty(),
            )
        })
    }

    // stored_to_response_auth_config: maps stored config to the response oneof,
    // hashing/truncating the client secret and dropping empty/None secrets.
    #[test]
    fn stored_to_response_auth_config_cases() {
        value_scenarios!(
            run = |(auth_method, stored)| {
                project_auth_config(stored_to_response_auth_config(auth_method, stored))
            };
            "auth method None yields no config" {
                (TokenDelegationAuthMethod::None, None) => None,
            }

            "client secret basic yields truncated hash" {
                (
                    TokenDelegationAuthMethod::ClientSecretBasic,
                    Some(ClientSecretBasic {
                        client_id: "my-client".to_string(),
                        client_secret: "secret".to_string(),
                    }),
                ) => Some(("my-client".to_string(), true, true, true)),
            }

            "empty client secret yields no config" {
                (
                    TokenDelegationAuthMethod::ClientSecretBasic,
                    Some(ClientSecretBasic {
                        client_id: "x".to_string(),
                        client_secret: String::new(),
                    }),
                ) => None,
            }
        );
    }

    // The hashed client secret in the response never exposes the cleartext.
    #[test]
    fn stored_to_response_auth_config_hash_omits_cleartext() {
        let out = stored_to_response_auth_config(
            TokenDelegationAuthMethod::ClientSecretBasic,
            Some(ClientSecretBasic {
                client_id: "my-client".to_string(),
                client_secret: "secret".to_string(),
            }),
        )
        .expect("client secret basic yields a config");
        let AuthMethodConfig::ClientSecretBasic(c) = out;
        assert!(
            !c.client_secret_hash.contains("secret"),
            "hash must not expose the cleartext secret: {}",
            c.client_secret_hash
        );
    }

    #[test]
    fn token_delegation_to_db_format_client_secret_basic_hash() {
        let config = TokenDelegation {
            token_endpoint: "https://auth.example.com/token".to_string(),
            subject_token_audience: "https://api.example.com".to_string(),
            auth_method_config: TokenDelegationAuthMethodConfig::ClientSecretBasic {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
            },
        };
        let (auth_method, config_json) = config.to_db_format();
        assert_eq!(auth_method, TokenDelegationAuthMethod::ClientSecretBasic);
        let stored: ClientSecretBasic = serde_json::from_str(&config_json).unwrap();
        assert_eq!(stored.client_id, "client");
        assert_eq!(stored.client_secret, "secret");
        // Hash is computed on the fly when retrieving
        let hash = compute_client_secret_hash("secret");
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 7 + 64);
    }

    #[test]
    fn identity_config_try_from_proto_success() {
        let proto = rpc_forge::TenantIdentityConfig {
            enabled: true,
            issuer: "https://issuer.example.com".to_string(),
            default_audience: "api".to_string(),
            allowed_audiences: vec!["api".to_string(), "other".to_string()],
            token_ttl_sec: 3600,
            subject_prefix: None,
            rotate_key: false,
            signing_key_overlap_sec: None,
        };
        let bounds = IdentityConfigValidationBounds {
            token_ttl_min_sec: 60,
            token_ttl_max_sec: 86400,
            algorithm: identity_config::SigningAlgorithm::Es256,
            encryption_key_id: "test-master".parse().unwrap(),
            trust_domain_allowlist: vec![],
            signing_key_overlap_max_sec: 604_800,
        };
        let config = identity_config_try_from_proto(proto, &bounds).unwrap();
        assert_eq!(config.issuer.as_str(), "https://issuer.example.com");
        assert_eq!(config.default_audience, "api");
        assert_eq!(config.allowed_audiences, vec!["api", "other"]);
        assert_eq!(config.token_ttl_sec, 3600);
        assert_eq!(config.subject_prefix, "spiffe://issuer.example.com");
        assert!(config.enabled);
        assert!(!config.rotate_key);
        assert_eq!(config.algorithm, identity_config::SigningAlgorithm::Es256);
        assert_eq!(config.encryption_key_id.as_str(), "test-master");
    }

    // identity_config_try_from_proto success: each row asserts the normalized
    // issuer and resolved SPIFFE subject_prefix produced from the proto + bounds.
    #[test]
    fn identity_config_try_from_proto_success_cases() {
        struct Row {
            scenario: &'static str,
            issuer: &'static str,
            subject_prefix: Option<&'static str>,
            allowlist: Vec<String>,
            expect_issuer: &'static str,
            expect_subject_prefix: &'static str,
        }
        let rows = [
            Row {
                scenario: "issuer is lowercased and normalized",
                issuer: "HTTPS://Issuer.EXAMPLE.COM/wl",
                subject_prefix: None,
                allowlist: vec![],
                expect_issuer: "https://issuer.example.com/wl",
                expect_subject_prefix: "spiffe://issuer.example.com",
            },
            Row {
                scenario: "custom subject_prefix in proto is honored",
                issuer: "https://issuer.example.com",
                subject_prefix: Some("spiffe://issuer.example.com/workloads"),
                allowlist: vec![],
                expect_issuer: "https://issuer.example.com",
                expect_subject_prefix: "spiffe://issuer.example.com/workloads",
            },
            Row {
                scenario: "empty optional subject_prefix defaults",
                issuer: "https://issuer.example.com",
                subject_prefix: Some(""),
                allowlist: vec![],
                expect_issuer: "https://issuer.example.com",
                expect_subject_prefix: "spiffe://issuer.example.com",
            },
            Row {
                scenario: "trust domain matching wildcard allowlist",
                issuer: "https://auth.login.example.com",
                subject_prefix: None,
                allowlist: vec!["**.login.example.com".to_string()],
                expect_issuer: "https://auth.login.example.com",
                expect_subject_prefix: "spiffe://auth.login.example.com",
            },
        ];
        check_cases(
            rows.map(|r| Case {
                scenario: r.scenario,
                input: (r.issuer, r.subject_prefix, r.allowlist),
                expect: Yields((
                    r.expect_issuer.to_string(),
                    r.expect_subject_prefix.to_string(),
                )),
            }),
            |(issuer, subject_prefix, allowlist)| {
                let proto = rpc_forge::TenantIdentityConfig {
                    enabled: true,
                    issuer: issuer.to_string(),
                    default_audience: "api".to_string(),
                    allowed_audiences: vec![],
                    token_ttl_sec: 3600,
                    subject_prefix: subject_prefix.map(|s| s.to_string()),
                    rotate_key: false,
                    signing_key_overlap_sec: None,
                };
                let bounds = identity_bounds(allowlist, "test-master");
                let config = identity_config_try_from_proto(proto, &bounds).map_err(drop)?;
                Ok::<_, ()>((config.issuer.as_str().to_string(), config.subject_prefix))
            },
        );
    }

    // identity_config_try_from_proto rejections: each row's error message must
    // contain every listed substring (the user-facing validation contract).
    #[test]
    fn identity_config_try_from_proto_error_cases() {
        struct Row {
            scenario: &'static str,
            proto: rpc_forge::TenantIdentityConfig,
            allowlist: Vec<String>,
            wants: Vec<&'static str>,
        }
        fn proto(
            issuer: &str,
            default_audience: &str,
            token_ttl_sec: u32,
            subject_prefix: Option<&str>,
            signing_key_overlap_sec: Option<u32>,
        ) -> rpc_forge::TenantIdentityConfig {
            rpc_forge::TenantIdentityConfig {
                enabled: true,
                issuer: issuer.to_string(),
                default_audience: default_audience.to_string(),
                allowed_audiences: vec![],
                token_ttl_sec,
                subject_prefix: subject_prefix.map(|s| s.to_string()),
                rotate_key: false,
                signing_key_overlap_sec,
            }
        }
        let rows = [
            Row {
                scenario: "empty issuer",
                proto: proto("", "api", 3600, None, None),
                allowlist: vec![],
                wants: vec!["issuer is required"],
            },
            Row {
                scenario: "empty default_audience",
                proto: proto("https://issuer.example.com", "", 3600, None, None),
                allowlist: vec![],
                wants: vec!["default_audience is required"],
            },
            Row {
                scenario: "non-spiffe subject_prefix",
                proto: proto(
                    "https://issuer.example.com",
                    "api",
                    3600,
                    Some("https://issuer.example.com/p"),
                    None,
                ),
                allowlist: vec![],
                wants: vec!["spiffe://"],
            },
            Row {
                scenario: "subject_prefix trust-domain mismatch",
                proto: proto(
                    "https://issuer.example.com",
                    "api",
                    3600,
                    Some("spiffe://other.example/wl"),
                    None,
                ),
                allowlist: vec![],
                wants: vec!["does not match"],
            },
            Row {
                scenario: "token_ttl_sec zero",
                proto: proto("https://issuer.example.com", "api", 0, None, None),
                allowlist: vec![],
                wants: vec!["token_ttl_sec"],
            },
            Row {
                scenario: "token_ttl_sec below min",
                proto: proto("https://issuer.example.com", "api", 30, None, None),
                allowlist: vec![],
                wants: vec!["token_ttl_sec must be between"],
            },
            Row {
                scenario: "token_ttl_sec above max",
                proto: proto("https://issuer.example.com", "api", 100000, None, None),
                allowlist: vec![],
                wants: vec!["token_ttl_sec must be between"],
            },
            Row {
                scenario: "trust domain not on allowlist",
                proto: proto("https://evil.example.com", "api", 3600, None, None),
                allowlist: vec!["login.example.com".to_string()],
                wants: vec!["trust domain", "allowlist"],
            },
            Row {
                scenario: "no allowlist entry matches",
                proto: proto("https://idp.other.example/", "api", 3600, None, None),
                allowlist: vec![
                    "login.example.com".to_string(),
                    "*.tenant.example.net".to_string(),
                ],
                wants: vec!["allowlist"],
            },
            Row {
                scenario: "overlap set when not rotating",
                proto: proto("https://issuer.example.com", "api", 3600, None, Some(120)),
                allowlist: vec![],
                wants: vec!["signing_key_overlap_sec may only be set"],
            },
        ];
        check_values(
            rows.map(|r| Check {
                scenario: r.scenario,
                input: (r.proto, r.allowlist, r.wants),
                expect: true,
            }),
            |(proto, allowlist, wants)| {
                let bounds = identity_bounds(allowlist, "test");
                let err = identity_config_try_from_proto(proto, &bounds).unwrap_err();
                wants.iter().all(|w| err.0.contains(w))
            },
        );
    }

    #[test]
    fn identity_config_try_from_proto_accepts_issuer_matching_second_allowlist_entry() {
        let allowlist = vec![
            "login.example.com".to_string(),
            "idp.other.example".to_string(),
            "*.tenant.example.net".to_string(),
        ];
        assert!(
            validate_trust_domain_allowlist_patterns(&allowlist).is_ok(),
            "fixture patterns valid at startup"
        );
        let proto = rpc_forge::TenantIdentityConfig {
            enabled: true,
            issuer: "https://idp.other.example/oidc".to_string(),
            default_audience: "api".to_string(),
            allowed_audiences: vec![],
            token_ttl_sec: 3600,
            subject_prefix: None,
            rotate_key: false,
            signing_key_overlap_sec: None,
        };
        let bounds = IdentityConfigValidationBounds {
            token_ttl_min_sec: 60,
            token_ttl_max_sec: 86400,
            algorithm: identity_config::SigningAlgorithm::Es256,
            encryption_key_id: "test".parse().unwrap(),
            trust_domain_allowlist: allowlist,
            signing_key_overlap_max_sec: 604_800,
        };
        let config = identity_config_try_from_proto(proto, &bounds).unwrap();
        assert_eq!(config.issuer.as_str(), "https://idp.other.example/oidc");
        assert_eq!(config.subject_prefix, "spiffe://idp.other.example");
    }

    // validate_identity_overlap_for_rotation: a missing overlap and an
    // overlap shorter than token_ttl_sec are rejected (with the listed
    // substring); a sufficient overlap is accepted (`None` want = Ok).
    #[test]
    fn validate_identity_overlap_cases() {
        fn rotating_config(signing_key_overlap_sec: Option<i32>) -> IdentityConfig {
            IdentityConfig {
                issuer: "https://issuer.example.com".parse().unwrap(),
                default_audience: "api".to_string(),
                allowed_audiences: vec![],
                token_ttl_sec: 3600,
                subject_prefix: "spiffe://issuer.example.com".to_string(),
                enabled: true,
                rotate_key: true,
                algorithm: identity_config::SigningAlgorithm::Es256,
                encryption_key_id: "test".parse().unwrap(),
                signing_key_overlap_sec,
            }
        }
        value_scenarios!(
            run = |(overlap, want): (Option<i32>, Option<&str>)| {
                let result = validate_identity_overlap_for_rotation(&rotating_config(overlap));
                match want {
                    Some(substr) => result.unwrap_err().0.contains(substr),
                    None => result.is_ok(),
                }
            };
            "missing overlap is rejected" {
                (None, Some("signing_key_overlap_sec is required")) => true,
            }

            "overlap shorter than token_ttl_sec is rejected" {
                (Some(120), Some("must be at least token_ttl_sec")) => true,
            }

            "overlap at least token_ttl_sec is accepted" {
                (Some(3600), None) => true,
            }
        );
    }

    /// Projects a converted `TokenDelegation` to the endpoint, audience, and the
    /// auth-method-config fields the originals asserted.
    #[allow(clippy::type_complexity)]
    fn project_token_delegation(
        config: TokenDelegation,
    ) -> (String, String, Option<(String, String)>) {
        let auth = match config.auth_method_config {
            TokenDelegationAuthMethodConfig::None => None,
            TokenDelegationAuthMethodConfig::ClientSecretBasic {
                client_id,
                client_secret,
            } => Some((client_id, client_secret)),
        };
        (config.token_endpoint, config.subject_token_audience, auth)
    }

    // TokenDelegation::try_from success: the endpoint and audience pass through,
    // and the auth method config maps to None or the client-secret-basic pair.
    #[test]
    fn token_delegation_try_from_success_cases() {
        scenarios!(
            run = |proto| {
                let config = TokenDelegation::try_from(proto).map_err(drop)?;
                Ok::<_, ()>(project_token_delegation(config))
            };
            "no auth method config" {
                rpc_forge::TokenDelegation {
                    token_endpoint: "https://auth.example.com/token".to_string(),
                    subject_token_audience: "https://api.example.com".to_string(),
                    auth_method_config: None,
                } => Yields((
                    "https://auth.example.com/token".to_string(),
                    "https://api.example.com".to_string(),
                    None,
                )),
            }

            "client secret basic" {
                rpc_forge::TokenDelegation {
                    token_endpoint: "https://auth.example.com/token".to_string(),
                    subject_token_audience: "https://api.example.com".to_string(),
                    auth_method_config: Some(
                        rpc_forge::token_delegation::AuthMethodConfig::ClientSecretBasic(
                            rpc_forge::ClientSecretBasic {
                                client_id: "my-client".to_string(),
                                client_secret: "my-secret".to_string(),
                            },
                        ),
                    ),
                } => Yields((
                    "https://auth.example.com/token".to_string(),
                    "https://api.example.com".to_string(),
                    Some(("my-client".to_string(), "my-secret".to_string())),
                )),
            }
        );
    }

    // TokenDelegation::try_from rejections: each row's error message must contain
    // the listed substring (the user-facing validation contract).
    #[test]
    fn token_delegation_try_from_error_cases() {
        fn client_secret_basic(
            client_id: &str,
            client_secret: &str,
        ) -> Option<rpc_forge::token_delegation::AuthMethodConfig> {
            Some(
                rpc_forge::token_delegation::AuthMethodConfig::ClientSecretBasic(
                    rpc_forge::ClientSecretBasic {
                        client_id: client_id.to_string(),
                        client_secret: client_secret.to_string(),
                    },
                ),
            )
        }
        value_scenarios!(
            run = |(proto, want): (rpc_forge::TokenDelegation, &str)| {
                TokenDelegation::try_from(proto)
                    .unwrap_err()
                    .0
                    .contains(want)
            };
            "empty token_endpoint" {
                (
                    rpc_forge::TokenDelegation {
                        token_endpoint: String::new(),
                        subject_token_audience: "https://api.example.com".to_string(),
                        auth_method_config: None,
                    },
                    "token_endpoint is required",
                ) => true,
            }

            "empty subject_token_audience" {
                (
                    rpc_forge::TokenDelegation {
                        token_endpoint: "https://auth.example.com/token".to_string(),
                        subject_token_audience: String::new(),
                        auth_method_config: None,
                    },
                    "subject_token_audience is required",
                ) => true,
            }

            "empty client_id" {
                (
                    rpc_forge::TokenDelegation {
                        token_endpoint: "https://auth.example.com/token".to_string(),
                        subject_token_audience: "https://api.example.com".to_string(),
                        auth_method_config: client_secret_basic("", "secret"),
                    },
                    "client_id is required",
                ) => true,
            }

            "empty client_secret" {
                (
                    rpc_forge::TokenDelegation {
                        token_endpoint: "https://auth.example.com/token".to_string(),
                        subject_token_audience: "https://api.example.com".to_string(),
                        auth_method_config: client_secret_basic("client", ""),
                    },
                    "client_secret is required",
                ) => true,
            }
        );
    }
}
