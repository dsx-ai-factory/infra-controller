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

//! Tenant identity config for SPIFFE JWT-SVID machine identity.
//! Stores per-org identity config and signing keys in `tenant_identity_config` table.

use carbide_uuid::machine::{HostOrDpuId, MachineId};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use model::tenant::identity_config::SigningKeyPublicV1;
use model::tenant::{
    EncryptedSigningPrivateKey, EncryptedTokenDelegationAuthConfig, IdentityConfig,
    SigningKeyMaterial, TenantIdentityConfig, TenantIdentityCurrentSigningKeySlot,
    TenantOrganizationId, TokenDelegation, TokenDelegationAuthMethod,
};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Explicit column list for [`TenantIdentityConfig`] queries. Avoid `SELECT *` so schema migrations
/// (e.g. dropped columns) do not invalidate cached prepared statements on live connections.
const TENANT_IDENTITY_CONFIG_COLUMNS: &str = "\
    organization_id, issuer, default_audience, allowed_audiences, \
    token_ttl_sec, subject_prefix, enabled, created_at, updated_at, \
    encrypted_signing_key_1, encrypted_signing_key_2, \
    signing_key_public_1, signing_key_public_2, \
    current_signing_key_slot, non_active_slot_expires_at, \
    token_endpoint, auth_method, encrypted_auth_method_config, \
    subject_token_audience, token_delegation_created_at";

fn tenant_identity_config_returning() -> String {
    format!("RETURNING {TENANT_IDENTITY_CONFIG_COLUMNS}")
}

/// After `non_active_slot_expires_at`, clears the non-current slot (public + private ciphertext).
pub async fn gc_expired_non_active_signing_key(
    org_id: &TenantOrganizationId,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    gc_expired_non_active_signing_key_at(org_id, txn, Utc::now()).await
}

async fn gc_expired_non_active_signing_key_at(
    org_id: &TenantOrganizationId,
    txn: &mut PgConnection,
    now: DateTime<Utc>,
) -> DatabaseResult<()> {
    let Some(row) = find(org_id, txn).await? else {
        return Ok(());
    };
    let Some(expires) = row.non_active_slot_expires_at else {
        return Ok(());
    };
    if expires > now {
        return Ok(());
    }

    let slot_to_clear = row.current_signing_key_slot.other();
    let stmt = match slot_to_clear {
        TenantIdentityCurrentSigningKeySlot::SigningKey1 => {
            "UPDATE tenant_identity_config SET \
                signing_key_public_1 = NULL, \
                encrypted_signing_key_1 = NULL, \
                non_active_slot_expires_at = NULL, \
                updated_at = NOW() \
                WHERE organization_id = $1"
        }
        TenantIdentityCurrentSigningKeySlot::SigningKey2 => {
            "UPDATE tenant_identity_config SET \
                signing_key_public_2 = NULL, \
                encrypted_signing_key_2 = NULL, \
                non_active_slot_expires_at = NULL, \
                updated_at = NOW() \
                WHERE organization_id = $1"
        }
    };
    sqlx::query(stmt)
        .bind(org_id.as_str())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query("gc_expired_non_active_signing_key", e))?;
    Ok(())
}

fn signing_public_json_from_material(
    km: &SigningKeyMaterial,
) -> DatabaseResult<Json<SigningKeyPublicV1>> {
    let doc = SigningKeyPublicV1::es256_from_public_pem(km.signing_key_public.as_str())
        .map_err(DatabaseError::InvalidArgument)?;
    Ok(Json(doc))
}

/// Refuses rotation only when signing-slot metadata is inconsistent with
/// [`gc_expired_non_active_signing_key_at`] invariants.
fn ensure_rotation_allowed_for_existing(
    ex: &TenantIdentityConfig,
    now: DateTime<Utc>,
) -> DatabaseResult<()> {
    let both_public = ex.signing_key_public_1.is_some() && ex.signing_key_public_2.is_some();
    if both_public {
        let Some(expires_at) = ex.non_active_slot_expires_at else {
            return Err(DatabaseError::InvalidArgument(
                "cannot rotate signing key: both JWKS public key slots are populated but \
                 non_active_slot_expires_at is NULL (inconsistent tenant_identity_config state)"
                    .into(),
            ));
        };
        if expires_at <= now {
            return Err(DatabaseError::InvalidArgument(
                "cannot rotate signing key: overlap period has ended but both JWKS public key slots are \
                 still populated (inconsistent tenant_identity_config state after GC; retry the request \
                 or repair the row)"
                    .into(),
            ));
        }
        return Ok(());
    }
    if ex
        .non_active_slot_expires_at
        .is_some_and(|expires_at| expires_at > now)
    {
        return Err(DatabaseError::InvalidArgument(
            "cannot rotate signing key: non_active_slot_expires_at is in the future but only one \
             JWKS public key slot is populated (inconsistent tenant_identity_config state)"
                .into(),
        ));
    }
    Ok(())
}

/// Signing-key slot ciphertext / JWKS JSON plus overlap deadline for [`set`]'s upsert row.
struct KeyRows {
    enc1: Option<EncryptedSigningPrivateKey>,
    enc2: Option<EncryptedSigningPrivateKey>,
    pub1: Option<Json<SigningKeyPublicV1>>,
    pub2: Option<Json<SigningKeyPublicV1>>,
    current_slot: TenantIdentityCurrentSigningKeySlot,
    non_active_expires_at: Option<DateTime<Utc>>,
}

/// Set identity config for an org.
/// When creating new or rotating key, caller must provide `key_material` (generated key pair, encrypted private key).
/// Caller must ensure tenant exists and global machine-identity is enabled.
/// On key rotation, `config.signing_key_overlap_sec` must be set (seconds until the previous JWKS key is dropped);
/// see [`IdentityConfig::try_from_proto`].
pub async fn set(
    org_id: &TenantOrganizationId,
    config: &IdentityConfig,
    key_material: Option<SigningKeyMaterial>,
    txn: &mut PgConnection,
) -> DatabaseResult<TenantIdentityConfig> {
    let now = Utc::now();
    gc_expired_non_active_signing_key_at(org_id, txn, now).await?;

    let allowed: Vec<String> = if config.allowed_audiences.is_empty() {
        vec![config.default_audience.clone()]
    } else {
        config.allowed_audiences.clone()
    };

    let token_ttl_i32: i32 = config
        .token_ttl_sec
        .try_into()
        .map_err(|_| DatabaseError::InvalidArgument("token_ttl out of range".into()))?;

    let existing = find(org_id, &mut *txn).await?;

    let key_rows = match (&existing, config.rotate_key, key_material) {
        (None, _, None) | (_, true, None) => {
            return Err(DatabaseError::InvalidArgument(
                "key_material is required when creating or rotating signing key".into(),
            ));
        }
        (Some(ex), true, Some(km)) => {
            ensure_rotation_allowed_for_existing(ex, now)?;
            let pub_doc = signing_public_json_from_material(&km)?;
            let new_enc = km.encrypted_signing_key;
            let other = ex.current_signing_key_slot.other();
            let overlap_sec = config.signing_key_overlap_sec.ok_or_else(|| {
                DatabaseError::InvalidArgument(
                    "signing_key_overlap_sec is required when rotating the signing key".into(),
                )
            })?;
            let overlap_for_expiry = u32::try_from(overlap_sec).map_err(|_| {
                DatabaseError::InvalidArgument(
                    "signing_key_overlap_sec must be non-negative and fit in u32".into(),
                )
            })?;
            let expires = Some(Utc::now() + ChronoDuration::seconds(i64::from(overlap_for_expiry)));
            match other {
                TenantIdentityCurrentSigningKeySlot::SigningKey1 => KeyRows {
                    enc1: Some(new_enc),
                    enc2: ex.encrypted_signing_key_2.clone(),
                    pub1: Some(pub_doc),
                    pub2: ex.signing_key_public_2.clone(),
                    current_slot: TenantIdentityCurrentSigningKeySlot::SigningKey1,
                    non_active_expires_at: expires,
                },
                TenantIdentityCurrentSigningKeySlot::SigningKey2 => KeyRows {
                    enc1: ex.encrypted_signing_key_1.clone(),
                    enc2: Some(new_enc),
                    pub1: ex.signing_key_public_1.clone(),
                    pub2: Some(pub_doc),
                    current_slot: TenantIdentityCurrentSigningKeySlot::SigningKey2,
                    non_active_expires_at: expires,
                },
            }
        }
        (None, _, Some(km)) => {
            let pub_doc = signing_public_json_from_material(&km)?;
            let new_enc = km.encrypted_signing_key;
            KeyRows {
                enc1: Some(new_enc),
                enc2: None,
                pub1: Some(pub_doc),
                pub2: None,
                current_slot: TenantIdentityCurrentSigningKeySlot::SigningKey1,
                non_active_expires_at: None,
            }
        }
        (Some(ex), false, None) => KeyRows {
            enc1: ex.encrypted_signing_key_1.clone(),
            enc2: ex.encrypted_signing_key_2.clone(),
            pub1: ex.signing_key_public_1.clone(),
            pub2: ex.signing_key_public_2.clone(),
            current_slot: ex.current_signing_key_slot,
            non_active_expires_at: ex.non_active_slot_expires_at,
        },
        (Some(_), false, Some(_)) => {
            return Err(DatabaseError::InvalidArgument(
                "key_material must not be set when rotate_key is false".into(),
            ));
        }
    };

    let query = format!(
        r#"
        INSERT INTO tenant_identity_config (
            organization_id, issuer, default_audience, allowed_audiences,
            token_ttl_sec, subject_prefix, enabled, created_at, updated_at,
            encrypted_signing_key_1, encrypted_signing_key_2,
            signing_key_public_1, signing_key_public_2,
            current_signing_key_slot, non_active_slot_expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW(), $8, $9, $10, $11, $12, $13)
        ON CONFLICT (organization_id) DO UPDATE SET
            issuer = EXCLUDED.issuer,
            default_audience = EXCLUDED.default_audience,
            allowed_audiences = EXCLUDED.allowed_audiences,
            token_ttl_sec = EXCLUDED.token_ttl_sec,
            subject_prefix = EXCLUDED.subject_prefix,
            enabled = EXCLUDED.enabled,
            updated_at = NOW(),
            encrypted_signing_key_1 = EXCLUDED.encrypted_signing_key_1,
            encrypted_signing_key_2 = EXCLUDED.encrypted_signing_key_2,
            signing_key_public_1 = EXCLUDED.signing_key_public_1,
            signing_key_public_2 = EXCLUDED.signing_key_public_2,
            current_signing_key_slot = EXCLUDED.current_signing_key_slot,
            non_active_slot_expires_at = EXCLUDED.non_active_slot_expires_at
        {}"#,
        tenant_identity_config_returning(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(org_id.as_str())
        .bind(&config.issuer)
        .bind(&config.default_audience)
        .bind(Json(allowed))
        .bind(token_ttl_i32)
        .bind(&config.subject_prefix)
        .bind(config.enabled)
        .bind(key_rows.enc1)
        .bind(key_rows.enc2)
        .bind(key_rows.pub1)
        .bind(key_rows.pub2)
        .bind(key_rows.current_slot)
        .bind(key_rows.non_active_expires_at)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query("UPSERT tenant_identity_config", e))
}

pub async fn find<DB>(
    org_id: &TenantOrganizationId,
    db: &mut DB,
) -> DatabaseResult<Option<TenantIdentityConfig>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let query = format!(
        "SELECT {TENANT_IDENTITY_CONFIG_COLUMNS} FROM tenant_identity_config WHERE organization_id = $1"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(org_id.as_str())
        .fetch_optional(&mut *db)
        .await
        .map_err(|e| DatabaseError::query("SELECT tenant_identity_config BY organization_id", e))
}

/// Machine ID used to locate the active instance when resolving tenant identity config.
///
/// Instances are host-scoped (`instances.machine_id` is the host). DPU agents call
/// `SignMachineIdentity` with a DPU mTLS identity, so map DPU callers to their owning host first.
async fn instance_machine_id_for_identity_lookup(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<MachineId> {
    let HostOrDpuId::Dpu(dpu_machine_id) = machine_id.host_or_dpu_id() else {
        return Ok(*machine_id);
    };
    let Some(host) = crate::machine::find_host_by_dpu_machine_id(txn, &dpu_machine_id).await?
    else {
        return Ok(*machine_id);
    };
    Ok(host.id)
}

/// Resolve tenant identity config for machine-identity RPCs: one join query, then overlap GC, then
/// reload by org PK so the row matches the post-GC database state (GC may clear a JWKS slot).
pub async fn find_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<TenantIdentityConfig> {
    let instance_machine_id = instance_machine_id_for_identity_lookup(txn, machine_id).await?;
    let row = sqlx::query_as::<_, TenantIdentityConfig>(
        r"
        SELECT tic.organization_id, tic.issuer, tic.default_audience, tic.allowed_audiences,
            tic.token_ttl_sec, tic.subject_prefix, tic.enabled, tic.created_at, tic.updated_at,
            tic.encrypted_signing_key_1, tic.encrypted_signing_key_2,
            tic.signing_key_public_1, tic.signing_key_public_2,
            tic.current_signing_key_slot, tic.non_active_slot_expires_at,
            tic.token_endpoint, tic.auth_method, tic.encrypted_auth_method_config,
            tic.subject_token_audience, tic.token_delegation_created_at
        FROM tenant_identity_config tic
        INNER JOIN instances i ON tic.organization_id = i.tenant_org
        WHERE i.machine_id = $1 AND i.deleted IS NULL AND tic.enabled = true",
    )
    .bind(instance_machine_id)
    .fetch_optional(&mut *txn)
    .await
    .map_err(|e| DatabaseError::query("SELECT tenant_identity_config BY machine_id", e))?;
    let Some(cfg) = row else {
        return Err(DatabaseError::NotFoundError {
            kind: "machine_identity",
            id: machine_id.to_string(),
        });
    };
    let org_id = cfg.organization_id.clone();
    gc_expired_non_active_signing_key(&org_id, txn).await?;
    find(&org_id, txn)
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine_identity",
            id: machine_id.to_string(),
        })
}

/// Set token delegation for an org. Identity config must exist first.
/// `encrypted_auth_method_config` must be standard base64 of JSON envelope v1 from `key_encryption::encrypt`
/// over the UTF-8 JSON produced by [`TokenDelegation::to_db_format`].
pub async fn set_token_delegation(
    org_id: &TenantOrganizationId,
    config: &TokenDelegation,
    auth_method: TokenDelegationAuthMethod,
    encrypted_auth_method_config: &EncryptedTokenDelegationAuthConfig,
    txn: &mut PgConnection,
) -> DatabaseResult<TenantIdentityConfig> {
    let query = format!(
        r#"
        UPDATE tenant_identity_config
        SET token_endpoint = $2, auth_method = $3, encrypted_auth_method_config = $4,
            subject_token_audience = $5, updated_at = NOW(),
            token_delegation_created_at = COALESCE(token_delegation_created_at, NOW())
        WHERE organization_id = $1
        {}"#,
        tenant_identity_config_returning(),
    );
    let row = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(org_id.as_str())
        .bind(&config.token_endpoint)
        .bind(auth_method)
        .bind(encrypted_auth_method_config.as_str())
        .bind(Some(config.subject_token_audience.as_str()))
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query("UPDATE tenant_identity_config token_delegation", e))?;
    row.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "tenant_identity_config",
        id: org_id.as_str().to_string(),
    })
}

/// Delete identity config for an org (removes the entire row).
pub async fn delete(org_id: &TenantOrganizationId, txn: &mut PgConnection) -> DatabaseResult<bool> {
    let result = sqlx::query("DELETE FROM tenant_identity_config WHERE organization_id = $1")
        .bind(org_id.as_str())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query("DELETE tenant_identity_config", e))?;
    Ok(result.rows_affected() > 0)
}

/// Clear token delegation for an org.
pub async fn delete_token_delegation(
    org_id: &TenantOrganizationId,
    txn: &mut PgConnection,
) -> DatabaseResult<Option<TenantIdentityConfig>> {
    let query = format!(
        r#"
        UPDATE tenant_identity_config
        SET token_endpoint = NULL, auth_method = NULL, encrypted_auth_method_config = NULL,
            subject_token_audience = NULL, token_delegation_created_at = NULL, updated_at = NOW()
        WHERE organization_id = $1
        {}"#,
        tenant_identity_config_returning(),
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(org_id.as_str())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query("CLEAR tenant_identity_config token_delegation", e))
}

/// Organization ids to re-wrap: one org (must exist) or all rows ordered by id.
pub async fn list_organization_ids_for_reencrypt<DB>(
    org_filter: Option<&TenantOrganizationId>,
    db: &mut DB,
) -> DatabaseResult<Vec<TenantOrganizationId>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    if let Some(org_id) = org_filter {
        if find(org_id, &mut *db).await?.is_none() {
            return Err(DatabaseError::NotFoundError {
                kind: "tenant_identity_config",
                id: org_id.as_str().to_string(),
            });
        }
        return Ok(vec![org_id.clone()]);
    }
    sqlx::query_scalar::<_, TenantOrganizationId>(
        "SELECT organization_id FROM tenant_identity_config ORDER BY organization_id",
    )
    .fetch_all(&mut *db)
    .await
    .map_err(|e| DatabaseError::query("LIST tenant_identity_config organization_ids", e))
}

/// Loads a row for update during master-key re-wrap.
pub async fn find_for_update(
    org_id: &TenantOrganizationId,
    txn: &mut PgConnection,
) -> DatabaseResult<Option<TenantIdentityConfig>> {
    let query = format!(
        "SELECT {TENANT_IDENTITY_CONFIG_COLUMNS} FROM tenant_identity_config WHERE organization_id = $1 FOR UPDATE"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(org_id.as_str())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query("SELECT tenant_identity_config FOR UPDATE", e))
}

/// Updates encrypted signing-key and token-delegation ciphertext columns only.
pub async fn update_encrypted_fields(
    org_id: &TenantOrganizationId,
    enc1: Option<EncryptedSigningPrivateKey>,
    enc2: Option<EncryptedSigningPrivateKey>,
    delegation: Option<EncryptedTokenDelegationAuthConfig>,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    sqlx::query(
        r#"
        UPDATE tenant_identity_config
        SET encrypted_signing_key_1 = $2,
            encrypted_signing_key_2 = $3,
            encrypted_auth_method_config = $4,
            updated_at = NOW()
        WHERE organization_id = $1
        "#,
    )
    .bind(org_id.as_str())
    .bind(enc1)
    .bind(enc2)
    .bind(delegation)
    .execute(txn)
    .await
    .map_err(|e| DatabaseError::query("UPDATE tenant_identity_config encrypted fields", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_secrets::key_encryption;
    use chrono::Utc;
    use model::metadata::Metadata;
    use model::tenant::identity_config::SigningAlgorithm;
    use model::tenant::{
        IdentityConfig, KeyId, TokenDelegation, TokenDelegationAuthMethod,
        TokenDelegationAuthMethodConfig,
    };

    use super::*;
    use crate::{DatabaseError, tenant};

    /// Synthetic P-256 SPKI public PEMs for tests (not secret). Prefer real PEM so validation stays
    /// aligned with `SigningKeyPublicV1` / JWKS expectations.
    const TEST_ES256_PUB_0: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAElFrs3yp8XhslMB6ZG6BG3Hvas7kR
tvTLdqh3uulBnXIXQBabKdLH8wuNfgO3xQrcRrm+Z0yucj/zCyGoJ8Iizw==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_1: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9tKNx7BW5TgNSJY31g4vT48RNl50
qbRrVHjvFz02cAUcng9QGX/8L/DVb51jVFq/F1tOjXEyhDON7R9plMicHQ==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_2: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9/p2rFtxVIP09iS6CPHdcxRXBiqn
+tk2afmDFptBAWTP09T6M1MTiYWbdKzuOal+rEzv8y1VdqQDJ1egup0A2w==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_3: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEMRBa5hEaWbdQn25IDNJLSfFXjXuT
1UHxIJ/3NgMa/v7PLcgmfz2WW9wfTivfbhD5g2ndLCpQLXrgtirqbhvFWQ==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_4: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9BbItCsp6q5/UdMxyCUy2VkDT5Zk
wLQJxr5OM1I00HA5WIuuYplqWIPWP5Lz3s6Qlh8Op4T51wMktUTYuO9lRA==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_5: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAELpyN493ciT/h189YhNpnndwua7Qo
CNBIXgZ2VUqZWsz+pNejmtX2i/sRs5mdGBmWxz5cEEXNQCAS9bSsVV9f3Q==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_6: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEVxhVG7gUkOkuGsTRlewt9srrggiK
SPemXUDmBH8uKkQlKQwzJRPfzpKNi+pcEJRLQI5IWP+ktuWwc/ZZrkEXAQ==
-----END PUBLIC KEY-----"#;
    const TEST_ES256_PUB_7: &str = r#"-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEKE0d2hbZdPmTpVKkmfMNi6ZmtnCa
ecbC7Qcisdw2/9l8bk/zfF9gvu4kh3hXZzMWgk+vj1e8KSX+NYswYiacQA==
-----END PUBLIC KEY-----"#;

    fn test_org_id() -> TenantOrganizationId {
        "IdentityConfigTestOrg".parse().unwrap()
    }

    async fn ensure_tenant(txn: &mut PgConnection, org_id: &TenantOrganizationId) {
        if tenant::find(org_id.as_str(), false, txn)
            .await
            .unwrap()
            .is_none()
        {
            tenant::create_and_persist(
                org_id.as_str().to_string(),
                Metadata {
                    name: "Test Org".to_string(),
                    description: "".to_string(),
                    labels: HashMap::new(),
                },
                None,
                txn,
            )
            .await
            .unwrap();
        }
    }

    fn placeholder_key_material() -> SigningKeyMaterial {
        test_signing_key_material_from_es256_public_pem(TEST_ES256_PUB_0)
    }

    fn test_signing_key_material_from_es256_public_pem(pem: &'static str) -> SigningKeyMaterial {
        SigningKeyMaterial {
            key_id: KeyId::from_public_key_material(pem),
            encrypted_signing_key: "PLACEHOLDER_ENCRYPTED_KEY".parse().unwrap(),
            signing_key_public: pem.parse().unwrap(),
        }
    }

    fn base_identity_config(rotate_key: bool, overlap: Option<i32>) -> IdentityConfig {
        IdentityConfig {
            issuer: "https://issuer.example.com".parse().unwrap(),
            default_audience: "api".to_string(),
            allowed_audiences: vec!["api".to_string(), "other".to_string()],
            token_ttl_sec: 3600,
            subject_prefix: "spiffe://issuer.example.com/org-x".to_string(),
            enabled: true,
            rotate_key,
            algorithm: SigningAlgorithm::Es256,
            encryption_key_id: "test-master".parse().unwrap(),
            signing_key_overlap_sec: overlap,
        }
    }

    #[crate::sqlx_test]
    async fn test_tenant_identity_config_set_find_delete(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let org_id = test_org_id();
        ensure_tenant(&mut txn, &org_id).await;

        let config = IdentityConfig {
            issuer: "https://issuer.example.com".parse().unwrap(),
            default_audience: "api".to_string(),
            allowed_audiences: vec!["api".to_string(), "audience2".to_string()],
            token_ttl_sec: 3600,
            subject_prefix: "spiffe://issuer.example.com/org-x".to_string(),
            enabled: true,
            rotate_key: false,
            algorithm: SigningAlgorithm::Es256,
            encryption_key_id: "test-master".parse().unwrap(),
            signing_key_overlap_sec: None,
        };

        let key_material = placeholder_key_material();
        let cfg = set(&org_id, &config, Some(key_material), &mut txn)
            .await
            .unwrap();
        assert_eq!(cfg.issuer.as_str(), "https://issuer.example.com");
        assert_eq!(cfg.default_audience, "api");
        assert_eq!(cfg.allowed_audiences.0, ["api", "audience2"]);
        assert_eq!(cfg.token_ttl_sec, 3600);
        assert_eq!(cfg.subject_prefix, "spiffe://issuer.example.com/org-x");
        assert!(cfg.enabled);
        assert_eq!(
            cfg.current_signing_key_slot,
            TenantIdentityCurrentSigningKeySlot::SigningKey1
        );
        assert!(cfg.signing_key_public_1.is_some());

        let found = find(&org_id, txn.as_mut()).await.unwrap().unwrap();
        assert_eq!(found.issuer, cfg.issuer);
        assert_eq!(found.default_audience, cfg.default_audience);
        assert_eq!(found.allowed_audiences.0, cfg.allowed_audiences.0);
        assert_eq!(found.token_ttl_sec, cfg.token_ttl_sec);
        assert_eq!(found.subject_prefix, cfg.subject_prefix);
        assert_eq!(found.enabled, cfg.enabled);
        assert_eq!(found.current_signing_key_slot, cfg.current_signing_key_slot);

        let deleted = delete(&org_id, &mut txn).await.unwrap();
        assert!(deleted);

        let not_found = find(&org_id, txn.as_mut()).await.unwrap();
        assert!(not_found.is_none());
    }

    #[crate::sqlx_test]
    async fn test_token_delegation_set_get_delete(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let org_id = test_org_id();
        ensure_tenant(&mut txn, &org_id).await;

        let config = IdentityConfig {
            issuer: "https://issuer.example.com".parse().unwrap(),
            default_audience: "api".to_string(),
            allowed_audiences: vec!["api".to_string()],
            token_ttl_sec: 3600,
            subject_prefix: "spiffe://issuer.example.com".to_string(),
            enabled: true,
            rotate_key: false,
            algorithm: SigningAlgorithm::Es256,
            encryption_key_id: "test-master".parse().unwrap(),
            signing_key_overlap_sec: None,
        };
        let key_material = placeholder_key_material();
        set(&org_id, &config, Some(key_material), &mut txn)
            .await
            .unwrap();

        let token_delegation = TokenDelegation {
            token_endpoint: "https://auth.example.com/token".to_string(),
            subject_token_audience: "https://api.example.com".to_string(),
            auth_method_config: TokenDelegationAuthMethodConfig::ClientSecretBasic {
                client_id: "test-client".to_string(),
                client_secret: "test-secret".to_string(),
            },
        };
        let (auth_method, plaintext_json) = token_delegation.to_db_format();
        let enc_key: key_encryption::Aes256Key = [0u8; 32];
        let enc =
            key_encryption::encrypt(plaintext_json.as_bytes(), &enc_key, "test-master").unwrap();
        let enc: EncryptedTokenDelegationAuthConfig = enc.try_into().unwrap();
        let cfg = set_token_delegation(&org_id, &token_delegation, auth_method, &enc, &mut txn)
            .await
            .unwrap();
        assert_eq!(
            cfg.token_endpoint.as_deref(),
            Some("https://auth.example.com/token")
        );
        assert_eq!(
            cfg.auth_method,
            Some(TokenDelegationAuthMethod::ClientSecretBasic)
        );
        assert_eq!(
            cfg.subject_token_audience.as_deref(),
            Some("https://api.example.com")
        );

        let cleared = delete_token_delegation(&org_id, &mut txn)
            .await
            .unwrap()
            .unwrap();
        assert!(cleared.token_endpoint.is_none());
        assert!(cleared.auth_method.is_none());
    }

    #[crate::sqlx_test]
    async fn test_tenant_identity_second_rotation_ok_while_overlap_active(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let org_id = test_org_id();
        ensure_tenant(&mut txn, &org_id).await;

        let initial = base_identity_config(false, None);
        set(
            &org_id,
            &initial,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_1,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        let once = base_identity_config(true, Some(86_400));
        set(
            &org_id,
            &once,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_2,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        let twice = base_identity_config(true, Some(3600));
        let cfg = set(
            &org_id,
            &twice,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_3,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        assert!(cfg.signing_key_public_1.is_some());
        assert!(cfg.signing_key_public_2.is_some());
        assert!(
            cfg.non_active_slot_expires_at
                .is_some_and(|ex| ex > Utc::now()),
            "new overlap deadline should be in the future"
        );
    }

    #[crate::sqlx_test]
    async fn test_tenant_identity_rotate_rejects_dual_public_without_overlap_deadline(
        pool: sqlx::PgPool,
    ) {
        let mut txn = pool.begin().await.unwrap();
        let org_id = test_org_id();
        ensure_tenant(&mut txn, &org_id).await;

        let initial = base_identity_config(false, None);
        set(
            &org_id,
            &initial,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_4,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        let rotated = base_identity_config(true, Some(3600));
        set(
            &org_id,
            &rotated,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_5,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        sqlx::query(
            r#"UPDATE tenant_identity_config
               SET non_active_slot_expires_at = NULL
               WHERE organization_id = $1"#,
        )
        .bind(org_id.as_str())
        .execute(&mut *txn)
        .await
        .unwrap();

        let again = base_identity_config(true, Some(3600));
        let err = set(
            &org_id,
            &again,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_6,
            )),
            &mut txn,
        )
        .await
        .unwrap_err();
        let DatabaseError::InvalidArgument(msg) = err else {
            panic!("expected InvalidArgument, got {err:?}");
        };
        assert!(msg.contains("non_active_slot_expires_at is NULL"), "{msg}");
    }

    #[crate::sqlx_test]
    async fn test_tenant_identity_rotate_rejects_future_overlap_with_single_public(
        pool: sqlx::PgPool,
    ) {
        let mut txn = pool.begin().await.unwrap();
        let org_id = test_org_id();
        ensure_tenant(&mut txn, &org_id).await;

        let initial = base_identity_config(false, None);
        set(
            &org_id,
            &initial,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_7,
            )),
            &mut txn,
        )
        .await
        .unwrap();

        sqlx::query(
            r#"UPDATE tenant_identity_config
               SET non_active_slot_expires_at = NOW() + INTERVAL '3600 seconds'
               WHERE organization_id = $1"#,
        )
        .bind(org_id.as_str())
        .execute(&mut *txn)
        .await
        .unwrap();

        let rotated = base_identity_config(true, Some(3600));
        let err = set(
            &org_id,
            &rotated,
            Some(test_signing_key_material_from_es256_public_pem(
                TEST_ES256_PUB_0,
            )),
            &mut txn,
        )
        .await
        .unwrap_err();
        let DatabaseError::InvalidArgument(msg) = err else {
            panic!("expected InvalidArgument, got {err:?}");
        };
        assert!(msg.contains("only one JWKS public key slot"), "{msg}");
    }
}
