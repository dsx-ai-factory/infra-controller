/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use it except in compliance with the License.
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

//! gRPC handlers for tenant_identity_config table.
//! Identity config: issuer, audiences, TTL, signing key (Get/Set/Delete).
//! Token delegation: token exchange config for external IdP (Get/Set/Delete).
//! JWKS and OpenID discovery RPCs live in [`machine_identity`](super::machine_identity).
//! (Proto message `forge.TenantIdentityConfig` is aliased as `ProtoTenantIdentityConfig` to avoid
//! clashing with the database row type [`TenantIdentityConfig`](TenantIdentityConfig).)

use ::rpc::Timestamp;
use ::rpc::forge::{
    GetTenantIdentityConfigRequest, GetTokenDelegationRequest, ReencryptTenantIdentityFailure,
    ReencryptTenantIdentitySecretsRequest, ReencryptTenantIdentitySecretsResponse,
    SetTenantIdentityConfigRequest, TenantIdentityConfig as ProtoTenantIdentityConfig,
    TenantIdentityConfigResponse, TenantIdentitySigningKey, TokenDelegationRequest,
    TokenDelegationResponse, token_delegation,
};
use carbide_secrets::credentials::CredentialReader;
use carbide_secrets::key_encryption;
use carbide_utils::none_if_empty::NoneIfEmpty;
use db::{ConditionalWrite, WithTransaction, tenant, tenant_identity_config};
use model::tenant::identity_config::TenantIdentityCurrentSigningKeySlot;
use model::tenant::{
    EncryptedSigningPrivateKey, EncryptedTokenDelegationAuthConfig, EncryptionKeyId,
    IdentityConfigValidationBounds, IdentityConfigValidationError, InvalidNonEmptyStr,
    InvalidTenantOrg, KeyId, SigningKeyMaterial, SigningPublicKeyPem, TenantIdentityConfig,
    TenantIdentityConfigDecrypted, TenantOrganizationId, TokenDelegation,
    TokenDelegationValidationBounds, TokenDelegationValidationError,
};
use rpc::model::tenant::{identity_config_try_from_proto, validate_identity_overlap_for_rotation};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_request_data_redacted};
use crate::handlers::machine_identity::require_machine_identity_site_enabled;
use crate::machine_identity::{
    MachineIdentityStoredSecretDecryptionFailed, ReencryptBlobOutcome,
    StoredMachineIdentitySecretKind, decrypt_token_delegation_encrypted_blob,
    machine_identity_encryption_secret, reencrypt_ciphertext_if_needed,
};

/// Decrypts DB ciphertext into [`TenantIdentityConfigDecrypted`]: `row` keeps envelope in
/// `encrypted_auth_method_config`; plaintext JSON is only in `auth_method_config`.
async fn tenant_identity_with_decrypted_token_delegation(
    credentials: &dyn CredentialReader,
    cfg: TenantIdentityConfig,
) -> Result<TenantIdentityConfigDecrypted, Status> {
    let auth_method_config = decrypt_token_delegation_encrypted_blob(
        credentials,
        cfg.encrypted_auth_method_config.as_ref(),
    )
    .await
    .inspect_err(|e| {
        carbide_instrument::emit(
            MachineIdentityStoredSecretDecryptionFailed::TokenDelegationAuth {
                organization_id: cfg.organization_id.to_string(),
                error: e.message().to_string(),
            },
        );
    })?;
    Ok(TenantIdentityConfigDecrypted {
        row: cfg,
        auth_method_config,
    })
}

/// Formats TokenDelegationRequest for logging with client_secret redacted.
fn format_token_delegation_request_redacted(req: &TokenDelegationRequest) -> String {
    let config_str = match &req.config {
        None => "None".to_string(),
        Some(cfg) => {
            let auth_method_config = match &cfg.auth_method_config {
                None => "None".to_string(),
                Some(token_delegation::AuthMethodConfig::ClientSecretBasic(c)) => format!(
                    "Some(ClientSecretBasic {{ client_id: \"{}\", client_secret: \"[REDACTED]\" }})",
                    c.client_id
                ),
            };
            format!(
                "Some(TokenDelegation {{ token_endpoint: \"{}\", subject_token_audience: \"{}\", auth_method_config: {} }})",
                cfg.token_endpoint, cfg.subject_token_audience, auth_method_config
            )
        }
    };
    format!(
        "TokenDelegationRequest {{ organization_id: \"{}\", config: {} }}",
        req.organization_id, config_str
    )
}

// --- Tenant identity configuration handlers ---

/// Builds [`TenantIdentitySigningKey`] entries from slotted public JSON; exactly one has
/// `current_signer == true`.
fn tenant_identity_signing_keys_response(
    cfg: &TenantIdentityConfig,
) -> Result<Vec<TenantIdentitySigningKey>, Status> {
    let mut keys = Vec::new();
    if let Some(ref doc) = cfg.signing_key_public_1 {
        let current_signer =
            cfg.current_signing_key_slot == TenantIdentityCurrentSigningKeySlot::SigningKey1;
        keys.push(TenantIdentitySigningKey {
            kid: doc.0.kid().to_string(),
            alg: doc.0.alg().to_string(),
            current_signer,
            expire_at: if current_signer {
                None
            } else {
                cfg.non_active_slot_expires_at.map(Timestamp::from)
            },
        });
    }
    if let Some(ref doc) = cfg.signing_key_public_2 {
        let current_signer =
            cfg.current_signing_key_slot == TenantIdentityCurrentSigningKeySlot::SigningKey2;
        keys.push(TenantIdentitySigningKey {
            kid: doc.0.kid().to_string(),
            alg: doc.0.alg().to_string(),
            current_signer,
            expire_at: if current_signer {
                None
            } else {
                cfg.non_active_slot_expires_at.map(Timestamp::from)
            },
        });
    }
    let n_current = keys.iter().filter(|k| k.current_signer).count();
    if keys.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "tenant identity config has no published signing keys".to_string(),
        )
        .into());
    }
    if n_current != 1 {
        return Err(CarbideError::InvalidArgument(format!(
            "expected exactly one current signer in signing_keys; found {n_current}"
        ))
        .into());
    }
    Ok(keys)
}

/// `Forge::get_tenant_identity_configuration`: fetches per-org identity config.
pub(crate) async fn get_configuration(
    api: &Api,
    request: Request<GetTenantIdentityConfigRequest>,
) -> Result<Response<TenantIdentityConfigResponse>, Status> {
    log_request_data(&request);

    require_machine_identity_site_enabled(api)?;

    let req = request.into_inner();
    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;
    let org_id_str = org_id.as_str().to_string();

    let cfg = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                tenant_identity_config::gc_expired_non_active_signing_key(&org_id, txn).await?;
                tenant_identity_config::find(&org_id, txn.as_mut()).await
            })
        })
        .await??;

    let cfg = match cfg {
        Some(c) => c,
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "tenant_identity_config",
                id: org_id_str.clone(),
            }
            .into());
        }
    };

    let signing_keys = tenant_identity_signing_keys_response(&cfg)?;

    Ok(Response::new(TenantIdentityConfigResponse {
        organization_id: org_id_str,
        config: Some(ProtoTenantIdentityConfig {
            enabled: cfg.enabled,
            issuer: cfg.issuer.as_str().to_string(),
            default_audience: cfg.default_audience.clone(),
            allowed_audiences: cfg.allowed_audiences.0.clone(),
            token_ttl_sec: cfg.token_ttl_sec as u32,
            subject_prefix: Some(cfg.subject_prefix.clone()),
            rotate_key: cfg.response_rotate_key(),
            signing_key_overlap_sec: None,
        }),
        created_at: Some(Timestamp::from(cfg.created_at)),
        updated_at: Some(Timestamp::from(cfg.updated_at)),
        signing_keys,
    }))
}

/// `Forge::delete_tenant_identity_configuration`: removes per-org identity config.
pub(crate) async fn delete_configuration(
    api: &Api,
    request: Request<GetTenantIdentityConfigRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    require_machine_identity_site_enabled(api)?;

    let req = request.into_inner();
    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;
    let org_id_str = org_id.as_str().to_string();

    let deleted = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                let deleted = tenant_identity_config::delete(&org_id, txn).await?;
                if deleted {
                    tenant::increment_version(org_id.as_str(), txn).await?;
                }
                Ok::<_, db::DatabaseError>(deleted)
            })
        })
        .await??;

    if !deleted {
        return Err(CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id_str,
        }
        .into());
    }

    Ok(Response::new(()))
}

/// `Forge::set_tenant_identity_configuration`: upserts per-org identity config into tenant_identity_config.
/// Requires auth. Tenant must exist. Key generation is placeholder until credential-backed key provisioning.
pub(crate) async fn set_configuration(
    api: &Api,
    request: Request<SetTenantIdentityConfigRequest>,
) -> Result<Response<TenantIdentityConfigResponse>, Status> {
    log_request_data(&request);

    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::InvalidArgument(
            "machine identity must be enabled in site config before setting identity configuration"
                .to_string(),
        )
        .into());
    }

    let req = request.into_inner();
    let proto = req.config.ok_or_else(|| {
        CarbideError::InvalidArgument("TenantIdentityConfig is required".to_string())
    })?;
    let config = identity_config_try_from_proto(
        proto,
        &IdentityConfigValidationBounds::from(api.runtime_config.machine_identity.clone()),
    )
    .map_err(|e: IdentityConfigValidationError| CarbideError::InvalidArgument(e.0))?;

    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;
    let org_id_str = org_id.as_str().to_string();

    let org_id_for_find = org_id.clone();
    let existing = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(
                async move { tenant_identity_config::find(&org_id_for_find, txn.as_mut()).await },
            )
        })
        .await??;

    validate_identity_overlap_for_rotation(&config)
        .map_err(|e: IdentityConfigValidationError| CarbideError::InvalidArgument(e.0))?;

    let key_material = match (&existing, config.rotate_key) {
        (None, _) | (_, true) => {
            let encryption_key = machine_identity_encryption_secret(
                &api.credential_manager,
                &config.encryption_key_id,
            )
            .await?;
            let (private_pem, public_pem) = key_encryption::generate_es256_key_pair()
                .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
            let public_pem_trimmed = public_pem.trim();
            let key_id = KeyId::from_public_key_material(public_pem_trimmed);
            let encrypted_signing_key: EncryptedSigningPrivateKey =
                key_encryption::encrypt(&private_pem, &encryption_key, &config.encryption_key_id)
                    .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?
                    .try_into()
                    .map_err(|e: InvalidNonEmptyStr| {
                        CarbideError::InvalidArgument(e.to_string())
                    })?;
            let signing_key_public: SigningPublicKeyPem = public_pem_trimmed
                .to_string()
                .try_into()
                .map_err(|e: InvalidNonEmptyStr| CarbideError::InvalidArgument(e.to_string()))?;
            Some(SigningKeyMaterial {
                key_id,
                encrypted_signing_key,
                signing_key_public,
            })
        }
        (Some(_), false) => None,
    };

    let cfg = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                let tenant_exists = tenant::find(org_id.as_str(), false, txn).await?;
                if tenant_exists.is_none() {
                    return Err(db::DatabaseError::NotFoundError {
                        kind: "Tenant",
                        id: org_id.as_str().to_string(),
                    });
                }
                let cfg = tenant_identity_config::set(&org_id, &config, key_material, txn).await?;
                tenant::increment_version(org_id.as_str(), txn).await?;
                Ok(cfg)
            })
        })
        .await??;

    let signing_keys = tenant_identity_signing_keys_response(&cfg)?;

    Ok(Response::new(TenantIdentityConfigResponse {
        organization_id: org_id_str,
        config: Some(ProtoTenantIdentityConfig {
            enabled: cfg.enabled,
            issuer: cfg.issuer.as_str().to_string(),
            default_audience: cfg.default_audience.clone(),
            allowed_audiences: cfg.allowed_audiences.0.clone(),
            token_ttl_sec: cfg.token_ttl_sec as u32,
            subject_prefix: Some(cfg.subject_prefix.clone()),
            rotate_key: cfg.response_rotate_key(),
            signing_key_overlap_sec: None,
        }),
        created_at: Some(Timestamp::from(cfg.created_at)),
        updated_at: Some(Timestamp::from(cfg.updated_at)),
        signing_keys,
    }))
}

// --- Token delegation handlers ---

pub(crate) async fn get_token_delegation(
    api: &Api,
    request: Request<GetTokenDelegationRequest>,
) -> Result<Response<TokenDelegationResponse>, Status> {
    log_request_data(&request);

    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::InvalidArgument(
            "machine identity must be enabled in site config".to_string(),
        )
        .into());
    }

    let req = request.into_inner();
    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;
    let org_id_str = org_id.as_str().to_string();

    let cfg = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(async move { tenant_identity_config::find(&org_id, txn.as_mut()).await })
        })
        .await??;

    let cfg = match cfg {
        Some(c) => c,
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "tenant_identity_config",
                id: org_id_str.clone(),
            }
            .into());
        }
    };

    if cfg.token_endpoint.is_none() || cfg.auth_method.is_none() {
        return Err(Status::from(CarbideError::NotFoundError {
            kind: "token_delegation",
            id: org_id_str.clone(),
        }));
    }

    let cfg = tenant_identity_with_decrypted_token_delegation(&api.credential_manager, cfg).await?;
    Ok(Response::new(cfg.try_into().map_err(CarbideError::from)?))
}

pub(crate) async fn set_token_delegation(
    api: &Api,
    request: Request<TokenDelegationRequest>,
) -> Result<Response<TokenDelegationResponse>, Status> {
    log_request_data_redacted(format_token_delegation_request_redacted(request.get_ref()));

    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::InvalidArgument(
            "machine identity must be enabled in site config".to_string(),
        )
        .into());
    }

    let req = request.into_inner();
    let config: TokenDelegation = req
        .config
        .as_ref()
        .ok_or_else(|| {
            CarbideError::InvalidArgument("TokenDelegation config is required".to_string())
        })
        .and_then(|c| {
            ::rpc::model::tenant::token_delegation_try_from_proto(
                c.clone(),
                &TokenDelegationValidationBounds::from(api.runtime_config.machine_identity.clone()),
            )
            .map_err(|e: TokenDelegationValidationError| CarbideError::InvalidArgument(e.0))
        })?;
    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id.parse().map_err(|e: InvalidTenantOrg| {
        Status::from(CarbideError::InvalidArgument(e.to_string()))
    })?;

    let org_id_for_find = org_id.clone();
    api.database_connection
        .with_txn(|txn| {
            Box::pin(
                async move { tenant_identity_config::find(&org_id_for_find, txn.as_mut()).await },
            )
        })
        .await??
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id.as_str().to_string(),
        })?;

    let (auth_method, plaintext_json) = config.to_db_format();
    let encryption_key_id =
        IdentityConfigValidationBounds::from(api.runtime_config.machine_identity.clone())
            .encryption_key_id;
    let secret =
        machine_identity_encryption_secret(&api.credential_manager, &encryption_key_id).await?;
    let encrypted_blob: EncryptedTokenDelegationAuthConfig =
        key_encryption::encrypt(plaintext_json.as_bytes(), &secret, &encryption_key_id)
            .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?
            .try_into()
            .map_err(|e: InvalidNonEmptyStr| CarbideError::InvalidArgument(e.to_string()))?;

    let cfg = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                let tenant_exists = tenant::find(org_id.as_str(), false, txn).await?;
                if tenant_exists.is_none() {
                    return Err(db::DatabaseError::NotFoundError {
                        kind: "Tenant",
                        id: org_id.as_str().to_string(),
                    });
                }
                let cfg = tenant_identity_config::set_token_delegation(
                    &org_id,
                    &config,
                    auth_method,
                    &encrypted_blob,
                    txn,
                )
                .await?;
                tenant::increment_version(org_id.as_str(), txn).await?;
                Ok(cfg)
            })
        })
        .await??;

    let cfg = tenant_identity_with_decrypted_token_delegation(&api.credential_manager, cfg).await?;
    Ok(Response::new(cfg.try_into().map_err(CarbideError::from)?))
}

pub(crate) async fn delete_token_delegation(
    api: &Api,
    request: Request<GetTokenDelegationRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::InvalidArgument(
            "machine identity must be enabled in site config".to_string(),
        )
        .into());
    }

    let req = request.into_inner();
    let org_id = req.organization_id.trim();
    if org_id.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_id
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;

    api.database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                let result = tenant_identity_config::delete_token_delegation(&org_id, txn).await?;
                if result.is_some() {
                    tenant::increment_version(org_id.as_str(), txn).await?;
                }
                Ok::<_, db::DatabaseError>(())
            })
        })
        .await??;

    Ok(Response::new(()))
}

enum ReencryptFieldResult {
    Absent,
    SkippedOnTarget,
    WouldReencrypt,
    Reencrypted(String),
    Failed(ReencryptTenantIdentityFailure),
}

#[derive(Clone, Copy)]
struct ReencryptField {
    name: &'static str,
    secret_kind: StoredMachineIdentitySecretKind,
}

fn tally_reencrypt_field(
    plan: &mut ReencryptOrgPlan,
    result: ReencryptFieldResult,
) -> Option<String> {
    match result {
        ReencryptFieldResult::Absent => None,
        ReencryptFieldResult::SkippedOnTarget => {
            plan.fields_skipped_on_target += 1;
            None
        }
        ReencryptFieldResult::WouldReencrypt => {
            plan.any_change = true;
            plan.fields_reencrypted += 1;
            None
        }
        ReencryptFieldResult::Reencrypted(new_ciphertext) => {
            plan.any_change = true;
            plan.fields_reencrypted += 1;
            Some(new_ciphertext)
        }
        ReencryptFieldResult::Failed(failure) => {
            plan.failures.push(failure);
            None
        }
    }
}

async fn reencrypt_one_field(
    credentials: &dyn CredentialReader,
    org_id: &str,
    field: ReencryptField,
    ciphertext: Option<&str>,
    target_key_id: &EncryptionKeyId,
    target_aes: &key_encryption::Aes256Key,
    dry_run: bool,
) -> Result<ReencryptFieldResult, Status> {
    let Some(ciphertext) = ciphertext.none_if_empty() else {
        return Ok(ReencryptFieldResult::Absent);
    };
    match reencrypt_ciphertext_if_needed(
        credentials,
        ciphertext,
        field.secret_kind,
        org_id,
        target_key_id,
        target_aes,
        dry_run,
    )
    .await
    {
        Ok(ReencryptBlobOutcome::SkippedOnTarget) => Ok(ReencryptFieldResult::SkippedOnTarget),
        Ok(ReencryptBlobOutcome::DryRunWouldReencrypt) => Ok(ReencryptFieldResult::WouldReencrypt),
        Ok(ReencryptBlobOutcome::Reencrypted(new_ciphertext)) => {
            Ok(ReencryptFieldResult::Reencrypted(new_ciphertext))
        }
        Err(status) => Ok(ReencryptFieldResult::Failed(
            ReencryptTenantIdentityFailure {
                organization_id: org_id.to_string(),
                field: field.name.to_string(),
                error: status.message().to_string(),
            },
        )),
    }
}

struct ReencryptOrgPlan {
    enc1: Option<EncryptedSigningPrivateKey>,
    enc2: Option<EncryptedSigningPrivateKey>,
    delegation: Option<EncryptedTokenDelegationAuthConfig>,
    fields_reencrypted: u32,
    fields_skipped_on_target: u32,
    any_change: bool,
    failures: Vec<ReencryptTenantIdentityFailure>,
}

fn store_reencrypted_signing_key(
    org_id_str: &str,
    field: &str,
    slot: &mut Option<EncryptedSigningPrivateKey>,
    failures: &mut Vec<ReencryptTenantIdentityFailure>,
    new_ciphertext: String,
) {
    match new_ciphertext.try_into() {
        Ok(parsed) => *slot = Some(parsed),
        Err(_) => failures.push(ReencryptTenantIdentityFailure {
            organization_id: org_id_str.to_string(),
            field: field.to_string(),
            error: "reencrypted ciphertext was empty".to_string(),
        }),
    }
}

fn store_reencrypted_delegation(
    org_id_str: &str,
    delegation: &mut Option<EncryptedTokenDelegationAuthConfig>,
    failures: &mut Vec<ReencryptTenantIdentityFailure>,
    new_ciphertext: String,
) {
    match new_ciphertext.try_into() {
        Ok(parsed) => *delegation = Some(parsed),
        Err(_) => failures.push(ReencryptTenantIdentityFailure {
            organization_id: org_id_str.to_string(),
            field: "encrypted_auth_method_config".to_string(),
            error: "reencrypted ciphertext was empty".to_string(),
        }),
    }
}

enum SigningKeySlot {
    Key1,
    Key2,
}

async fn reencrypt_signing_key_fields(
    credentials: &dyn CredentialReader,
    org_id_str: &str,
    plan: &mut ReencryptOrgPlan,
    target_key_id: &EncryptionKeyId,
    target_aes: &key_encryption::Aes256Key,
    dry_run: bool,
) -> Result<(), Status> {
    for slot in [SigningKeySlot::Key1, SigningKeySlot::Key2] {
        let (field, ciphertext) = match slot {
            SigningKeySlot::Key1 => (
                ReencryptField {
                    name: "encrypted_signing_key_1",
                    secret_kind: StoredMachineIdentitySecretKind::SigningKey,
                },
                plan.enc1.as_ref().map(|v| v.as_str()).map(str::to_string),
            ),
            SigningKeySlot::Key2 => (
                ReencryptField {
                    name: "encrypted_signing_key_2",
                    secret_kind: StoredMachineIdentitySecretKind::SigningKey,
                },
                plan.enc2.as_ref().map(|v| v.as_str()).map(str::to_string),
            ),
        };
        if let Some(new_ciphertext) = tally_reencrypt_field(
            plan,
            reencrypt_one_field(
                credentials,
                org_id_str,
                field,
                ciphertext.as_deref(),
                target_key_id,
                target_aes,
                dry_run,
            )
            .await?,
        ) {
            let enc_slot = match slot {
                SigningKeySlot::Key1 => &mut plan.enc1,
                SigningKeySlot::Key2 => &mut plan.enc2,
            };
            store_reencrypted_signing_key(
                org_id_str,
                field.name,
                enc_slot,
                &mut plan.failures,
                new_ciphertext,
            );
        }
    }
    Ok(())
}

async fn plan_org_reencrypt(
    credentials: &dyn CredentialReader,
    org_id: &TenantOrganizationId,
    row: &TenantIdentityConfig,
    target_key_id: &EncryptionKeyId,
    target_aes: &key_encryption::Aes256Key,
    dry_run: bool,
) -> Result<ReencryptOrgPlan, Status> {
    let org_id_str = org_id.as_str();
    let mut plan = ReencryptOrgPlan {
        enc1: row.encrypted_signing_key_1.clone(),
        enc2: row.encrypted_signing_key_2.clone(),
        delegation: row.encrypted_auth_method_config.clone(),
        fields_reencrypted: 0,
        fields_skipped_on_target: 0,
        any_change: false,
        failures: Vec::new(),
    };

    reencrypt_signing_key_fields(
        credentials,
        org_id_str,
        &mut plan,
        target_key_id,
        target_aes,
        dry_run,
    )
    .await?;

    let delegation_ciphertext = plan
        .delegation
        .as_ref()
        .map(|v| v.as_str())
        .map(str::to_string);
    if let Some(new_ciphertext) = tally_reencrypt_field(
        &mut plan,
        reencrypt_one_field(
            credentials,
            org_id_str,
            ReencryptField {
                name: "encrypted_auth_method_config",
                secret_kind: StoredMachineIdentitySecretKind::TokenDelegationAuth,
            },
            delegation_ciphertext.as_deref(),
            target_key_id,
            target_aes,
            dry_run,
        )
        .await?,
    ) {
        store_reencrypted_delegation(
            org_id_str,
            &mut plan.delegation,
            &mut plan.failures,
            new_ciphertext,
        );
    }

    Ok(plan)
}

/// Site-operator RPC: re-wrap encrypted tenant identity fields with site
/// `[machine_identity].current_encryption_key_id`.
pub(crate) async fn reencrypt_tenant_identity_secrets(
    api: &Api,
    request: Request<ReencryptTenantIdentitySecretsRequest>,
) -> Result<Response<ReencryptTenantIdentitySecretsResponse>, Status> {
    log_request_data(&request);
    require_machine_identity_site_enabled(api)?;

    let req = request.into_inner();
    let dry_run = req.dry_run;
    let bounds = IdentityConfigValidationBounds::from(api.runtime_config.machine_identity.clone());
    let target_key_id = bounds.encryption_key_id.clone();
    let target_aes =
        machine_identity_encryption_secret(&api.credential_manager, &target_key_id).await?;

    let org_filter: Option<TenantOrganizationId> = match req.organization_id {
        Some(ref id) if !id.trim().is_empty() => Some(
            id.trim()
                .parse()
                .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?,
        ),
        _ => None,
    };

    let org_ids = {
        let mut db = api.db_reader();
        tenant_identity_config::list_organization_ids_for_reencrypt(org_filter.as_ref(), &mut db)
            .await?
    };

    let mut response = ReencryptTenantIdentitySecretsResponse {
        rows_examined: u32::try_from(org_ids.len()).unwrap_or(u32::MAX),
        rows_updated: 0,
        rows_skipped_all_on_target: 0,
        fields_reencrypted: 0,
        fields_skipped_on_target: 0,
        rows_failed: 0,
        failures: Vec::new(),
        current_encryption_key_id: target_key_id.as_str().to_string(),
    };

    for org_id in org_ids {
        let mut db = api.db_reader();
        let Some(row) = tenant_identity_config::find(&org_id, &mut db).await? else {
            return Err(CarbideError::NotFoundError {
                kind: "tenant_identity_config",
                id: org_id.as_str().to_string(),
            }
            .into());
        };

        let plan = plan_org_reencrypt(
            api.credential_manager.as_ref(),
            &org_id,
            &row,
            &target_key_id,
            &target_aes,
            dry_run,
        )
        .await?;

        if !plan.failures.is_empty() {
            response.rows_failed += 1;
            response.failures.extend(plan.failures);
            continue;
        }

        if plan.any_change {
            if !dry_run {
                let enc1 = plan.enc1;
                let enc2 = plan.enc2;
                let delegation = plan.delegation;
                let write = api
                    .database_connection
                    .with_txn(|txn| {
                        Box::pin(async move {
                            let write = tenant_identity_config::update_encrypted_fields(
                                &row, enc1, enc2, delegation, txn,
                            )
                            .await?;
                            if let ConditionalWrite::Applied(()) = write {
                                tenant::increment_version(row.organization_id.as_str(), txn)
                                    .await?;
                            }
                            Ok::<_, db::DatabaseError>(write)
                        })
                    })
                    .await??;
                if let ConditionalWrite::NotApplied(reason) = write {
                    response.rows_failed += 1;
                    response.failures.push(ReencryptTenantIdentityFailure {
                        organization_id: org_id.as_str().to_string(),
                        field: "tenant_identity_config".to_string(),
                        error: reason.to_string(),
                    });
                    continue;
                }
            }
            response.rows_updated += 1;
        } else {
            response.rows_skipped_all_on_target += 1;
        }
        response.fields_reencrypted += plan.fields_reencrypted;
        response.fields_skipped_on_target += plan.fields_skipped_on_target;
    }

    Ok(Response::new(response))
}
