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

use std::collections as stdcol;

use carbide_instrument::red;
use jsonwebtoken as jst;

use crate::NrasError;

#[derive(Debug, serde::Deserialize)]
struct Jwk {
    kty: String,
    kid: String,
    x: Option<String>,
    y: Option<String>,
    x5c: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

pub trait KeyStore {
    fn find_key(&self, kid: &str) -> Option<jst::DecodingKey>;
}

#[derive(Debug)]
pub struct NrasKeyStore {
    keys: stdcol::HashMap<String, jst::DecodingKey>,
}

impl KeyStore for NrasKeyStore {
    fn find_key(&self, kid: &str) -> Option<jst::DecodingKey> {
        self.keys.get(kid).cloned()
    }
}

impl NrasKeyStore {
    pub async fn new_with_config(config: &crate::Config) -> Result<NrasKeyStore, crate::NrasError> {
        // The JWKS fetch is a required NRAS call on the attestation path; wrap the
        // GET and its status check in the RED triad so it is no longer dark and a
        // non-success status records outcome = error. Reuses the
        // carbide_external_call_duration_milliseconds family (operation="fetch_jwks").
        let response_text = red::instrumented("nras", "fetch_jwks", async {
            let jwks_response = reqwest::get(&config.nras_jwks_url).await?;
            let status_code = jwks_response.status();
            if status_code != reqwest::StatusCode::OK {
                // Status only: the response body can carry sensitive material, so it is
                // never placed in the error or its log.
                return Err(NrasError::Communication(format!(
                    "NRAS KeyStore returned status code {}",
                    status_code
                )));
            }
            // Read the body only after confirming success.
            Ok(jwks_response.text().await?)
        })
        .await?;

        // parse JWKS and find matching JWK
        let jwks: Jwks = serde_json::from_str(&response_text)
            .map_err(|e| NrasError::Serde(format!("Error parsing JWKS: {}", e)))?;

        let mut decoding_keys = stdcol::HashMap::<String, jst::DecodingKey>::new();

        for jwk in jwks.keys.iter() {
            let decoding_key =
                if let Some(x5c) = &jwk.x5c {
                    // Use first cert in chain
                    let pem = pem_wrap_cert(&x5c[0]);
                    jsonwebtoken::DecodingKey::from_ec_pem(pem.as_bytes()).map_err(|e| {
                        NrasError::Jwk(format!("Error creating DecodingKey from EC PEM: {}", e))
                    })?
                } else if jwk.kty == "EC" {
                    let x = jwk.x.as_ref().ok_or_else(|| {
                        NrasError::Jwk("Didn't find X component of EC".to_string())
                    })?;
                    let y = jwk.y.as_ref().ok_or_else(|| {
                        NrasError::Jwk("Didn't find Y component of EC".to_string())
                    })?;
                    jsonwebtoken::DecodingKey::from_ec_components(x, y).map_err(|e| {
                        NrasError::Jwk(format!(
                            "Error creating DecodingKey from EC X and Y components: {}",
                            e
                        ))
                    })?
                } else {
                    return Err(NrasError::Jwk("Unsupported JWK key type".to_string()));
                };

            decoding_keys.insert(jwk.kid.clone(), decoding_key);
        }

        Ok(NrasKeyStore {
            keys: decoding_keys,
        })
    }
}

fn pem_wrap_cert(b64: &str) -> String {
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        b64
    )
}
