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

const DEFAULT_NRAS_URL: &str = "https://nras.attestation.nvidia.com";

#[derive(Parser, Debug)]
#[command(
    name = "nras_gpu",
    about = "Verify SPDM GPU evidence and validate the signed NRAS EAR"
)]
struct Args {
    /// NRAS service base URL; /v1/verify is appended
    #[arg(long, default_value = DEFAULT_NRAS_URL, env = "NRAS_URL")]
    nras_url: String,

    /// Signing-key URL (default: <nras-url>/.well-known/jwks.json)
    #[arg(long, env = "NRAS_JWKS_URL")]
    nras_jwks_url: Option<String>,

    /// Label preserved in the EAR submodules
    #[arg(long, default_value = "gpu_0", env = "NRAS_DEVICE_ID")]
    device_id: String,

    /// PEM certificate chain file, ordered from the leaf certificate through L1
    #[arg(long, short = 'c', env = "NRAS_CERTIFICATE_PATH")]
    certificate_path: std::path::PathBuf,

    /// Hexadecimal nonce bound into the evidence (32–96 bytes, or 64–192 hex characters)
    #[arg(long, env = "NRAS_NONCE")]
    nonce: String,

    /// Standard Base64-encoded SPDM measurement transcript, signed with the supplied nonce
    #[arg(long, env = "NRAS_EVIDENCE")]
    evidence: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Resolve the signing-key URL from the service base, never from the verify path.
    let nras_jwks_url = args.nras_jwks_url.unwrap_or_else(|| {
        format!(
            "{}/.well-known/jwks.json",
            args.nras_url.trim_end_matches('/')
        )
    });
    let config = nras::Config {
        nras_url: args.nras_url,
        nras_jwks_url,
        ..Default::default()
    };
    let certificate_chain = std::fs::read_to_string(args.certificate_path)?;
    let client = nras::NrasVerifierClient::new_with_config(
        &config,
        carbide_instrument::Instrumentation::noop(),
    );

    // The evidence and nonce must come from the same measurement collection.
    let response = client
        .attest(vec![nras::DeviceAttestationInfo {
            device_id: args.device_id,
            evidence: args.evidence,
            certificate: nras::certificate_to_base64(&certificate_chain),
            nonce: args.nonce,
        }])
        .await?;

    // Validate the signature before displaying claims. A valid token can still
    // report warning or contraindicated; HTTP success alone is not an appraisal.
    let keystore =
        nras::NrasKeyStore::new_with_config(&config, &carbide_instrument::Instrumentation::noop())
            .await?;
    let claims = response.validate_and_parse(&keystore, config.validate_jwt_expiry)?;
    println!("{claims:#?}");
    Ok(())
}
