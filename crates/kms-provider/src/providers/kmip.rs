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
//! KmipKmsProvider implements KmsBackend over a KMIP 1.4 server that holds a
//! non-exportable AES-256 key per kek_id. NICo generates each DEK locally and
//! sends only the DEK to the server's Encrypt operation; the server returns
//! the ciphertext, an IV it generated and an authentication tag, which are
//! stored together in a versioned encoding and handed back to Decrypt. The
//! KEK never enters NICo configuration or memory.
//!
//! The provider does not create, activate, rotate, revoke or destroy keys.
//! Those stay operator-controlled on the KMIP server; rotation in NICo is a
//! new kek_id mapping plus `secrets re-wrap`, as for every other provider.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use carbide_instrument::red;
use carbide_kmip::{
    AeadCiphertext, CryptographicAlgorithm, KeyAttributes, KmipClient, KmipClientConfig, KmipError,
    Operation, ProtocolVersion, State, USAGE_MASK_DECRYPT, USAGE_MASK_ENCRYPT,
};
use zeroize::Zeroizing;

use crate::{EncryptedDek, KmsBackend, KmsError};

/// The protocol version the client speaks; the server must list it.
const REQUIRED_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 4 };

/// Version byte of the stored DEK encoding, see [`encode_wrapped`].
const ENCODING_VERSION: u8 = 1;

/// Key length the provider requires of every configured key, in bits.
const REQUIRED_KEY_BITS: i32 = 256;

/// A KMS provider backed by one KMIP server.
pub struct KmipKmsProvider {
    client: Arc<KmipClient>,
    /// kek_id to the KMIP Unique Identifier of its key object.
    keys: HashMap<String, String>,
}

impl KmipKmsProvider {
    /// Connects to the server and validates everything the provider relies
    /// on before any DEK is wrapped: the configuration and TLS material
    /// (through [`KmipClient::new`]), that the server offers KMIP 1.4 and
    /// the Encrypt and Decrypt operations, and, for every configured key,
    /// that it is an Active AES-256 key permitted to encrypt and decrypt. A
    /// probe wrap and unwrap of a throwaway value then proves the server
    /// returns an IV and a tag that round-trip. Every failure names the
    /// configuration field it concerns.
    pub async fn connect(
        config: KmipClientConfig,
        keys: HashMap<String, String>,
    ) -> Result<Self, KmsError> {
        if keys.is_empty() {
            return Err(KmsError::Other("keys: no KMIP keys configured".to_string()));
        }
        let client =
            KmipClient::new(config).map_err(|error| KmsError::Other(format_error_chain(&error)))?;
        let provider = Self::new(Arc::new(client), keys);
        provider.validate_server().await?;
        let mut kek_ids: Vec<&String> = provider.keys.keys().collect();
        kek_ids.sort();
        for kek_id in &kek_ids {
            provider
                .validate_key(kek_id, &provider.keys[*kek_id])
                .await?;
        }
        tracing::info!(
            endpoint_address = %provider.client.endpoint(),
            known_keys = ?kek_ids,
            "initialized KMIP KMS provider"
        );
        Ok(provider)
    }

    fn new(client: Arc<KmipClient>, keys: HashMap<String, String>) -> Self {
        Self { client, keys }
    }

    /// Requires KMIP 1.4 and the Encrypt and Decrypt operations.
    async fn validate_server(&self) -> Result<(), KmsError> {
        let endpoint = |detail: String| KmsError::Other(format!("endpoint: {detail}"));
        let versions =
            red::instrumented("kmip", "discover_versions", self.client.discover_versions())
                .await
                .map_err(|error| endpoint(format_error_chain(&error)))?;
        if !versions.contains(&REQUIRED_PROTOCOL_VERSION) {
            let offered: Vec<String> = versions.iter().map(ToString::to_string).collect();
            return Err(endpoint(format!(
                "KMIP server offers protocol versions [{}] but {REQUIRED_PROTOCOL_VERSION} is required",
                offered.join(", ")
            )));
        }
        let query = red::instrumented("kmip", "query", self.client.query_operations())
            .await
            .map_err(|error| endpoint(format_error_chain(&error)))?;
        for required in [Operation::Encrypt, Operation::Decrypt] {
            if !query.operations.contains(&required) {
                return Err(endpoint(format!(
                    "KMIP server does not list the {required} operation"
                )));
            }
        }
        tracing::info!(
            endpoint_address = %self.client.endpoint(),
            vendor_identification = query.vendor_identification.as_deref().unwrap_or("unknown"),
            "KMIP server offers KMIP 1.4 with Encrypt and Decrypt"
        );
        Ok(())
    }

    /// Requires an Active AES-256 key permitted to encrypt and decrypt, and
    /// proves a wrap and unwrap round-trip through it.
    async fn validate_key(&self, kek_id: &str, unique_identifier: &str) -> Result<(), KmsError> {
        let field = format!("keys.{kek_id}");
        let attributes = red::instrumented(
            "kmip",
            "get_attributes",
            self.client.get_key_attributes(unique_identifier),
        )
        .await
        .map_err(|error| {
            if error.is_item_not_found() {
                KmsError::KeyNotFound(format!(
                    "{field}: KMIP object {unique_identifier:?} not found"
                ))
            } else {
                KmsError::Other(format!("{field}: {}", format_error_chain(&error)))
            }
        })?;
        check_key_attributes(&field, &attributes)?;

        let probe: Zeroizing<[u8; 32]> = Zeroizing::new(rand::random());
        let wrapped = self
            .encrypt_dek(kek_id, &probe)
            .await
            .map_err(|error| KmsError::Other(format!("{field}: probe wrap failed: {error}")))?;
        let unwrapped = self
            .decrypt_dek(kek_id, &wrapped)
            .await
            .map_err(|error| KmsError::Other(format!("{field}: probe unwrap failed: {error}")))?;
        if *unwrapped != *probe {
            return Err(KmsError::Other(format!(
                "{field}: probe unwrap did not return the wrapped value"
            )));
        }
        Ok(())
    }

    fn unique_identifier(&self, kek_id: &str) -> Result<&str, KmsError> {
        self.keys
            .get(kek_id)
            .map(String::as_str)
            .ok_or_else(|| KmsError::KeyNotFound(kek_id.to_string()))
    }
}

#[async_trait]
impl KmsBackend for KmipKmsProvider {
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError> {
        let unique_identifier = self.unique_identifier(kek_id)?;
        let sealed = red::instrumented(
            "kmip",
            "encrypt_dek",
            self.client.encrypt(unique_identifier, dek),
        )
        .await
        .map_err(|error| map_error(error, Operation::Encrypt, kek_id, unique_identifier))?;
        encode_wrapped(sealed)
    }

    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError> {
        let unique_identifier = self.unique_identifier(kek_id)?;
        let sealed = decode_wrapped(encrypted)?;
        // The plaintext buffer zeroizes on drop, and the array is filled in
        // place inside its zeroizing wrapper, so no plain copy of the DEK is
        // left behind on the stack.
        let plaintext = red::instrumented(
            "kmip",
            "decrypt_dek",
            self.client.decrypt(unique_identifier, &sealed),
        )
        .await
        .map_err(|error| map_error(error, Operation::Decrypt, kek_id, unique_identifier))?;
        let mut dek = Zeroizing::new([0u8; 32]);
        if plaintext.len() != dek.len() {
            return Err(KmsError::DecryptionFailed(format!(
                "DEK has wrong length: {}",
                plaintext.len()
            )));
        }
        dek.copy_from_slice(&plaintext);
        Ok(dek)
    }

    fn can_decrypt_kek(&self, kek_id: &str) -> bool {
        self.keys.contains_key(kek_id)
    }

    fn kek_ids(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }
}

/// Policy for a configured key: Active, AES, 256 bits, Encrypt and Decrypt
/// usage. A missing attribute is a failure, not a pass.
fn check_key_attributes(field: &str, attributes: &KeyAttributes) -> Result<(), KmsError> {
    let violation = |detail: String| KmsError::Other(format!("{field}: {detail}"));
    match attributes.state {
        Some(State::Active) => {}
        Some(state) => return Err(violation(format!("key state is {state}, expected Active"))),
        None => return Err(violation("server did not report the key state".to_string())),
    }
    match attributes.algorithm {
        Some(CryptographicAlgorithm::Aes) => {}
        Some(algorithm) => {
            return Err(violation(format!(
                "key algorithm is {algorithm}, expected Aes"
            )));
        }
        None => {
            return Err(violation(
                "server did not report the cryptographic algorithm".to_string(),
            ));
        }
    }
    match attributes.length_bits {
        Some(REQUIRED_KEY_BITS) => {}
        Some(bits) => {
            return Err(violation(format!(
                "key length is {bits} bits, expected {REQUIRED_KEY_BITS}"
            )));
        }
        None => {
            return Err(violation(
                "server did not report the cryptographic length".to_string(),
            ));
        }
    }
    match attributes.usage_mask {
        Some(mask) if mask & USAGE_MASK_ENCRYPT != 0 && mask & USAGE_MASK_DECRYPT != 0 => Ok(()),
        Some(mask) => Err(violation(format!(
            "cryptographic usage mask {mask:#x} does not permit both Encrypt and Decrypt"
        ))),
        None => Err(violation(
            "server did not report the cryptographic usage mask".to_string(),
        )),
    }
}

/// Maps a client failure to the stable `KmsError` variants callers handle:
/// an unknown object is `KeyNotFound`; everything else is a failure of the
/// operation that was attempted.
fn map_error(
    error: KmipError,
    operation: Operation,
    kek_id: &str,
    unique_identifier: &str,
) -> KmsError {
    if error.is_item_not_found() {
        return KmsError::KeyNotFound(format!("{kek_id} (KMIP object {unique_identifier:?})"));
    }
    match operation {
        Operation::Decrypt => {
            KmsError::DecryptionFailed(format!("kmip decrypt: {}", format_error_chain(&error)))
        }
        _ => KmsError::EncryptionFailed(format!("kmip encrypt: {}", format_error_chain(&error))),
    }
}

/// Deepest `source()` level [`format_error_chain`] walks, so a cyclic chain
/// cannot hang the caller.
const MAX_ERROR_CHAIN_DEPTH: usize = 16;

/// Renders `error` as `"{error}: {root cause}"`, where the root cause is the
/// deepest `source()` in its chain. The string-typed [`KmsError`] variants
/// would otherwise keep only the top-level message and lose, for example, the
/// certificate verification failure beneath a connect error. The root cause is
/// omitted when the top-level message already includes it.
fn format_error_chain(error: &dyn std::error::Error) -> String {
    let top = error.to_string();
    let root = std::iter::successors(error.source(), |cause| cause.source())
        .take(MAX_ERROR_CHAIN_DEPTH)
        .last()
        .map(ToString::to_string);
    match root {
        Some(root) if !top.contains(&root) => format!("{top}: {root}"),
        _ => top,
    }
}

/// Stores an AEAD result as an `EncryptedDek`: `nonce` holds the server's IV
/// verbatim and `ciphertext` holds `[version 1][tag length][tag][ciphertext]`,
/// so every field authenticated decryption needs survives the journal and an
/// unknown version is refused instead of misread.
fn encode_wrapped(sealed: AeadCiphertext) -> Result<EncryptedDek, KmsError> {
    let tag_len = u8::try_from(sealed.tag.len())
        .ok()
        .filter(|len| *len != 0)
        .ok_or_else(|| {
            KmsError::EncryptionFailed(format!(
                "KMIP authentication tag of {} bytes does not fit the DEK encoding",
                sealed.tag.len()
            ))
        })?;
    if sealed.iv.is_empty() {
        return Err(KmsError::EncryptionFailed(
            "KMIP server returned an empty IV".to_string(),
        ));
    }
    let mut ciphertext = Vec::with_capacity(2 + sealed.tag.len() + sealed.ciphertext.len());
    ciphertext.push(ENCODING_VERSION);
    ciphertext.push(tag_len);
    ciphertext.extend_from_slice(&sealed.tag);
    ciphertext.extend_from_slice(&sealed.ciphertext);
    Ok(EncryptedDek {
        ciphertext,
        nonce: sealed.iv,
    })
}

/// The inverse of [`encode_wrapped`].
fn decode_wrapped(encrypted: &EncryptedDek) -> Result<AeadCiphertext, KmsError> {
    let invalid = |detail: &str| KmsError::DecryptionFailed(format!("KMIP DEK encoding: {detail}"));
    let [version, tag_len, rest @ ..] = encrypted.ciphertext.as_slice() else {
        return Err(invalid("blob is shorter than its header"));
    };
    if *version != ENCODING_VERSION {
        return Err(KmsError::DecryptionFailed(format!(
            "unsupported KMIP DEK encoding version {version}"
        )));
    }
    let tag_len = usize::from(*tag_len);
    if tag_len == 0 {
        return Err(invalid("tag length is zero"));
    }
    if rest.len() < tag_len {
        return Err(invalid("blob is shorter than its tag"));
    }
    if encrypted.nonce.is_empty() {
        return Err(invalid("IV is empty"));
    }
    let (tag, ciphertext) = rest.split_at(tag_len);
    Ok(AeadCiphertext {
        iv: encrypted.nonce.clone(),
        ciphertext: ciphertext.to_vec(),
        tag: tag.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use carbide_kmip::test_support::{EncryptFault, MockBehavior, MockKey, MockKmipServer};
    use carbide_kmip::{
        DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, MAX_REQUEST_TIMEOUT, ResultReason,
    };
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use tempfile::TempDir;

    use super::*;

    const KEK: &str = "kek-1";
    const UID: &str = "kmip-object-1";

    fn sealed(iv_len: usize) -> AeadCiphertext {
        AeadCiphertext {
            iv: vec![0x11; iv_len],
            ciphertext: vec![0x22; 32],
            tag: vec![0x33; 16],
        }
    }

    /// A mock server holding `UID` as an active AES-256 key, with the client
    /// configuration that reaches it written into `dir`.
    async fn server_and_config(
        behavior: MockBehavior,
    ) -> (MockKmipServer, TempDir, KmipClientConfig) {
        let server = MockKmipServer::start(behavior).await;
        server.add_aes256_key(UID);
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = server.client_config_in(dir.path());
        (server, dir, config)
    }

    fn keys() -> HashMap<String, String> {
        HashMap::from([(KEK.to_string(), UID.to_string())])
    }

    fn variant(error: &KmsError) -> &'static str {
        match error {
            KmsError::KeyNotFound(_) => "KeyNotFound",
            KmsError::EncryptionFailed(_) => "EncryptionFailed",
            KmsError::DecryptionFailed(_) => "DecryptionFailed",
            KmsError::Other(_) => "Other",
        }
    }

    /// The first `name` found on `PATH`, like the Transit tests' search for
    /// the `vault` binary.
    fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    /// Waits until `port` accepts a TCP connection or `child` exits.
    async fn wait_for_listener(
        port: u16,
        child: &mut tokio::process::Child,
        log: &std::path::Path,
    ) {
        for _ in 0..120 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "pykmip-server exited with {status} before listening; log:\n{}",
                    std::fs::read_to_string(log).unwrap_or_default()
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!(
            "pykmip-server did not listen on port {port} within 30s; log:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
    }

    // Verifies the provider end to end against PyKMIP, the reference software
    // KMIP server: the key is created and activated through PyKMIP's own
    // client under the same certificate identity the provider uses, then
    // connect-time validation, wrap and unwrap run through the Rust provider.
    // Skipped when `pykmip-server` is not on PATH, like the Transit tests
    // without `vault`; the server's 16-byte IVs also cover the verbatim IV
    // storage against a real implementation.
    #[tokio::test]
    async fn pykmip_round_trip() {
        let Some(server_binary) = find_on_path("pykmip-server") else {
            eprintln!("pykmip-server not available, skipping test");
            return;
        };
        let dir = tempfile::tempdir().expect("temporary directory");
        let pki = carbide_kmip::test_support::write_test_pki(dir.path());
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("free port")
            .port();
        let policies = dir.path().join("policies");
        std::fs::create_dir(&policies).expect("policy directory");
        let config_path = dir.path().join("server.conf");
        std::fs::write(
            &config_path,
            format!(
                "[server]\nhostname=127.0.0.1\nport={port}\ncertificate_path={}\nkey_path={}\n\
                 ca_path={}\nauth_suite=TLS1.2\npolicy_path={}\nenable_tls_client_auth=True\n\
                 logging_level=INFO\ndatabase_path={}\n",
                pki.server_cert.display(),
                pki.server_key.display(),
                pki.ca.display(),
                policies.display(),
                dir.path().join("pykmip.db").display(),
            ),
        )
        .expect("write server config");
        let log_path = dir.path().join("pykmip.log");
        let mut server = tokio::process::Command::new(&server_binary)
            .arg("-f")
            .arg(&config_path)
            .arg("-l")
            .arg(&log_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start pykmip-server");
        wait_for_listener(port, &mut server, &log_path).await;

        // Create and activate the KEK as the identity the provider will use,
        // through PyKMIP's own client: the Rust client deliberately has no
        // Create or Activate.
        let python = server_binary
            .parent()
            .map(|bin| bin.join("python"))
            .filter(|python| python.is_file())
            .unwrap_or_else(|| std::path::PathBuf::from("python3"));
        let script = [
            "from kmip.pie.client import ProxyKmipClient".to_string(),
            "from kmip.core import enums".to_string(),
            format!(
                "client = ProxyKmipClient(hostname='127.0.0.1', port={port}, cert='{}', key='{}', \
                 ca='{}', config_file=None, kmip_version=enums.KMIPVersion.KMIP_1_4)",
                pki.client_cert.display(),
                pki.client_key.display(),
                pki.ca.display(),
            ),
            "with client:".to_string(),
            "    uid = client.create(enums.CryptographicAlgorithm.AES, 256, \
             cryptographic_usage_mask=[enums.CryptographicUsageMask.ENCRYPT, \
             enums.CryptographicUsageMask.DECRYPT])"
                .to_string(),
            "    client.activate(uid)".to_string(),
            "print(uid)".to_string(),
        ]
        .join("\n");
        let created = tokio::process::Command::new(python)
            .arg("-c")
            .arg(script)
            .output()
            .await
            .expect("run the PyKMIP client");
        assert!(
            created.status.success(),
            "key creation failed: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        let unique_identifier = String::from_utf8_lossy(&created.stdout).trim().to_string();

        let config = KmipClientConfig {
            endpoint: format!("127.0.0.1:{port}"),
            server_name: None,
            ca_bundle: pki.ca,
            client_cert: pki.client_cert,
            client_key: pki.client_key,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        };
        let provider = KmipKmsProvider::connect(
            config,
            HashMap::from([(KEK.to_string(), unique_identifier)]),
        )
        .await
        .expect("connect to pykmip");
        let (dek, wrapped) = provider.generate_and_wrap_dek(KEK).await.expect("wrap");
        assert_eq!(wrapped.nonce.len(), 16, "PyKMIP generates block-size IVs");
        let unwrapped = provider.decrypt_dek(KEK, &wrapped).await.expect("unwrap");
        assert_eq!(*unwrapped, *dek);
        server.kill().await.ok();
    }

    // Verifies the persisted DEK encoding: a round trip restores every AEAD
    // field, and a blob from an unknown version or without a complete tag is
    // refused rather than fed to the server.
    #[test]
    fn dek_encoding_round_trips_and_rejects_malformed_blobs() {
        let valid = encode_wrapped(sealed(12)).expect("encode");
        let sixteen_byte_iv = encode_wrapped(sealed(16)).expect("encode");
        let mut future_version = encode_wrapped(sealed(12)).expect("encode");
        future_version.ciphertext[0] = 2;
        let mut zero_tag = encode_wrapped(sealed(12)).expect("encode");
        zero_tag.ciphertext[1] = 0;
        let mut truncated = encode_wrapped(sealed(12)).expect("encode");
        truncated.ciphertext.truncate(10);
        let header_only = EncryptedDek {
            ciphertext: vec![1],
            nonce: vec![0x11; 12],
        };
        let no_iv = EncryptedDek {
            nonce: vec![],
            ..encode_wrapped(sealed(12)).expect("encode")
        };
        scenarios!(run = |encrypted: EncryptedDek| decode_wrapped(&encrypted).map_err(|error| error.to_string());
            "a stored blob decodes to the fields the server produced" {
                valid => Yields(sealed(12)),
                sixteen_byte_iv => Yields(sealed(16)),
            }
            "malformed blobs are refused" {
                future_version => FailsWith("decryption failed: unsupported KMIP DEK encoding version 2".to_string()),
                zero_tag => FailsWith("decryption failed: KMIP DEK encoding: tag length is zero".to_string()),
                truncated => FailsWith("decryption failed: KMIP DEK encoding: blob is shorter than its tag".to_string()),
                header_only => FailsWith("decryption failed: KMIP DEK encoding: blob is shorter than its header".to_string()),
                no_iv => FailsWith("decryption failed: KMIP DEK encoding: IV is empty".to_string()),
            }
        );
    }

    // Verifies the full provider contract against the mock: connect-time
    // validation passes for a qualified server and key, the default
    // generate-and-wrap path round-trips through decrypt, and the provider
    // answers for exactly its configured kek_ids.
    #[tokio::test]
    async fn wrap_then_unwrap_round_trips_through_the_mock_server() {
        let (_server, _dir, config) = server_and_config(MockBehavior::default()).await;
        let provider = KmipKmsProvider::connect(config, keys())
            .await
            .expect("connect");

        let (dek, wrapped) = provider.generate_and_wrap_dek(KEK).await.expect("wrap");
        assert_eq!(wrapped.ciphertext[0], ENCODING_VERSION);
        assert_eq!(wrapped.nonce.len(), 12, "the server IV is stored verbatim");
        let unwrapped = provider.decrypt_dek(KEK, &wrapped).await.expect("unwrap");
        assert_eq!(*unwrapped, *dek);

        assert!(provider.can_decrypt_kek(KEK));
        assert!(!provider.can_decrypt_kek("other"));
        assert_eq!(provider.kek_ids(), vec![KEK.to_string()]);
    }

    // Verifies the error mapping callers depend on: an unknown kek_id or KMIP
    // object is KeyNotFound, and every other failure is attributed to the
    // operation that was attempted, so the journal code reports wrap and
    // unwrap failures distinctly.
    #[tokio::test]
    async fn client_errors_map_to_stable_kms_variants() {
        #[derive(Clone, Copy)]
        enum Call {
            Encrypt,
            Decrypt,
        }
        struct Case {
            scenario: &'static str,
            behavior: MockBehavior,
            kek_id: &'static str,
            unique_identifier: &'static str,
            call: Call,
            expect: &'static str,
        }
        let refused = MockBehavior {
            refuse_with: Some((ResultReason::PermissionDenied, "denied")),
            ..MockBehavior::default()
        };
        let cases = [
            Case {
                scenario: "a kek_id this provider does not hold",
                behavior: MockBehavior::default(),
                kek_id: "unknown",
                unique_identifier: UID,
                call: Call::Encrypt,
                expect: "KeyNotFound",
            },
            Case {
                scenario: "a KMIP object the server does not hold",
                behavior: MockBehavior::default(),
                kek_id: KEK,
                unique_identifier: "missing",
                call: Call::Encrypt,
                expect: "KeyNotFound",
            },
            Case {
                scenario: "a refused encrypt",
                behavior: refused.clone(),
                kek_id: KEK,
                unique_identifier: UID,
                call: Call::Encrypt,
                expect: "EncryptionFailed",
            },
            Case {
                scenario: "a refused decrypt",
                behavior: refused,
                kek_id: KEK,
                unique_identifier: UID,
                call: Call::Decrypt,
                expect: "DecryptionFailed",
            },
        ];
        for case in cases {
            let server = MockKmipServer::start(case.behavior).await;
            server.add_aes256_key(UID);
            let dir = tempfile::tempdir().expect("temporary directory");
            let client = KmipClient::new(server.client_config_in(dir.path())).expect("client");
            let provider = KmipKmsProvider::new(
                Arc::new(client),
                HashMap::from([(KEK.to_string(), case.unique_identifier.to_string())]),
            );
            let result = match case.call {
                Call::Encrypt => provider
                    .encrypt_dek(case.kek_id, &[7u8; 32])
                    .await
                    .map(drop),
                Call::Decrypt => {
                    let wrapped = encode_wrapped(sealed(12)).expect("encode");
                    provider.decrypt_dek(case.kek_id, &wrapped).await.map(drop)
                }
            };
            let error = result.expect_err(case.scenario);
            assert_eq!(variant(&error), case.expect, "{}: {error}", case.scenario);
        }
    }

    // Verifies that connect refuses an unqualified server or key before any
    // DEK is wrapped, naming the configuration field at fault and keeping the
    // root cause of a sourced failure (such as the certificate error beneath
    // a failed connect), so a bad deployment fails the boot with a
    // diagnosable message.
    #[tokio::test]
    async fn connect_rejects_unqualified_servers_and_keys_naming_the_field() {
        struct Case {
            scenario: &'static str,
            behavior: MockBehavior,
            key: fn(&mut MockKey),
            keys: HashMap<String, String>,
            config: fn(&mut KmipClientConfig),
            expect_variant: &'static str,
            expect_contains: &'static [&'static str],
        }
        fn unchanged(_: &mut MockKey) {}
        fn as_is(_: &mut KmipClientConfig) {}
        let cases = [
            Case {
                scenario: "server without KMIP 1.4",
                behavior: MockBehavior {
                    advertised_versions: vec![(1, 2), (1, 3)],
                    ..MockBehavior::default()
                },
                key: unchanged,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["endpoint:", "1.4"],
            },
            Case {
                scenario: "server without the Encrypt operation",
                behavior: MockBehavior {
                    advertised_operations: vec![
                        Operation::Query,
                        Operation::DiscoverVersions,
                        Operation::GetAttributes,
                        Operation::Decrypt,
                    ],
                    ..MockBehavior::default()
                },
                key: unchanged,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["endpoint:", "Encrypt"],
            },
            Case {
                scenario: "key that is not active",
                behavior: MockBehavior::default(),
                key: |key| key.state = State::Deactivated,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["keys.kek-1:", "Deactivated"],
            },
            Case {
                scenario: "key that is not 256 bits",
                behavior: MockBehavior::default(),
                key: |key| key.length_bits = 128,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["keys.kek-1:", "128"],
            },
            Case {
                scenario: "key without decrypt usage",
                behavior: MockBehavior::default(),
                key: |key| key.usage_mask = USAGE_MASK_ENCRYPT,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["keys.kek-1:", "Decrypt"],
            },
            Case {
                scenario: "server that returns no IV",
                behavior: MockBehavior {
                    encrypt_fault: Some(EncryptFault::OmitIv),
                    ..MockBehavior::default()
                },
                key: unchanged,
                keys: keys(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["keys.kek-1:", "probe wrap failed"],
            },
            Case {
                scenario: "unique identifier the server does not hold",
                behavior: MockBehavior::default(),
                key: unchanged,
                keys: HashMap::from([(KEK.to_string(), "missing".to_string())]),
                config: as_is,
                expect_variant: "KeyNotFound",
                expect_contains: &["keys.kek-1:", "\"missing\""],
            },
            Case {
                scenario: "no keys at all",
                behavior: MockBehavior::default(),
                key: unchanged,
                keys: HashMap::new(),
                config: as_is,
                expect_variant: "Other",
                expect_contains: &["keys:"],
            },
            Case {
                scenario: "request timeout above the hard maximum",
                behavior: MockBehavior::default(),
                key: unchanged,
                keys: keys(),
                config: |config| {
                    config.request_timeout = MAX_REQUEST_TIMEOUT + Duration::from_secs(1);
                },
                expect_variant: "Other",
                expect_contains: &["request_timeout"],
            },
            Case {
                scenario: "CA bundle file that does not exist",
                behavior: MockBehavior::default(),
                key: unchanged,
                keys: keys(),
                config: |config| config.ca_bundle = PathBuf::from("/nonexistent/kmip-ca.pem"),
                expect_variant: "Other",
                expect_contains: &["ca_bundle", "No such file or directory"],
            },
            Case {
                scenario: "server name the certificate does not match",
                behavior: MockBehavior::default(),
                key: unchanged,
                keys: keys(),
                config: |config| config.server_name = Some("wrong.example".to_string()),
                expect_variant: "Other",
                expect_contains: &["endpoint:", "not valid for name"],
            },
        ];
        for case in cases {
            let (server, _dir, mut config) = server_and_config(case.behavior).await;
            server.update_key(UID, case.key);
            (case.config)(&mut config);
            let error = KmipKmsProvider::connect(config, case.keys)
                .await
                .err()
                .unwrap_or_else(|| panic!("{}: connect must fail", case.scenario));
            assert_eq!(
                variant(&error),
                case.expect_variant,
                "{}: {error}",
                case.scenario
            );
            let message = error.to_string();
            for expected in case.expect_contains {
                assert!(
                    message.contains(expected),
                    "{}: {message:?} lacks {expected:?}",
                    case.scenario
                );
            }
        }
    }
}
