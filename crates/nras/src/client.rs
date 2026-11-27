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

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use carbide_instrument::red;
use serde_json as sj;

use crate::{DeviceAttestationInfo, NrasError, VerifierResponseJwt};

/// Submits device evidence to a remote verifier.
#[async_trait]
pub trait VerifierClient: std::fmt::Debug + Send + Sync + 'static {
    /// Verifies one or more labelled SPDM evidence items and returns an unvalidated EAR JWT.
    /// Labels must be unique and must not use reserved collection names. Malformed encodings,
    /// an empty collection and nonces outside 32–96 decoded bytes are rejected before HTTP.
    /// HTTP failures are errors; a negative appraisal is carried by the successful EAR result.
    async fn attest(
        &self,
        device_info: Vec<DeviceAttestationInfo>,
    ) -> Result<VerifierResponseJwt, NrasError>;
}

/// NRAS client using CMW JSON requests and compact EAR JWT responses.
/// Rejects responses without three non-empty, unpadded Base64url segments.
/// Signature and claim validation remain the caller's responsibility.
#[derive(Debug)]
pub struct NrasVerifierClient {
    instrumentation: carbide_instrument::Instrumentation,
    config: crate::Config,
    // Middleware propagates the current span's W3C trace context to NRAS.
    http_client: reqwest_middleware::ClientWithMiddleware,
}

impl NrasVerifierClient {
    /// Creates a verifier whose RED metrics use the supplied destination.
    pub fn new_with_config(
        config: &crate::Config,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> NrasVerifierClient {
        NrasVerifierClient {
            instrumentation,
            config: config.clone(),
            http_client: reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                .with(reqwest_tracing::TracingMiddleware::default())
                .build(),
        }
    }
}

#[async_trait]
impl VerifierClient for NrasVerifierClient {
    async fn attest(
        &self,
        attestation_values: Vec<DeviceAttestationInfo>,
    ) -> Result<VerifierResponseJwt, NrasError> {
        if attestation_values.is_empty() {
            return Err(NrasError::Serde(
                "At least one evidence item is required".to_string(),
            ));
        }

        let mut cmw_msg = sj::json!({
            "__cmwc_t": "tag:nvidia.com,2026:verifier-input/profiles/base/pre-1.0.0",
        });

        // every attester in the message has a unique entry that must be populated in turn
        for attestation_value in attestation_values {
            let label = &attestation_value.device_id;
            // Evidence labels must leave collection metadata available to the verifier.
            if matches!(label.as_str(), "__cmwc_t" | "nonce" | "appraisal-settings") {
                return Err(NrasError::Bmc(format!(
                    "device id {label} is reserved for collection metadata"
                )));
            }
            if cmw_msg.get(label).is_some() {
                return Err(NrasError::Bmc(format!(
                    "device id {label} cannot be repeated"
                )));
            }

            // re-encode (decode and later encode again) values to match the NRAS format
            let evidence_bytes = STANDARD
                .decode(&attestation_value.evidence)
                .map_err(|error| NrasError::Bmc(format!("Invalid Base64 evidence: {error}")))?;
            let certificate_bytes = STANDARD
                .decode(&attestation_value.certificate)
                .map_err(|error| NrasError::Bmc(format!("Invalid Base64 certificate: {error}")))?;

            // Validate the nonce before converting it to the format NRAS expects.
            let nonce = &attestation_value.nonce;
            if !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) || nonce.len() % 2 != 0 {
                return Err(NrasError::Serde(
                    "Invalid hex nonce: expected pairs of ASCII hex characters".to_string(),
                ));
            }
            // The schema permits at least 32 bytes and at most 128 Base64url characters (96 bytes).
            if nonce.len() < 64 || nonce.len() > 192 {
                return Err(NrasError::Serde(
                    "nonce must contain 64–192 hex characters (32–96 bytes)".to_string(),
                ));
            }
            // NRAS checks evidence freshness using the original nonce bytes. Decode the
            // hex string before encoding those bytes as Base64url for the request.
            let nonce_bytes = (0..nonce.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&nonce[index..index + 2], 16))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| NrasError::Serde(format!("Invalid hex nonce: {error}")))?;

            // Each CMW record carries the original bytes as Base64url without padding.
            cmw_msg[label] = sj::json!({
                "__cmwc_t": "tag:nvidia.com,2026:evidence-item/v1",
                "evidence": [
                    "application/vnd.nvidia.spdm-measurement-transcript",
                    URL_SAFE_NO_PAD.encode(&evidence_bytes)
                ],
                "certificate": [
                    "application/vnd.nvidia.pem-cert-chain",
                    URL_SAFE_NO_PAD.encode(&certificate_bytes)
                ],
                "nonce": ["application/octet-stream", URL_SAFE_NO_PAD.encode(&nonce_bytes)]
            });
        }

        let request = self
            .http_client
            .post(format!(
                "{}/v1/verify",
                self.config.nras_url.trim_end_matches('/')
            ))
            .header("Content-Type", "application/cmw+json")
            .header("Accept", "application/eat+jwt")
            .body(cmw_msg.to_string());

        // Record request failures inside the RED wrapper. Evidence, nonces, certificates
        // and response bodies must never be included in these errors or their logs.
        let response_text = red::instrumented(&self.instrumentation, "nras", "attest", async {
            let att_response = request.send().await?;
            let status_code = att_response.status();
            if status_code != reqwest::StatusCode::OK {
                return Err(NrasError::Communication(format!(
                    "NRAS returned status code {}",
                    status_code
                )));
            }
            let response_text = att_response.text().await?;

            // Check the compact encoding before reporting success, without exposing the body.
            let mut segments = response_text.split('.');
            if segments.clone().count() != 3
                || segments
                    .any(|segment| segment.is_empty() || URL_SAFE_NO_PAD.decode(segment).is_err())
            {
                return Err(NrasError::Jwt(
                    "expected a compact JWT with three non-empty Base64url segments".to_string(),
                ));
            }
            Ok(response_text)
        })
        .await?;

        // The caller still needs to validate the signature, claims and appraisal verdict.
        Ok(response_text.into())
    }
}
