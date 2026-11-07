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
use clap::Parser;
use nras::VerifierClient;

const DEFAULT_NRAS_URL: &str = "https://nras.attestation-dev.nvidia.com";
const DEFAULT_NRAS_GPU_URL_SUFFIX: &str = "/v4/attest/gpu/health";
const DEFAULT_NONCE: &str = "abcdef13455";
const DEFAULT_EVIDENCE: &str = "EeABGpR4QbD50HlMYaZVZl4sXy1iV0lG/omaPmg33F7DO4D3ABFgAAABDwAAGgELAIIIAKaAbRwVx5iJy8lS5J1Mcz2fbxX6vEMqjX/0F8oIVGbBYeC28oXwNbMAAERkQYLFGv/im6lEaYePMFKj4vFAz3AMiOwp9urtaLOTHrjNy/HtkHKCMV3SanUBdSPPVPWBjfoxpSpQ8ivO+2Fu0B3Showk+mLfCAqEzVX3SMY9cbF3jZXNcWuLuBBdwA==";
const DEFAULT_FIRMWARE_VERSION: &str = "97.10.52.00.17";
const DEFAULT_ARCHITECTURE: &str = "blackwell";

/// CLI names for nras::MachineArchitecture (lowercase). Update when adding new variants to the lib.
const SUPPORTED_ARCHITECTURES: &[&str] = &["blackwell"];

fn parse_architecture(s: &str) -> Option<nras::MachineArchitecture> {
    match s.to_lowercase().as_str() {
        "blackwell" => Some(nras::MachineArchitecture::Blackwell),
        _ => None,
    }
}

/// Sample PEM certificate (literal \n). Used when --certificate-path is not provided.
const SAMPLE_PEM_CERT: &str = r#"-----BEGIN CERTIFICATE-----\nMIIDdTCCAvqgAwIBAgIUYtQXVPLg2GAE54Bs6sNdIMpd/TQwCgYIKoZIzj0EAwMw\nZDEbMBkGA1UEBRMSNDBBNjgwNkQxQzE1Qzc5ODg5MQswCQYDVQQGEwJVUzEbMBkG\nA1UECgwSTlZJRElBIENvcnBvcmF0aW9uMRswGQYDVQQDDBJHQjEwMCBBMDEgRlNQ\nIEJST00wIBcNMjMwNjIwMDAwMDAwWhgPOTk5OTEyMzEyMzU5NTlaMHwxMTAvBgNV\nBAUTKDYyRDQxNzU0RjJFMEQ4NjAwNEU3ODA2Q0VBQzM1RDIwQ0E1REZEMzQxCzAJ\nBgNVBAYTAlVTMRswGQYDVQQKDBJOVklESUEgQ29ycG9yYXRpb24xHTAbBgNVBAMM\nFEdCMTAwIEEwMSBGU1AgRk1DIExGMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAEtSJI\n29zBLJkYD8lZb/WdDT1HzXHOLvCMrhR3/6RxCo4XklE4OIndyEF8XcF0SsN8cPyM\n4E2dM9+v78wVGHAa1KDU9BOOCF4A2PwDvWPS4s73ss3ETFXfj62riouzjinoo4IB\nUTCCAU0wDgYDVR0PAQH/BAQDAgeAMB0GA1UdDgQWBBTi1BdU8uDYYATngGzqw10g\nyl39NDAfBgNVHSMEGDAWgBRUbGZrt6GJGV9NGhrPrTexfqHHVDA4BgNVHREEMTAv\noC0GCisGAQQBgxyCEgGgHwwdTlZJRElBOkdCMTAwOjQ4QjAyREY1RkM4QjM1QkQw\ngcAGB2eBBQUEAQEEgbQwgbGABk5WSURJQYENR0IxMDAgQTAxIEZTUIICMDODAQOE\nAQCFAQCmfjA9BglghkgBZQMEAgIEMIvqK/G+hBPSJwoJrV0ePA7QRfmfkMUIyR+f\n8FdOcM1kCjItNUjyKQbnpiCqdSKEfDA9BglghkgBZQMEAgIEMAAAAAAAAAAAAAAA\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAIcFAJAAAAGIAcCJ\nAQAwCgYIKoZIzj0EAwMDaQAwZgIxALnF5aBVFkx93OynSs2tRDa1rzPqSlcq/DTi\nry+buM4hGG2a/lWnp07jBeaxQ94H6QIxAL87/oc7z1u5W1dYeZBm3Xb21kvRwiQy\nWbDoC/dJGGfyPPMNDt/lkLko9q0qg7KqCw==\n-----END CERTIFICATE-----\n"#;

#[derive(Parser, Debug)]
#[command(name = "nras_gpu", about = "NRAS GPU attestation example")]
struct Args {
    /// NRAS base URL
    #[arg(long, default_value = DEFAULT_NRAS_URL, env = "NRAS_URL")]
    nras_url: String,

    /// NRAS GPU attest endpoint path (appended to nras_url)
    #[arg(long, default_value = DEFAULT_NRAS_GPU_URL_SUFFIX, env = "NRAS_GPU_URL_SUFFIX")]
    nras_gpu_url_suffix: String,

    /// NRAS JWKS URL for token validation (default: <nras_url>/.well-known/jwks.json)
    #[arg(long, env = "NRAS_JWKS_URL")]
    nras_jwks_url: Option<String>,

    /// Path to PEM certificate file (device CA certificate). If omitted, a sample cert is used.
    #[arg(long, short = 'c', env = "NRAS_CERTIFICATE_PATH")]
    certificate_path: Option<std::path::PathBuf>,

    /// Attestation nonce
    #[arg(long, default_value = DEFAULT_NONCE, env = "NRAS_NONCE")]
    nonce: String,

    /// Evidence payload (base64-encoded measurement)
    #[arg(long, default_value = DEFAULT_EVIDENCE, env = "NRAS_EVIDENCE")]
    evidence: String,

    /// Firmware version string for the device
    #[arg(long, default_value = DEFAULT_FIRMWARE_VERSION, env = "NRAS_FIRMWARE_VERSION")]
    firmware_version: String,

    /// Machine architecture (e.g. blackwell)
    #[arg(long, default_value = DEFAULT_ARCHITECTURE, env = "NRAS_ARCHITECTURE")]
    architecture: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let nras_jwks_url = args.nras_jwks_url.unwrap_or_else(|| {
        let base = args.nras_url.trim_end_matches('/');
        format!("{}/.well-known/jwks.json", base)
    });

    let config = nras::Config {
        nras_url: args.nras_url,
        nras_gpu_url_suffix: args.nras_gpu_url_suffix,
        nras_jwks_url,
        ..Default::default()
    };

    let pem_cert = match &args.certificate_path {
        Some(path) => std::fs::read_to_string(path)?,
        None => SAMPLE_PEM_CERT.to_string(),
    };

    let certificate = nras::certificate_to_base64(&pem_cert);

    let nras_verifier_client = nras::NrasVerifierClient::new_with_config(&config);

    let supported = SUPPORTED_ARCHITECTURES.join(", ");
    let architecture = parse_architecture(&args.architecture).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Unsupported architecture '{}'. Supported values: {}",
                args.architecture, supported
            ),
        )
    })?;

    let verifier_response = nras_verifier_client
        .attest_gpu(&nras::DeviceAttestationInfo {
            nonce: args.nonce,
            architecture,
            ec: vec![nras::EvidenceCertificate {
                evidence: args.evidence,
                certificate,
                firmware_version: args.firmware_version,
            }],
        })
        .await?;

    println!("RawAttestationOutcome is: {:#?}", verifier_response);

    let nras_keystore = nras::NrasKeyStore::new_with_config(&config).await?;
    let parser = nras::Parser::new_with_config(&config);

    let processed_response =
        parser.parse_attestation_outcome(&verifier_response, &nras_keystore)?;

    println!("ProcessedAttestationOutcome is: {:#?}", processed_response);

    Ok(())
}
