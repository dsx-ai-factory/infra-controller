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

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

//this code was copy and pasted from the implementation of the same struct in sqlx::core,
//and is only necessary for as long as we're optionally validating TLS
#[derive(Debug)]
pub struct DummyTlsVerifier {
    print_warning: bool,
}

impl DummyTlsVerifier {
    pub fn new_for_tests() -> Self {
        Self {
            print_warning: false,
        }
    }

    pub fn new_for_prod() -> Self {
        Self {
            print_warning: true,
        }
    }

    fn warn_if_enabled(&self) {
        if self.print_warning {
            eprintln!(
                "IGNORING SERVER CERT, Please ensure that I am removed to actually validate TLS."
            );
        }
    }
}

impl ServerCertVerifier for DummyTlsVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.warn_if_enabled();
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.warn_if_enabled();
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.warn_if_enabled();
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use rustls::client::danger::ServerCertVerifier;

    use super::*;

    #[test]
    fn exposes_supported_signature_schemes() {
        value_scenarios!(
            run = |for_prod| {
                let verifier = if for_prod {
                    DummyTlsVerifier::new_for_prod()
                } else {
                    DummyTlsVerifier::new_for_tests()
                };
                let schemes = verifier.supported_verify_schemes();
                (
                    schemes.contains(&SignatureScheme::RSA_PKCS1_SHA256),
                    schemes.contains(&SignatureScheme::ED25519),
                )
            };
            "constructors" {
                false => (true, true),
                true => (true, true),
            }
        );
    }
}
