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

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use nras::{DeviceAttestationInfo, KeyStore};
use serde_json::{Value, json};

pub(crate) const KEY_ID: &str = "test-nras-es384";
pub(crate) const NONCE_HEX: &str = "fb";
pub(crate) const CERTIFICATE_CHAIN: &str = "-----BEGIN CERTIFICATE-----\nleaf\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nissuer\n-----END CERTIFICATE-----\n";

pub(crate) fn device(label: &str) -> DeviceAttestationInfo {
    DeviceAttestationInfo {
        device_id: label.to_string(),
        evidence: STANDARD.encode([0xfb, 0xff]),
        certificate: nras::certificate_to_base64(CERTIFICATE_CHAIN),
        nonce: NONCE_HEX.repeat(32),
    }
}

pub(crate) fn ear_claims() -> Value {
    let now = jsonwebtoken::get_current_timestamp();
    json!({
        "eat_profile": "test-ear-profile",
        "iss": "https://nras.example.test",
        "jti": "test-ear",
        "iat": now,
        "nbf": now - 10,
        "exp": now + 3600,
        "ear_verifier_id": {"build": "test"},
        "ear_status": "affirming",
        "ear_nvidia_inputs": {"digests": [], "input-extension": true},
        "submods": {
            "gpu_0": {
                "eat_profile": "test-submodule-profile",
                "evidence_profile": "test-spdm-profile",
                "ear_status": "affirming",
                "eat_nonce": URL_SAFE_NO_PAD.encode([0xfb; 32]),
                "ear_verifier_claims": {"measurement": true},
                "ear_attester_claims": {"model": "test-gpu"},
                "submodule-extension": [1, 2],
            }
        },
        "ear-extension": "preserved",
    })
}

pub(crate) struct SignedEarFixture {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    pub(crate) jwks: String,
}

impl SignedEarFixture {
    pub(crate) fn new() -> Self {
        // Generate test signing material in memory; no private keys are stored in the repository.
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let certificate = rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap();
        let public_key = key.public_key_raw();
        Self {
            encoding_key: EncodingKey::from_ec_der(&key.serialize_der()),
            decoding_key: DecodingKey::from_ec_pem(certificate.pem().as_bytes()).unwrap(),
            jwks: json!({"keys": [{
                "kty": "EC",
                "crv": "P-384",
                "kid": KEY_ID,
                "x5c": [STANDARD.encode(certificate.der())],
                "x": URL_SAFE_NO_PAD.encode(&public_key[1..49]),
                "y": URL_SAFE_NO_PAD.encode(&public_key[49..]),
            }]})
            .to_string(),
        }
    }

    pub(crate) fn sign(&self, claims: &Value, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::ES384);
        header.kid = kid.map(str::to_string);
        jsonwebtoken::encode(&header, claims, &self.encoding_key).unwrap()
    }
}

impl KeyStore for SignedEarFixture {
    fn find_key(&self, kid: &str) -> Option<DecodingKey> {
        (kid == KEY_ID).then(|| self.decoding_key.clone())
    }
}
