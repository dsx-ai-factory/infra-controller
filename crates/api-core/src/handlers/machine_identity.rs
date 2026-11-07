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

//! gRPC handlers for machine identity: JWT-SVID signing, JWKS, and OpenID discovery.
//! PEM/JWK encoding helpers live in `crate::machine_identity`; persisted config in `tenant_identity_config`.
//! Public signing metadata uses `signing_key_public_*` JSON (`kid`, `alg`, `public_pem`).

use ::rpc::forge::{
    self as rpc, Jwks, JwksKind, JwksRequest, MachineIdentityResponse, OpenIdConfigRequest,
    OpenIdConfiguration,
};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::MachineId;
use chrono::Utc;
use db::{WithTransaction, tenant_identity_config};
use model::tenant::{InvalidTenantOrg, TenantIdentityConfig, TenantOrganizationId};
use serde_json::json;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::auth::AuthContext;
use crate::machine_identity::{
    Es256Signer, MachineIdentityStoredSecretDecryptionFailed, SignOptions, Signer,
    decrypt_machine_identity_ciphertext, decrypt_token_delegation_encrypted_blob,
    token_delegation_credentials, token_exchange_http_client, token_exchange_request,
};

/// Shared gate for APIs that require site `[machine_identity].enabled` (identity admin + discovery).
pub(super) fn require_machine_identity_site_enabled(api: &Api) -> Result<(), Status> {
    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::InvalidArgument(
            "machine identity must be enabled in site config".to_string(),
        )
        .into());
    }
    Ok(())
}

fn jwks_uri_for_issuer(issuer: &str) -> String {
    let base = issuer.trim_end_matches('/');
    format!("{base}/.well-known/jwks.json")
}

fn spiffe_jwks_uri_for_issuer(issuer: &str) -> String {
    let base = issuer.trim_end_matches('/');
    format!("{base}/.well-known/spiffe/jwks.json")
}

async fn load_enabled_identity_for_well_known(
    api: &Api,
    org_id: &TenantOrganizationId,
) -> Result<TenantIdentityConfig, Status> {
    let org_id_str = org_id.as_str().to_string();
    let cfg = api
        .database_connection
        .with_txn(|txn| {
            let org_id = org_id.clone();
            Box::pin(async move {
                tenant_identity_config::gc_expired_non_active_signing_key(&org_id, txn).await?;
                tenant_identity_config::find(&org_id, txn.as_mut()).await
            })
        })
        .await??;
    match cfg {
        Some(c) if c.enabled => Ok(c),
        _ => Err(CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id_str,
        }
        .into()),
    }
}

fn push_jwk_from_signing_public_doc(
    keys: &mut Vec<serde_json::Value>,
    doc: &model::tenant::identity_config::SigningKeyPublicV1,
    jwk_key_use: crate::machine_identity::JwkPublicKeyUse,
) -> Result<(), CarbideError> {
    keys.push(
        crate::machine_identity::public_pem_to_jwk_value(
            doc.public_pem(),
            doc.kid(),
            doc.alg().as_jwt_alg_str(),
            jwk_key_use,
        )
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?,
    );
    Ok(())
}

/// SPIFFE `sub` claim: stored prefix plus `/<machine-id>` (single slash join).
fn jwt_sub_claim(subject_prefix: &str, machine_id: &MachineId) -> String {
    let base = subject_prefix.trim_end_matches('/');
    format!("{base}/{machine_id}")
}

fn validate_audiences_in_allowlist(
    audiences: &[String],
    allowlist: &[String],
) -> Result<(), Status> {
    for a in audiences {
        if !allowlist.iter().any(|x| x == a) {
            return Err(CarbideError::InvalidArgument(format!(
                "audience {a:?} is not in allowed_audiences for this organization"
            ))
            .into());
        }
    }
    Ok(())
}

/// Handles the SignMachineIdentity gRPC call: validates the request, extracts
/// machine identity from the client certificate, and returns a JWT(-SVID)–shaped OAuth token
/// response.
///
/// The machine ID is taken from the client's mTLS certificate SPIFFE ID. The tenant organization
/// is resolved from the instance row for that machine; per-org identity config supplies issuer,
/// subject prefix, audiences, TTL, and signing key material.
///
/// When per-org **token delegation** is configured (`token_endpoint` + `subject_token_audience` +
/// `auth_method`), Carbide first signs a subject JWT (`aud` = exchange service,
/// `request_meta_data.aud` = caller-requested workload audiences) with the same `exp` / `iat` delta
/// as `token_ttl_sec`, then performs an RFC 8693 token exchange `POST` to the tenant
/// `token_endpoint` and returns that response (**`expires_in_sec` is taken from the tenant STS JSON
/// `expires_in` field, not from `token_ttl_sec`**). Otherwise the handler returns a directly signed
/// JWT using the org `token_ttl_sec` as `expires_in_sec`.
pub(crate) async fn sign_machine_identity(
    api: &Api,
    request: Request<rpc::MachineIdentityRequest>,
) -> Result<Response<MachineIdentityResponse>, Status> {
    log_request_data(&request);

    if !api.runtime_config.machine_identity.enabled {
        return Err(CarbideError::UnavailableError(
            "machine identity is disabled in site config".into(),
        )
        .into());
    }

    let auth_context = request
        .extensions()
        .get::<AuthContext>()
        .ok_or_else(|| Status::unauthenticated("no authentication context found"))?;

    let machine_id_str = auth_context
        .get_spiffe_machine_id()
        .ok_or_else(|| Status::unauthenticated("no machine identity in client certificate"))?;

    tracing::info!(machine_id = %machine_id_str, "Processing machine identity request");

    let machine_id: MachineId = machine_id_str
        .parse()
        .map_err(|e| CarbideError::InvalidArgument(format!("invalid machine ID format: {e}")))?;

    let req = request.get_ref();

    let identity_row = api
        .database_connection
        .with_txn(|txn| {
            Box::pin(
                async move { tenant_identity_config::find_by_machine_id(txn, &machine_id).await },
            )
        })
        .await??;

    let allowed: &[String] = identity_row.allowed_audiences.0.as_slice();
    let audiences: Vec<String> = if req.audience.is_empty() {
        vec![identity_row.default_audience.clone()]
    } else {
        req.audience.clone()
    };
    validate_audiences_in_allowlist(&audiences, allowed)?;

    let enc_key = identity_row
        .current_encrypted_signing_key()
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let private_pem =
        decrypt_machine_identity_ciphertext(api.credential_manager.as_ref(), enc_key.as_str())
            .await
            .inspect_err(|e| {
                carbide_instrument::emit(MachineIdentityStoredSecretDecryptionFailed::SigningKey {
                    organization_id: identity_row.organization_id.to_string(),
                    error: e.message().to_string(),
                });
            })
            .map_err(|_| {
                CarbideError::internal("stored signing key could not be decrypted".to_string())
            })?;

    let active_pub = identity_row
        .current_signing_public()
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
    let signer = Es256Signer::new(&private_pem, active_pub.kid())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    let now = Utc::now().timestamp();

    if let (Some(token_endpoint), Some(subject_token_audience), Some(auth_method)) = (
        identity_row.token_endpoint.as_deref().none_if_empty(),
        identity_row
            .subject_token_audience
            .as_deref()
            .none_if_empty(),
        identity_row.auth_method,
    ) {
        let subject_ttl = i64::from(identity_row.token_ttl_sec);
        let exp = now.saturating_add(subject_ttl);
        let subject_aud_json = json!([subject_token_audience]);
        let request_meta_data = json!({ "aud": &audiences });
        let claims = json!({
            "sub": jwt_sub_claim(&identity_row.subject_prefix, &machine_id),
            "iss": identity_row.issuer,
            "aud": subject_aud_json,
            "exp": exp,
            "iat": now,
            "nbf": now,
            "request_meta_data": request_meta_data,
        });

        let subject_jwt = signer
            .sign(&claims, &SignOptions::default())
            .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

        let delegation_plain = decrypt_token_delegation_encrypted_blob(
            api.credential_manager.as_ref(),
            identity_row.encrypted_auth_method_config.as_ref(),
        )
        .await
        .inspect_err(|e| {
            carbide_instrument::emit(
                MachineIdentityStoredSecretDecryptionFailed::TokenDelegationAuth {
                    organization_id: identity_row.organization_id.to_string(),
                    error: e.message().to_string(),
                },
            );
        })?;
        let delegation_creds =
            token_delegation_credentials(auth_method, delegation_plain.as_deref())?;
        let http = token_exchange_http_client(
            api.runtime_config
                .machine_identity
                .token_endpoint_http_proxy
                .as_deref(),
        )?;
        let response = token_exchange_request(
            &http,
            token_endpoint,
            &subject_jwt,
            &audiences,
            delegation_creds.as_ref(),
        )
        .await?;
        return Ok(Response::new(response));
    }

    let ttl = i64::from(identity_row.token_ttl_sec);
    let exp = now.saturating_add(ttl);
    let aud_claim = if audiences.len() == 1 {
        json!(audiences[0].clone())
    } else {
        json!(audiences)
    };

    let claims = json!({
        "sub": jwt_sub_claim(&identity_row.subject_prefix, &machine_id),
        "iss": identity_row.issuer,
        "aud": aud_claim,
        "exp": exp,
        "iat": now,
        "nbf": now,
    });

    let token = signer
        .sign(&claims, &SignOptions::default())
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    let response = MachineIdentityResponse {
        access_token: token,
        issued_token_type: "urn:ietf:params:oauth:token-type:jwt".to_string(),
        token_type: "Bearer".to_string(),
        expires_in_sec: u32::try_from(identity_row.token_ttl_sec).unwrap_or(0),
    };

    Ok(Response::new(response))
}

/// Public JWKS for JWT verification (intended for unauthenticated callers via REST gateway).
pub(crate) async fn get_jwks(
    api: &Api,
    request: Request<JwksRequest>,
) -> Result<Response<Jwks>, Status> {
    log_request_data(&request);
    require_machine_identity_site_enabled(api)?;

    let req = request.into_inner();
    let org_raw = req.organization_id.trim();
    if org_raw.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_raw
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;

    let jwks_kind = match req.kind {
        None => JwksKind::Unspecified,
        Some(raw) => JwksKind::try_from(raw).map_err(|_| {
            CarbideError::InvalidArgument(format!("invalid JwksRequest.kind enum value: {raw}"))
        })?,
    };

    let jwk_key_use = match jwks_kind {
        JwksKind::Unspecified | JwksKind::Oidc => {
            crate::machine_identity::JwkPublicKeyUse::OidcSignature
        }
        JwksKind::Spiffe => crate::machine_identity::JwkPublicKeyUse::SpiffeJwtSvid,
    };

    let cfg = load_enabled_identity_for_well_known(api, &org_id).await?;

    let mut keys: Vec<serde_json::Value> = Vec::new();
    if let Some(ref doc) = cfg.signing_key_public_1 {
        push_jwk_from_signing_public_doc(&mut keys, &doc.0, jwk_key_use)?;
    }
    if let Some(ref doc) = cfg.signing_key_public_2 {
        push_jwk_from_signing_public_doc(&mut keys, &doc.0, jwk_key_use)?;
    }
    keys.sort_by(|a, b| {
        let ka = a.get("kid").and_then(|v| v.as_str()).unwrap_or("");
        let kb = b.get("kid").and_then(|v| v.as_str()).unwrap_or("");
        ka.cmp(kb)
    });
    if keys.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id.as_str().to_string(),
        }
        .into());
    }
    let jwks = crate::machine_identity::jwks_document_string(&keys)
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    Ok(Response::new(Jwks { jwks }))
}

/// OpenID Provider metadata (issuer, JWKS URIs). Signing algorithms are listed explicitly; key material is in GetJWKS `jwks`.
pub(crate) async fn get_open_id_configuration(
    api: &Api,
    request: Request<OpenIdConfigRequest>,
) -> Result<Response<OpenIdConfiguration>, Status> {
    log_request_data(&request);
    require_machine_identity_site_enabled(api)?;

    let req = request.into_inner();
    let org_raw = req.organization_id.trim();
    if org_raw.is_empty() {
        return Err(
            CarbideError::InvalidArgument("organization_id is required".to_string()).into(),
        );
    }
    let org_id: TenantOrganizationId = org_raw
        .parse()
        .map_err(|e: InvalidTenantOrg| CarbideError::InvalidArgument(e.to_string()))?;

    let cfg = load_enabled_identity_for_well_known(api, &org_id).await?;

    if cfg.issuer.as_str().trim().is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id.as_str().to_string(),
        }
        .into());
    }

    let active_pub = cfg
        .current_signing_public()
        .map_err(|_| CarbideError::NotFoundError {
            kind: "tenant_identity_config",
            id: org_id.as_str().to_string(),
        })?;

    Ok(Response::new(OpenIdConfiguration {
        issuer: cfg.issuer.as_str().to_string(),
        jwks_uri: jwks_uri_for_issuer(cfg.issuer.as_ref()),
        spiffe_jwks_uri: spiffe_jwks_uri_for_issuer(cfg.issuer.as_ref()),
        response_types_supported: vec!["token".into()],
        subject_types_supported: vec!["public".into()],
        id_token_signing_alg_values_supported: vec![active_pub.alg().to_string()],
    }))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;

    use super::*;

    #[test]
    fn jwt_sub_claim_trims_trailing_slash_on_prefix() {
        let mid =
            MachineId::from_str("fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g")
                .unwrap();
        let expected = format!("spiffe://td.example/myorg/{mid}");
        assert_eq!(jwt_sub_claim("spiffe://td.example/myorg", &mid), expected);
        assert_eq!(jwt_sub_claim("spiffe://td.example/myorg/", &mid), expected);
    }

    #[test]
    fn audiences_empty_request_uses_default_then_validate() {
        let allowed = vec!["a".to_string(), "b".to_string()];
        let req: Vec<String> = vec![];
        let audiences: Vec<String> = if req.is_empty() {
            vec!["a".to_string()]
        } else {
            req
        };
        validate_audiences_in_allowlist(&audiences, &allowed).unwrap();
        assert_eq!(audiences, vec!["a".to_string()]);
    }

    #[test]
    fn audiences_requested_must_each_be_allowed() {
        let allowed = vec!["a".to_string(), "b".to_string()];
        let req = vec!["b".to_string()];
        let audiences: Vec<String> = if req.is_empty() {
            vec!["a".to_string()]
        } else {
            req
        };
        validate_audiences_in_allowlist(&audiences, &allowed).unwrap();
        assert_eq!(audiences, vec!["b".to_string()]);
    }

    #[test]
    fn audiences_not_in_allowed_errors() {
        let allowed = vec!["a".to_string(), "b".to_string()];
        let audiences = vec!["x".to_string()];
        let err = validate_audiences_in_allowlist(&audiences, &allowed).unwrap_err();
        assert!(err.message().contains("allowed_audiences"));
    }
}
