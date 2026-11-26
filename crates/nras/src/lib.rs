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

//! NVIDIA Remote Attestation Service client for SPDM evidence and signed EAR results.

mod client;
mod keystore;

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
pub use client::{NrasVerifierClient, VerifierClient};
use jsonwebtoken as jst;
pub use keystore::{KeyStore, NrasKeyStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Service locations and token validation settings for NRAS.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Service base URL; the client appends `/v1/verify`.
    pub nras_url: String,
    /// Full URL of the service's `/.well-known/jwks.json` signing-key endpoint.
    pub nras_jwks_url: String,
    /// Check JWT expiry when true; [`Config::default`] enables this check.
    /// Required when deserialising configuration. Signature and not-before checks always apply.
    pub validate_jwt_expiry: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            nras_url: Default::default(),
            nras_jwks_url: Default::default(),
            validate_jwt_expiry: true,
        }
    }
}

/// A request, transport, signing-key or token-validation failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum NrasError {
    #[error("error talking to NRAS: {0}")]
    Communication(String),
    #[error("error serialising/deserialising: {0}")]
    Serde(String),
    #[error("error parsing JWT token: {0}")]
    Jwt(String),
    #[error("error looking up a decoding key: {0}")]
    DecodingKeyNotFound(String),
    #[error("error forming JWK decoding key: {0}")]
    Jwk(String),
    #[error("error with BMC data: {0}")]
    Bmc(String),
}

impl From<reqwest::Error> for NrasError {
    fn from(value: reqwest::Error) -> NrasError {
        NrasError::Communication(format!("Communication error: {}", value))
    }
}

impl From<reqwest_middleware::Error> for NrasError {
    fn from(value: reqwest_middleware::Error) -> NrasError {
        NrasError::Communication(format!("Communication error: {}", value))
    }
}

/// Base64-encodes a PEM certificate chain, expanding optional literal `\n` separators.
/// The caller supplies the complete chain, ordered from the leaf certificate onwards.
pub fn certificate_to_base64(pem: &str) -> String {
    let pem_with_newlines = pem.replace("\\n", "\n");
    STANDARD.encode(pem_with_newlines.as_bytes())
}

/// One labelled SPDM evidence item to include in a CMW verification collection.
pub struct DeviceAttestationInfo {
    /// Unique label preserved as a submodule key in the returned EAR.
    /// The collection names `__cmwc_t`, `nonce` and `appraisal-settings` are reserved.
    pub device_id: String,
    /// Standard Base64-encoded SPDM measurement transcript.
    pub evidence: String,
    /// Standard Base64-encoded PEM certificate chain, leaf first through L1.
    pub certificate: String,
    /// Hexadecimal encoding of the 32–96 nonce bytes bound into this evidence.
    pub nonce: String,
}

/// An unvalidated compact EAR JWT returned by a verifier implementation.
#[derive(Debug)]
pub struct VerifierResponseJwt(String);

impl From<String> for VerifierResponseJwt {
    /// Wraps a token without validating its signature or claims.
    fn from(token: String) -> Self {
        Self(token)
    }
}

impl AsRef<str> for VerifierResponseJwt {
    /// Returns the original token, for example for storage before later validation.
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl VerifierResponseJwt {
    /// Validates the ES384 signature, key identifier and not-before time, then parses the EAR.
    /// Expiry validation is optional; time checks allow the JWT library's 60-second clock skew.
    /// A validated token can still carry a negative appraisal in [`EarClaims::ear_status`].
    pub fn validate_and_parse(
        &self,
        keystore: &dyn KeyStore,
        validate_jwt_expiry: bool,
    ) -> Result<EarClaims, NrasError> {
        // decode header and obtain validation algorithm
        let jwt_header = jst::decode_header(&self.0)
            .map_err(|e| NrasError::Jwt(format!("Error decoding header: {}", e)))?;

        let kid = jwt_header
            .kid
            .ok_or(NrasError::Jwt("No kid found in JWT header".to_string()))?;

        let decoding_key = keystore
            .find_key(&kid)
            .ok_or(NrasError::DecodingKeyNotFound(format!(
                "DecodingKey not found for kid - {}",
                kid
            )))?;

        // The service profile permits ES384; do not expand the accepted algorithms
        // merely because an incoming token names a different one.
        let mut validation = jst::Validation::new(jst::Algorithm::ES384);
        validation.validate_exp = validate_jwt_expiry;
        validation.validate_nbf = true;

        // now decode the payload and perform the validation using the jwt_pub_key
        let jwt_token_data: jst::TokenData<EarClaims> =
            jst::decode::<EarClaims>(&self.0, &decoding_key, &validation)
                .map_err(|e| NrasError::Jwt(format!("Error decoding JWT token: {}", e)))?;

        Ok(jwt_token_data.claims)
    }
}

/// Validated Entity Attestation Result claims, including each submitted evidence label.
/// Claims without a concrete type in the NRAS OpenAPI schema are preserved as JSON.
#[derive(Debug, Deserialize)]
pub struct EarClaims {
    /// Profile identifying the EAR claim set.
    pub eat_profile: Value,
    /// Token issuer.
    pub iss: String,
    /// Unique token identifier.
    pub jti: String,
    /// Issue time in Unix seconds.
    pub iat: i64,
    /// Earliest valid time in Unix seconds.
    pub nbf: i64,
    /// Expiry time in Unix seconds.
    pub exp: i64,
    /// Echo of the optional request-level nonce; evidence freshness uses per-item nonces.
    #[serde(default)]
    pub eat_nonce: Option<Value>,
    /// Identity of the verifier producing the appraisal.
    pub ear_verifier_id: Value,
    /// Worst appraisal status across the evidence submodules.
    pub ear_status: EarStatus,
    /// Digests identifying the verifier inputs.
    pub ear_nvidia_inputs: NvidiaInputs,
    /// Evidence appraisals keyed by the caller's original device labels.
    pub submods: BTreeMap<String, EarSubmodule>,
    /// Additional claims defined by the EAR profile or extensions.
    #[serde(flatten)]
    pub additional_claims: BTreeMap<String, Value>,
}

/// An appraisal verdict, independent of HTTP success or JWT signature validity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EarStatus {
    /// Evidence supports the attester's trustworthiness.
    Affirming,
    /// Evidence carries a warning requiring an acceptance-policy decision.
    Warning,
    /// Evidence does not support the attester's trustworthiness.
    Contraindicated,
}

/// NVIDIA input-identification claims carried by an EAR.
#[derive(Debug, Deserialize)]
pub struct NvidiaInputs {
    /// Profile-defined input digests; their detailed shape is not fixed by the verify schema.
    pub digests: Value,
    /// Additional input-identification claims.
    #[serde(flatten)]
    pub additional_claims: BTreeMap<String, Value>,
}

/// Appraisal and attester claims for one labelled evidence item.
#[derive(Debug, Deserialize)]
pub struct EarSubmodule {
    /// Profile identifying this submodule's claim set.
    pub eat_profile: Value,
    /// Profile describing the submitted evidence.
    pub evidence_profile: Value,
    /// Verdict for this evidence item.
    pub ear_status: EarStatus,
    /// Nonce bound into this item's signed evidence.
    pub eat_nonce: Value,
    /// Claims produced by the verifier.
    pub ear_verifier_claims: Value,
    /// Claims describing the attester.
    pub ear_attester_claims: Value,
    /// Optional profile-defined appraisal purpose.
    #[serde(default)]
    pub ear_nvidia_purpose: Option<Value>,
    /// Optional details explaining an appraisal failure.
    #[serde(default)]
    pub ear_nvidia_error_details: Option<Value>,
    /// Additional submodule claims defined by profiles or extensions.
    #[serde(flatten)]
    pub additional_claims: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn certificate_encoding_preserves_the_complete_chain() {
        value_scenarios!(run = |pem: &str| {
            String::from_utf8(STANDARD.decode(certificate_to_base64(pem)).unwrap()).unwrap()
        };
            "PEM separators" {
                "leaf\\nissuer\\n" => "leaf\nissuer\n".to_string(),
                "leaf\nissuer\n" => "leaf\nissuer\n".to_string(),
            }
        );
    }
}
