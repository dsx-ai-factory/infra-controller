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
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// JWT `alg` for per-tenant signing keys. Only ES256 (ECDSA P-256) is implemented end-to-end.
pub const TENANT_IDENTITY_SIGNING_JWT_ALG: &str = "ES256";

/// Per-tenant JWT signing algorithm persisted inside `signing_key_public_*` JSON (`alg`) and site config.
/// Only [`SigningAlgorithm::Es256`] is implemented end-to-end today; the enum leaves room for more JOSE `alg` values later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SigningAlgorithm {
    #[default]
    Es256,
}

impl SigningAlgorithm {
    #[must_use]
    pub const fn as_jwt_alg_str(self) -> &'static str {
        match self {
            Self::Es256 => TENANT_IDENTITY_SIGNING_JWT_ALG,
        }
    }
}

impl Display for SigningAlgorithm {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_jwt_alg_str())
    }
}

/// Unsupported or unknown `algorithm` string from config or the database.
#[derive(thiserror::Error, Debug)]
#[error(
    "unsupported tenant identity signing algorithm {0:?} (only {TENANT_IDENTITY_SIGNING_JWT_ALG} is implemented)"
)]
pub struct UnsupportedTenantSigningAlgorithm(pub String);

impl FromStr for SigningAlgorithm {
    type Err = UnsupportedTenantSigningAlgorithm;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            TENANT_IDENTITY_SIGNING_JWT_ALG => Ok(Self::Es256),
            other => Err(UnsupportedTenantSigningAlgorithm(other.to_string())),
        }
    }
}

impl sqlx::Type<sqlx::Postgres> for SigningAlgorithm {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("VARCHAR")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for SigningAlgorithm {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.to_string(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for SigningAlgorithm {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        s.parse()
            .map_err(|e: UnsupportedTenantSigningAlgorithm| sqlx::Error::Decode(Box::new(e)).into())
    }
}

// --- JWT issuer (`iss`) ---

/// Normalized JWT issuer URL or SPIFFE ID.
#[derive(Clone, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent, type_name = "VARCHAR")]
pub struct Issuer(String);

impl Issuer {
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Parse and normalize a raw issuer string. Returns the normalized issuer and the lowercase
    /// trust-domain token (registered host) used for SPIFFE and allowlist checks.
    pub fn parse(raw: &str) -> Result<(Self, String), InvalidIssuer> {
        let (normalized, trust_domain) =
            super::identity_config_policy::normalize_issuer_and_trust_domain(raw)
                .map_err(InvalidIssuer)?;
        Ok((Self(normalized), trust_domain))
    }

    /// Trust-domain token for this issuer (lowercase registered host), derived from the normalized
    /// `iss` string.
    pub fn trust_domain(&self) -> Result<String, InvalidIssuer> {
        super::identity_config_policy::normalize_issuer_and_trust_domain(self.as_str())
            .map(|(_, td)| td)
            .map_err(InvalidIssuer)
    }

    /// Whether this issuer's trust domain satisfies site [`super::identity_config_policy`] allowlist
    /// patterns (same semantics as [`super::identity_config_policy::trust_domain_matches_allowlist`]).
    /// Empty `allowlist` → `Ok`.
    pub fn trust_domain_matches_allowlist(&self, allowlist: &[String]) -> Result<(), String> {
        let td = self.trust_domain().map_err(|e| e.0)?;
        super::identity_config_policy::trust_domain_matches_allowlist(&td, allowlist)
    }

    /// Resolve optional proto `subject_prefix` against this issuer's trust domain (see
    /// [`super::identity_config_policy::resolve_subject_prefix`]).
    pub fn resolve_subject_prefix(&self, proto: Option<&str>) -> Result<String, String> {
        let td = self.trust_domain().map_err(|e| e.0)?;
        super::identity_config_policy::resolve_subject_prefix(&td, proto)
    }
}

impl AsRef<str> for Issuer {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for Issuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Issuer").field(&self.0).finish()
    }
}

impl Serialize for Issuer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Issuer string failed normalization or policy checks from [`super::identity_config_policy`].
#[derive(thiserror::Error, Debug)]
#[error("{0}")]
pub struct InvalidIssuer(pub String);

impl TryFrom<String> for Issuer {
    type Error = InvalidIssuer;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw).map(|(issuer, _)| issuer)
    }
}

impl FromStr for Issuer {
    type Err = InvalidIssuer;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).map(|(issuer, _)| issuer)
    }
}

// --- Non-empty string newtype (shared) and machine-identity ciphertext types ---

/// Owned UTF-8 string that is not empty and not only whitespace (`trim()` non-empty).
/// `S` distinguishes usage sites at compile time.
#[derive(PartialEq, Eq, Hash)]
pub struct NonEmptyStr<S> {
    inner: String,
    _tag: PhantomData<S>,
}

impl<S> Clone for NonEmptyStr<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _tag: PhantomData,
        }
    }
}

impl<S> fmt::Debug for NonEmptyStr<S> {
    /// Redacts contents (length only): some markers protect ciphertext; avoid logging raw strings.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NonEmptyStr")
            .field("len", &self.inner.len())
            .finish()
    }
}

impl<S> NonEmptyStr<S> {
    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }
}

impl<S> AsRef<str> for NonEmptyStr<S> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Empty string was provided where a [`NonEmptyStr`] is required.
#[derive(thiserror::Error, Debug)]
#[error("non-empty string required")]
pub struct InvalidNonEmptyStr;

impl<S> TryFrom<String> for NonEmptyStr<S> {
    type Error = InvalidNonEmptyStr;

    fn try_from(inner: String) -> Result<Self, Self::Error> {
        if inner.trim().is_empty() {
            return Err(InvalidNonEmptyStr);
        }
        Ok(Self {
            inner,
            _tag: PhantomData,
        })
    }
}

impl<S> FromStr for NonEmptyStr<S> {
    type Err = InvalidNonEmptyStr;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_string())
    }
}

impl<S> sqlx::Type<sqlx::Postgres> for NonEmptyStr<S> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<S> sqlx::Encode<'_, sqlx::Postgres> for NonEmptyStr<S> {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.inner, buf)
    }
}

impl<'r, S> sqlx::Decode<'r, sqlx::Postgres> for NonEmptyStr<S> {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(s).map_err(|e| sqlx::Error::Decode(Box::new(e)).into())
    }
}

/// Marker for [`NonEmptyStr`] used as `machine_identity.encryption_keys` id and envelope label.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct EncryptionKeyIdTag;

/// Selects the AES key under `machine_identity.encryption_keys` and labels encryption envelopes.
pub type EncryptionKeyId = NonEmptyStr<EncryptionKeyIdTag>;

/// Marker for JWT `kid` inside `signing_key_public_*` JSON (e.g. hex digest of public key material).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TenantIdentitySigningKeyIdTag;

/// Per-tenant signing key identifier (JWT `kid`), stored in `signing_key_public_*` JSON; must be non-empty.
pub type KeyId = NonEmptyStr<TenantIdentitySigningKeyIdTag>;

impl KeyId {
    /// JWT `kid` from `hex(sha256(utf8_bytes(public_key_material)))`.
    ///
    /// Delegates to [`Self::key_id_from_public_key`] (e.g. SPKI PEM from
    /// ES256 key generation). Infallible: that function always yields 64 hex characters.
    pub fn from_public_key_material(public_key_material: &str) -> Self {
        Self::try_from(Self::key_id_from_public_key(public_key_material))
            .expect("key_id_from_public_key yields 64 hex chars, always non-empty")
    }

    /// Computes key_id as hex(sha256(public_key)).
    /// Works with any public key representation (PEM, DER, etc.).
    ///
    /// API domain code should prefer `KeyId::from_public_key_material` in `carbide-api-model`, which
    /// delegates to this function (one implementation).
    fn key_id_from_public_key(public_key: &str) -> String {
        let hash = Sha256::digest(public_key.as_bytes());
        hex::encode(hash)
    }
}

/// Marker for `tenant_identity_config` PEM text embedded in signing public JSON (`public_pem`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TenantSigningPublicKeyPemTag;

/// ES256 public key in PEM form (stored in `signing_key_public_* .public_pem`).
pub type SigningPublicKeyPem = NonEmptyStr<TenantSigningPublicKeyPemTag>;

fn serialize_signing_algorithm_as_jwt_alg_str<S>(
    alg: &SigningAlgorithm,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(alg.as_jwt_alg_str())
}

/// Versioned signing public metadata JSON (`tenant_identity_config.signing_key_public_1|2`).
///
/// Fields are private; use [`Self::v`], [`Self::kid`], [`Self::alg`], [`Self::public_pem`].
/// `serde::Deserialize` trims `public_pem` before recomputing [`KeyId`], matching [`Self::es256_from_public_pem`].
/// JSON `alg` remains a JWT string (e.g. `"ES256"`), not an enum object — see [`serialize_signing_algorithm_as_jwt_alg_str`].
/// `kid` and `public_pem` are trimmed on build/deserialize so values match [`KeyId::from_public_key_material`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SigningKeyPublicV1 {
    v: u32,
    kid: String,
    #[serde(serialize_with = "serialize_signing_algorithm_as_jwt_alg_str")]
    alg: SigningAlgorithm,
    public_pem: String,
}

#[derive(Deserialize)]
struct SigningKeyPublicV1Wire {
    v: u32,
    kid: String,
    alg: String,
    public_pem: String,
}

impl<'de> Deserialize<'de> for SigningKeyPublicV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SigningKeyPublicV1Wire::deserialize(deserializer)?;
        let alg: SigningAlgorithm = wire.alg.parse().map_err(serde::de::Error::custom)?;
        Self::try_from_parts(wire.v, wire.kid, alg, wire.public_pem)
            .map_err(serde::de::Error::custom)
    }
}

impl SigningKeyPublicV1 {
    /// Only [`SigningAlgorithm::Es256`] is accepted for v1 documents, even if more variants are
    /// added to [`SigningAlgorithm`] later (e.g. for other config surfaces).
    fn try_from_parts(
        v: u32,
        kid: String,
        alg: SigningAlgorithm,
        public_pem: String,
    ) -> Result<Self, String> {
        let public_pem = public_pem.trim();
        if public_pem.is_empty() {
            return Err("signing public PEM is empty".to_string());
        }
        let public_pem = public_pem.to_string();
        if v != 1 {
            return Err(format!(
                "unsupported tenant signing public document version {v}"
            ));
        }
        let kid = kid.trim();
        if kid.is_empty() {
            return Err("signing public kid is empty".to_string());
        }
        let expected_kid = KeyId::from_public_key_material(&public_pem);
        if expected_kid.as_str() != kid {
            return Err("signing public kid does not match public_pem".to_string());
        }
        let kid = kid.to_string();
        if alg != SigningAlgorithm::Es256 {
            return Err("only ES256 tenant signing keys are supported".to_string());
        }
        Ok(Self {
            v,
            kid,
            alg,
            public_pem,
        })
    }

    #[must_use]
    pub const fn v(&self) -> u32 {
        self.v
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        self.kid.as_str()
    }

    #[must_use]
    pub const fn alg(&self) -> SigningAlgorithm {
        self.alg
    }

    #[must_use]
    pub fn public_pem(&self) -> &str {
        self.public_pem.as_str()
    }

    /// Builds version-1 JSON content for an ES256 SPKI PEM (canonical `kid` from [`KeyId::from_public_key_material`]).
    ///
    /// PEM is trimmed before hashing and storage so `kid` always matches persisted `public_pem`.
    /// Generated PEM often ends with a trailing newline.
    pub fn es256_from_public_pem(public_pem: &str) -> Result<Self, String> {
        let trimmed = public_pem.trim();
        if trimmed.is_empty() {
            return Err("signing public PEM is empty".to_string());
        }
        let kid = KeyId::from_public_key_material(trimmed);
        Self::try_from_parts(
            1,
            kid.as_str().to_string(),
            SigningAlgorithm::Es256,
            trimmed.to_string(),
        )
    }
}

/// Database enum `tenant_identity_current_signing_key_slot_t`: which slot signs new JWTs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "tenant_identity_current_signing_key_slot_t")]
pub enum TenantIdentityCurrentSigningKeySlot {
    #[sqlx(rename = "signing_key_1")]
    SigningKey1,
    #[sqlx(rename = "signing_key_2")]
    SigningKey2,
}

impl TenantIdentityCurrentSigningKeySlot {
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::SigningKey1 => Self::SigningKey2,
            Self::SigningKey2 => Self::SigningKey1,
        }
    }
}

/// Non-empty UTF-8 string holding a `key_encryption` JSON envelope (base64). `M` distinguishes
/// what plaintext the ciphertext wraps so distinct columns are not interchangeable.
pub type EnvelopeCiphertext<M> = NonEmptyStr<M>;

/// Marker for `tenant_identity_config.encrypted_signing_key_*` (encrypted ES256 private PEM).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TenantSigningPrivateKeyCiphertextTag;

/// Ciphertext for a stored signing private key slot (`encrypted_signing_key_1` / `_2`).
pub type EncryptedSigningPrivateKey = EnvelopeCiphertext<TenantSigningPrivateKeyCiphertextTag>;

/// Marker for token-delegation auth config ciphertext (`encrypted_auth_method_config`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TokenDelegationEncryptedAuthConfigTag;

/// Ciphertext for `tenant_identity_config.encrypted_auth_method_config` (delegation client secret JSON).
pub type EncryptedTokenDelegationAuthConfig =
    EnvelopeCiphertext<TokenDelegationEncryptedAuthConfigTag>;

#[cfg(test)]
mod key_id_tests {
    use p256::pkcs8::{DecodePrivateKey, DecodePublicKey};
    use serde_json::json;

    use super::{KeyId, SigningKeyPublicV1};

    #[test]
    fn key_id_from_public_key_material_is_deterministic_hex64() {
        let pem = "-----BEGIN PUBLIC KEY-----\nMFkw...\n-----END PUBLIC KEY-----";
        let a = KeyId::from_public_key_material(pem);
        let b = KeyId::from_public_key_material(pem);
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), 64);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn es256_public_doc_trims_trailing_newline() {
        let base = "-----BEGIN PUBLIC KEY-----\nMFkw...\n-----END PUBLIC KEY-----";
        let pem = format!("{base}\n");
        let doc = SigningKeyPublicV1::es256_from_public_pem(&pem).expect("build doc");
        assert_eq!(doc.public_pem(), base);
        assert_eq!(doc.kid(), KeyId::from_public_key_material(base).as_str());
    }

    #[test]
    fn signing_public_doc_trims_whitespace_around_kid() {
        let base = "-----BEGIN PUBLIC KEY-----\nMFkw...\n-----END PUBLIC KEY-----";
        let canonical = SigningKeyPublicV1::es256_from_public_pem(base).expect("build doc");
        let loose_kid = format!(" \t{} \n", canonical.kid());
        let v = json!({
            "v": 1,
            "kid": loose_kid,
            "alg": "ES256",
            "public_pem": canonical.public_pem(),
        });
        let doc: SigningKeyPublicV1 = serde_json::from_value(v).expect("deserialize");
        assert_eq!(doc.kid(), canonical.kid());
        assert_eq!(doc, canonical);
    }

    #[test]
    fn key_id_from_public_ke_yis_deterministic() {
        let pub_key = "-----BEGIN PUBLIC KEY-----\nMFkw...\n-----END PUBLIC KEY-----";
        let id1 = KeyId::key_id_from_public_key(pub_key);
        let id2 = KeyId::key_id_from_public_key(pub_key);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 64);
    }

    #[test]
    fn generate_es256_key_pair_produces_valid_outputs() {
        let (private_pem, public_pem) =
            carbide_secrets::key_encryption::generate_es256_key_pair().unwrap();
        assert!(private_pem.starts_with(b"-----BEGIN"));
        assert!(public_pem.contains("PUBLIC KEY"));
        let key_id = KeyId::key_id_from_public_key(&public_pem);
        assert_eq!(key_id.len(), 64);
        p256::PublicKey::from_public_key_pem(public_pem.trim()).unwrap();
        p256::SecretKey::from_pkcs8_pem(std::str::from_utf8(&private_pem).unwrap()).unwrap();
    }
}

// Real Postgres round-trips for the sqlx `Encode`/`Decode` codecs. Binding a
// value and reading it back exercises the `Decode`-from-`PgValueRef` path the
// in-memory tests can't reach; the per-test pool comes from the shared
// `sqlx_test` harness, so `carbide-test-support` itself stays db-agnostic.
#[cfg(test)]
mod sqlx_db_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};

    use super::*;
    use crate::tenant::TenantOrganizationId;

    #[crate::sqlx_test]
    async fn codecs_round_trip_through_postgres(pool: sqlx::PgPool) -> eyre::Result<()> {
        // SigningAlgorithm <-> VARCHAR
        check_cases_async(
            [Case {
                scenario: "SigningAlgorithm Es256 round-trips",
                input: SigningAlgorithm::Es256,
                expect: Yields(SigningAlgorithm::Es256),
            }],
            |alg: SigningAlgorithm| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, SigningAlgorithm>("SELECT $1::varchar")
                        .bind(alg)
                        .fetch_one(&pool)
                        .await
                        .map_err(drop)
                }
            },
        )
        .await;

        // TenantOrganizationId <-> TEXT
        check_cases_async(
            [
                Case {
                    scenario: "alphanumeric org id round-trips",
                    input: TenantOrganizationId::try_from("acme123".to_string()).unwrap(),
                    expect: Yields(TenantOrganizationId::try_from("acme123".to_string()).unwrap()),
                },
                Case {
                    scenario: "org id with dashes and underscores round-trips",
                    input: TenantOrganizationId::try_from("acme-corp_1".to_string()).unwrap(),
                    expect: Yields(
                        TenantOrganizationId::try_from("acme-corp_1".to_string()).unwrap(),
                    ),
                },
            ],
            |org: TenantOrganizationId| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, TenantOrganizationId>("SELECT $1::text")
                        .bind(org)
                        .fetch_one(&pool)
                        .await
                        .map_err(drop)
                }
            },
        )
        .await;

        // KeyId (`NonEmptyStr`, no `PartialEq`) <-> TEXT; compare the text, and
        // the decode still rejects an empty residue.
        check_cases_async(
            [
                Case {
                    scenario: "typical key id round-trips",
                    input: "signing-key-1",
                    expect: Yields("signing-key-1".to_string()),
                },
                Case {
                    scenario: "single-character key id round-trips",
                    input: "k",
                    expect: Yields("k".to_string()),
                },
            ],
            |raw: &str| {
                let pool = pool.clone();
                async move {
                    let key = KeyId::try_from(raw.to_string()).map_err(drop)?;
                    let back: KeyId = sqlx::query_scalar("SELECT $1::text")
                        .bind(key)
                        .fetch_one(&pool)
                        .await
                        .map_err(drop)?;
                    Ok::<_, ()>(back.as_str().to_string())
                }
            },
        )
        .await;
        Ok(())
    }
}
