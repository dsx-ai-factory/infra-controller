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

// these are not visible outside of this crate
mod client;
mod keystore;
mod parser;

// re-exports
use std::collections as stdcol;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
pub use client::{NrasVerifierClient, VerifierClient};
pub use keystore::{KeyStore, NrasKeyStore};
pub use parser::Parser;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub nras_url: String,
    pub nras_gpu_url_suffix: String,
    pub nras_jwks_url: String,
    pub validate_jwt_expiry: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            nras_url: Default::default(),
            nras_gpu_url_suffix: Default::default(),
            nras_jwks_url: Default::default(),
            validate_jwt_expiry: true,
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum NrasError {
    #[error("error talking to NRAS: {0}")]
    Communication(String),
    #[error("error serialising/deserialising: {0}")]
    Serde(String),
    #[error("error parsing verifier response: {0}")]
    ParsingVerifierResponse(String),
    #[error("error - NotImplemented")]
    NotImplemented,
    #[error("error parsing JWT token: {0}")]
    Jwt(String),
    #[error("error looking up a decoding key: {0}")]
    DecodingKeyNotFound(String),
    #[error("error forming JWK decoding key: {0}")]
    Jwk(String),
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

type Evidence = String;
type DeviceCertificate = String;

#[derive(Serialize, Default)]
pub enum MachineArchitecture {
    #[serde(rename(serialize = "BLACKWELL"))]
    #[default]
    Blackwell,
}

/// Converts a PEM certificate string (with optional literal `\n`) to a single-line Base64 string.
pub fn certificate_to_base64(pem: &str) -> String {
    let pem_with_newlines = pem.replace("\\n", "\n");
    STANDARD.encode(pem_with_newlines.as_bytes())
}

/// Evidence and certificate for one attestation device.
/// The `certificate` field must be the device CA certificate as a single-line Base64 string
/// (i.e. Base64-encoding of the PEM text). Use [certificate_to_base64] to produce this from PEM
// Double base64 encoding is unusual, but it's the format required by NRAS.
#[derive(Serialize)]
pub struct EvidenceCertificate {
    pub evidence: Evidence,
    pub certificate: DeviceCertificate,
    pub firmware_version: String,
}

#[derive(Serialize, Default)]
pub struct DeviceAttestationInfo {
    #[serde(rename(serialize = "evidence_list"))]
    pub ec: Vec<EvidenceCertificate>,
    #[serde(rename(serialize = "arch"))]
    pub architecture: MachineArchitecture,
    pub nonce: String,
}

impl From<DeviceAttestationInfo> for String {
    fn from(value: DeviceAttestationInfo) -> String {
        serde_json::to_string(&value).unwrap()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawAttestationOutcome {
    // this typically corresponds to ["JWT", "<jwt_token>"] entry in the response
    pub overall_outcome: (String, String),
    // this typically corresponds to {"GPU-0": "<jwt_token>"} entries
    pub devices_outcome: stdcol::HashMap<String, String>,
}

#[derive(Debug)]
pub struct ProcessedAttestationOutcome {
    pub attestation_passed: bool,
    // the key is submod name, e.g. "GPU-0", the value are the claims
    // extracted from that submod
    pub devices: stdcol::HashMap<String, stdcol::HashMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample PEM certificate (literal \n), same as nras_gpu example default.
    const SAMPLE_PEM_CERT: &str = r#"-----BEGIN CERTIFICATE-----\nMIIDdTCCAvqgAwIBAgIUYtQXVPLg2GAE54Bs6sNdIMpd/TQwCgYIKoZIzj0EAwMw\nZDEbMBkGA1UEBRMSNDBBNjgwNkQxQzE1Qzc5ODg5MQswCQYDVQQGEwJVUzEbMBkG\nA1UECgwSTlZJRElBIENvcnBvcmF0aW9uMRswGQYDVQQDDBJHQjEwMCBBMDEgRlNQ\nIEJST00wIBcNMjMwNjIwMDAwMDAwWhgPOTk5OTEyMzEyMzU5NTlaMHwxMTAvBgNV\nBAUTKDYyRDQxNzU0RjJFMEQ4NjAwNEU3ODA2Q0VBQzM1RDIwQ0E1REZEMzQxCzAJ\nBgNVBAYTAlVTMRswGQYDVQQKDBJOVklESUEgQ29ycG9yYXRpb24xHTAbBgNVBAMM\nFEdCMTAwIEEwMSBGU1AgRk1DIExGMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEtSJI\n29zBLJkYD8lZb/WdDT1HzXHOLvCMrhR3/6RxCo4XklE4OIndyEF8XcF0SsN8cPyM\n4E2dM9+v78wVGHAa1KDU9BOOCF4A2PwDvWPS4s73ss3ETFXfj62riouzjinoo4IB\nUTCCAU0wDgYDVR0PAQH/BAQDAgeAMB0GA1UdDgQWBBTi1BdU8uDYYATngGzqw10g\nyl39NDAfBgNVHSMEGDAWgBRUbGZrt6GJGV9NGhrPrTexfqHHVDA4BgNVHREEMTAv\noC0GCisGAQQBgxyCEgGgHwwdTlZJRElBOkdCMTAwOjQ4QjAyREY1RkM4QjM1QkQw\ngcAGB2eBBQUEAQEEgbQwgbGABk5WSURJQYENR0IxMDAgQTAxIEZTUIICMDODAQOE\nAQCFAQCmfjA9BglghkgBZQMEAgIEMIvqK/G+hBPSJwoJrV0ePA7QRfmfkMUIyR+f\n8FdOcM1kCjItNUjyKQbnpiCqdSKEfDA9BglghkgBZQMEAgIEMAAAAAAAAAAAAAAA\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIcFAJAAAAGIAcCJ\nAQAwCgYIKoZIzj0EAwMDaQAwZgIxALnF5aBVFkx93OynSs2tRDa1rzPqSlcq/DTi\nry+buM4hGG2a/lWnp07jBeaxQ94H6QIxAL87/oc7z1u5W1dYeZBm3Xb21kvRwiQy\nWbDoC/dJGGfyPPMNDt/lkLko9q0qg7KqCw==\n-----END CERTIFICATE-----\n"#;

    #[test]
    fn test_evidence_certificate_serialization_with_firmware_version() {
        let ec = EvidenceCertificate {
            evidence: "test_evidence".to_string(),
            certificate: "test_certificate".to_string(),
            firmware_version: "96.00.81.00.0F".to_string(),
        };

        let json = serde_json::to_string(&ec).unwrap();
        assert!(json.contains("\"firmware_version\":\"96.00.81.00.0F\""));
        assert!(json.contains("\"evidence\":\"test_evidence\""));
        assert!(json.contains("\"certificate\":\"test_certificate\""));
    }

    #[test]
    fn test_evidence_certificate_with_empty_firmware_version() {
        let ec = EvidenceCertificate {
            evidence: "test_evidence".to_string(),
            certificate: "test_certificate".to_string(),
            firmware_version: "".to_string(),
        };

        let json = serde_json::to_string(&ec).unwrap();
        assert!(json.contains("\"firmware_version\":\"\""));
    }

    #[test]
    fn test_device_attestation_info_serialization() {
        let info = DeviceAttestationInfo {
            ec: vec![EvidenceCertificate {
                evidence: "EeABGpR4QbD50HlMYaZVZl4sXy1iV0lG/omaPmg33F7DO4D3ABFgAAABDwAAGgELAIIIAKaAbRwVx5iJy8lS5J1Mcz2fbxX6vEMqjX/0F8oIVGbBYeC28oXwNbMAAERkQYLFGv/im6lEaYePMFKj4vFAz3AMiOwp9urtaLOTHrjNy/HtkHKCMV3SanUBdSPPVPWBjfoxpSpQ8ivO+2Fu0B3Showk+mLfCAqEzVX3SMY9cbF3jZXNcWuLuBBdwA==".to_string(),
                certificate: certificate_to_base64(SAMPLE_PEM_CERT),
                firmware_version: "BF-23.10-4".to_string(),
            }],
            architecture: MachineArchitecture::Blackwell,
            nonce: "test-nonce-123".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();

        // Check renamed fields
        assert!(json.contains("\"EeABGpR4QbD50HlMYaZVZl4sXy1iV0lG/omaPmg33F7DO4D3ABFgAAABDwAAGgELAIIIAKaAbRwVx5iJy8lS5J1Mcz2fbxX6vEMqjX/0F8oIVGbBYeC28oXwNbMAAERkQYLFGv/im6lEaYePMFKj4vFAz3AMiOwp9urtaLOTHrjNy/HtkHKCMV3SanUBdSPPVPWBjfoxpSpQ8ivO+2Fu0B3Showk+mLfCAqEzVX3SMY9cbF3jZXNcWuLuBBdwA==\""));
        assert!(json.contains("\"arch\":\"BLACKWELL\""));
        assert!(json.contains("\"nonce\":\"test-nonce-123\""));
        assert!(json.contains("\"firmware_version\":\"BF-23.10-4\""));
        // Certificate is Base64(PEM); decoded starts with PEM header
        assert!(json.contains("\"certificate\":\""));
    }

    #[test]
    fn test_device_attestation_info_into_string() {
        let info = DeviceAttestationInfo {
            ec: vec![EvidenceCertificate {
                evidence: "ev".to_string(),
                certificate: "cert".to_string(),
                firmware_version: "v1".to_string(),
            }],
            architecture: MachineArchitecture::Blackwell,
            nonce: "nonce".to_string(),
        };

        let json_string: String = info.into();
        assert!(json_string.contains("evidence_list"));
        assert!(json_string.contains("firmware_version"));
    }

    #[test]
    fn test_device_attestation_info_multiple_evidence_certificates() {
        let info = DeviceAttestationInfo {
            ec: vec![
                EvidenceCertificate {
                    evidence: "ev1".to_string(),
                    certificate: "cert1".to_string(),
                    firmware_version: "1.0.0".to_string(),
                },
                EvidenceCertificate {
                    evidence: "ev2".to_string(),
                    certificate: "cert2".to_string(),
                    firmware_version: "2.0.0".to_string(),
                },
            ],
            architecture: MachineArchitecture::Blackwell,
            nonce: "multi-nonce".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("1.0.0"));
        assert!(json.contains("2.0.0"));

        // Verify it's a valid JSON array
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let evidence_list = parsed.get("evidence_list").unwrap().as_array().unwrap();
        assert_eq!(evidence_list.len(), 2);
    }

    #[test]
    fn test_device_attestation_info_default() {
        let info = DeviceAttestationInfo::default();
        assert!(info.ec.is_empty());
        assert_eq!(info.nonce, "");

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"evidence_list\":[]"));
    }
}
