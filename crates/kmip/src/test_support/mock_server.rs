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
//! An in-process KMIP server for tests. It implements Discover Versions,
//! Query, Get Attributes, Encrypt and Decrypt over real AES-256-GCM, requires
//! a client certificate from its own CA, and can be told to misbehave.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::generic_array::{ArrayLength, GenericArray};
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, KeyInit};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use rustls_pki_types::PrivateKeyDer;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::client::{USAGE_MASK_DECRYPT, USAGE_MASK_ENCRYPT};
use crate::config::KmipClientConfig;
use crate::framing::{self, MAX_RESPONSE_BYTES, TAG_REQUEST_MESSAGE};
use crate::wire::{
    Attribute, AttributeName, AttributeValue, AuthenticatedEncryptionTag, BatchCount,
    BlockCipherMode, CryptographicAlgorithm, Data, DecryptResponsePayload,
    DiscoverVersionsResponsePayload, EncryptResponsePayload, GetAttributesResponsePayload,
    IgnoredPayload, IvCounterNonce, ObjectType, Operation, PROTOCOL_VERSION_1_4, ProtocolVersion,
    ProtocolVersionMajor, ProtocolVersionMinor, QueryFunction, QueryResponsePayload, RandomIv,
    RequestMessage, RequestPayload, ResponseBatchItem, ResponseHeader, ResponseMessage,
    ResultMessage, ResultReason, ResultStatus, ServerInformation, State, TimeStamp,
    VendorIdentification,
};

/// Switches for server behaviours the client must tolerate or reject.
#[derive(Clone, Debug)]
pub struct MockBehavior {
    /// Length of the IV the server generates for Encrypt, 12 or 16 bytes,
    /// overriding the request's IV Length the way PyKMIP does. `None` honours
    /// the requested IV Length.
    pub iv_len: Option<usize>,
    /// Leave the IV out of Encrypt responses.
    pub omit_iv: bool,
    /// Leave the authentication tag out of Encrypt responses.
    pub omit_tag: bool,
    /// Close each connection after this many responses.
    pub close_after_responses: Option<usize>,
    /// Sleep this long before writing every response.
    pub response_delay: Option<Duration>,
    /// Refuse every operation with this reason and message.
    pub refuse_with: Option<(ResultReason, &'static str)>,
    /// The operations Query reports.
    pub advertised_operations: Vec<Operation>,
    /// The `(major, minor)` versions Discover Versions reports.
    pub advertised_versions: Vec<(i32, i32)>,
}

impl Default for MockBehavior {
    fn default() -> Self {
        Self {
            iv_len: None,
            omit_iv: false,
            omit_tag: false,
            close_after_responses: None,
            response_delay: None,
            refuse_with: None,
            advertised_operations: vec![
                Operation::Query,
                Operation::DiscoverVersions,
                Operation::GetAttributes,
                Operation::Encrypt,
                Operation::Decrypt,
            ],
            advertised_versions: vec![(1, 4), (1, 3), (1, 2)],
        }
    }
}

/// A symmetric key object held by the mock server.
#[derive(Clone, Debug)]
pub struct MockKey {
    pub material: [u8; 32],
    pub state: State,
    pub algorithm: CryptographicAlgorithm,
    pub length_bits: i32,
    pub usage_mask: i32,
}

impl MockKey {
    /// A fresh, active AES-256 key permitted to encrypt and decrypt.
    pub fn aes256() -> Self {
        Self {
            material: rand::random(),
            state: State::Active,
            algorithm: CryptographicAlgorithm::Aes,
            length_bits: 256,
            usage_mask: USAGE_MASK_ENCRYPT | USAGE_MASK_DECRYPT,
        }
    }
}

#[derive(Default)]
struct Stats {
    connections: AtomicUsize,
    requests: AtomicUsize,
}

struct ServerState {
    behavior: MockBehavior,
    keys: Mutex<HashMap<String, MockKey>>,
    stats: Stats,
}

/// Certificates for one test: a CA, a server identity for `localhost` and
/// `127.0.0.1`, and a client identity issued by the same CA.
struct TestPki {
    ca_pem: String,
    server_cert_der: rustls_pki_types::CertificateDer<'static>,
    server_key_der: Vec<u8>,
    ca_der: rustls_pki_types::CertificateDer<'static>,
    client_cert_pem: String,
    client_key_pem: String,
}

impl TestPki {
    fn generate() -> Self {
        let ca_key = KeyPair::generate().expect("CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "carbide-kmip mock CA");
        let ca_cert = ca_params.self_signed(&ca_key).expect("CA certificate");
        let ca_pem = ca_cert.pem();
        let ca_der = ca_cert.der().clone();
        let issuer = Issuer::new(ca_params, ca_key);

        let server_key = KeyPair::generate().expect("server key");
        let mut server_params =
            CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .expect("server params");
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params
            .signed_by(&server_key, &issuer)
            .expect("server certificate");

        let (client_cert_pem, client_key_pem) = issue_client_identity(&issuer);

        Self {
            ca_pem,
            server_cert_der: server_cert.der().clone(),
            server_key_der: server_key.serialize_der(),
            ca_der,
            client_cert_pem,
            client_key_pem,
        }
    }
}

/// A client certificate and key, as PEM, issued by `issuer`.
fn issue_client_identity(issuer: &Issuer<'_, KeyPair>) -> (String, String) {
    let client_key = KeyPair::generate().expect("client key");
    let mut client_params = CertificateParams::new(Vec::<String>::new()).expect("client params");
    client_params
        .distinguished_name
        .push(DnType::CommonName, "nico-api");
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_cert = client_params
        .signed_by(&client_key, issuer)
        .expect("client certificate");
    (client_cert.pem(), client_key.serialize_pem())
}

/// A running mock KMIP server. Dropping it stops accepting connections.
pub struct MockKmipServer {
    addr: SocketAddr,
    pki: TestPki,
    state: Arc<ServerState>,
    accept_task: JoinHandle<()>,
}

impl Drop for MockKmipServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl MockKmipServer {
    /// Starts a server on an ephemeral loopback port.
    pub async fn start(behavior: MockBehavior) -> Self {
        let pki = TestPki::generate();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut roots = RootCertStore::empty();
        roots.add(pki.ca_der.clone()).expect("mock CA is trusted");
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .expect("client certificate verifier");
        let server_config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS protocol versions")
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![pki.server_cert_der.clone()],
                PrivateKeyDer::Pkcs8(pki.server_key_der.clone().into()),
            )
            .expect("mock server TLS configuration");
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock KMIP listener");
        let addr = listener.local_addr().expect("listener address");
        let state = Arc::new(ServerState {
            behavior,
            keys: Mutex::new(HashMap::new()),
            stats: Stats::default(),
        });
        let accept_task = tokio::spawn(accept_loop(listener, acceptor, state.clone()));
        Self {
            addr,
            pki,
            state,
            accept_task,
        }
    }

    /// `127.0.0.1:port` of the listener.
    pub fn endpoint(&self) -> String {
        self.addr.to_string()
    }

    /// Stores `key` under `unique_identifier`, replacing any previous object.
    pub fn add_key(&self, unique_identifier: &str, key: MockKey) {
        self.keys().insert(unique_identifier.to_string(), key);
    }

    /// Stores a fresh active AES-256 key under `unique_identifier`.
    pub fn add_aes256_key(&self, unique_identifier: &str) -> MockKey {
        let key = MockKey::aes256();
        self.add_key(unique_identifier, key.clone());
        key
    }

    /// Changes a stored key in place, for example to deactivate it.
    pub fn update_key(&self, unique_identifier: &str, update: impl FnOnce(&mut MockKey)) {
        if let Some(key) = self.keys().get_mut(unique_identifier) {
            update(key);
        }
    }

    /// Writes the CA bundle, client certificate and client key into `dir` and
    /// returns a client configuration that reaches this server with them.
    pub fn client_config_in(&self, dir: &Path) -> KmipClientConfig {
        let ca_bundle = dir.join("ca.pem");
        let client_cert = dir.join("client.pem");
        let client_key = dir.join("client.key");
        std::fs::write(&ca_bundle, &self.pki.ca_pem).expect("write CA bundle");
        std::fs::write(&client_cert, &self.pki.client_cert_pem).expect("write client cert");
        std::fs::write(&client_key, &self.pki.client_key_pem).expect("write client key");
        KmipClientConfig::new(self.endpoint(), ca_bundle, client_cert, client_key)
    }

    /// Writes a client certificate and key issued by an unrelated CA into
    /// `dir`, which this server will reject at the TLS handshake.
    pub fn write_untrusted_client_identity(dir: &Path) -> (PathBuf, PathBuf) {
        let ca_key = KeyPair::generate().expect("untrusted CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let issuer = Issuer::new(ca_params, ca_key);
        let (cert_pem, key_pem) = issue_client_identity(&issuer);
        let cert = dir.join("untrusted-client.pem");
        let key = dir.join("untrusted-client.key");
        std::fs::write(&cert, cert_pem).expect("write untrusted cert");
        std::fs::write(&key, key_pem).expect("write untrusted key");
        (cert, key)
    }

    /// TCP connections accepted so far.
    pub fn connections(&self) -> usize {
        self.state.stats.connections.load(Ordering::SeqCst)
    }

    /// Request frames read so far.
    pub fn requests(&self) -> usize {
        self.state.stats.requests.load(Ordering::SeqCst)
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, MockKey>> {
        self.state
            .keys
            .lock()
            .expect("mock key store mutex poisoned; a handler panicked")
    }
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, state: Arc<ServerState>) {
    loop {
        let Ok((tcp, _peer)) = listener.accept().await else {
            return;
        };
        state.stats.connections.fetch_add(1, Ordering::SeqCst);
        let acceptor = acceptor.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                return;
            };
            let mut served = 0;
            loop {
                let Ok(frame) =
                    framing::read_frame(&mut tls, TAG_REQUEST_MESSAGE, MAX_RESPONSE_BYTES).await
                else {
                    return;
                };
                state.stats.requests.fetch_add(1, Ordering::SeqCst);
                let response = state.handle(&frame);
                if let Some(delay) = state.behavior.response_delay {
                    tokio::time::sleep(delay).await;
                }
                if framing::write_frame(&mut tls, &response).await.is_err() {
                    return;
                }
                served += 1;
                if state
                    .behavior
                    .close_after_responses
                    .is_some_and(|limit| served >= limit)
                {
                    tls.shutdown().await.ok();
                    return;
                }
            }
        });
    }
}

impl ServerState {
    fn handle(&self, frame: &[u8]) -> Vec<u8> {
        let request: RequestMessage<RequestPayload> = match kmip_ttlv::from_slice(frame) {
            Ok(request) => request,
            Err(error) => {
                return failure(
                    None,
                    ResultReason::InvalidMessage,
                    &format!("malformed request: {error}"),
                );
            }
        };
        let Some(item) = request.batch_items.into_iter().next() else {
            return failure(
                None,
                ResultReason::InvalidMessage,
                "request carries no batch item",
            );
        };
        let operation = item.operation;
        if let Some((reason, message)) = self.behavior.refuse_with {
            return failure(Some(operation), reason, message);
        }
        match item.payload {
            RequestPayload::DiscoverVersions(payload) => {
                let offered: Vec<ProtocolVersion> = self
                    .behavior
                    .advertised_versions
                    .iter()
                    .map(|(major, minor)| ProtocolVersion {
                        major: ProtocolVersionMajor(*major),
                        minor: ProtocolVersionMinor(*minor),
                    })
                    .filter(|version| {
                        payload
                            .protocol_versions
                            .as_ref()
                            .is_none_or(|wanted| wanted.contains(version))
                    })
                    .collect();
                success(
                    operation,
                    DiscoverVersionsResponsePayload {
                        protocol_versions: (!offered.is_empty()).then_some(offered),
                    },
                )
            }
            RequestPayload::Query(payload) => {
                let wants = |function| payload.query_functions.contains(&function);
                success(
                    operation,
                    QueryResponsePayload {
                        operations: wants(QueryFunction::QueryOperations)
                            .then(|| self.behavior.advertised_operations.clone()),
                        object_types: wants(QueryFunction::QueryObjects)
                            .then(|| vec![ObjectType::SymmetricKey]),
                        vendor_identification: wants(QueryFunction::QueryServerInformation)
                            .then(|| VendorIdentification("carbide-kmip mock server".to_string())),
                        server_information: wants(QueryFunction::QueryServerInformation)
                            .then_some(ServerInformation::default()),
                    },
                )
            }
            RequestPayload::GetAttributes(payload) => {
                let Some(unique_identifier) = payload.unique_identifier else {
                    return failure(
                        Some(operation),
                        ResultReason::MissingData,
                        "a unique identifier is required",
                    );
                };
                let keys = self.lock_keys();
                let Some(key) = keys.get(&unique_identifier.0) else {
                    return failure(
                        Some(operation),
                        ResultReason::ItemNotFound,
                        "object not found",
                    );
                };
                let wanted: Option<Vec<String>> = payload
                    .attribute_names
                    .map(|names| names.into_iter().map(|name| name.0).collect());
                let attributes: Vec<Attribute> = [
                    ("State", AttributeValue::State(key.state)),
                    (
                        "Cryptographic Algorithm",
                        AttributeValue::CryptographicAlgorithm(key.algorithm),
                    ),
                    (
                        "Cryptographic Length",
                        AttributeValue::CryptographicLength(key.length_bits),
                    ),
                    (
                        "Cryptographic Usage Mask",
                        AttributeValue::CryptographicUsageMask(key.usage_mask),
                    ),
                ]
                .into_iter()
                .filter(|(name, _)| {
                    wanted
                        .as_ref()
                        .is_none_or(|wanted| wanted.iter().any(|wanted| wanted == name))
                })
                .map(|(name, value)| Attribute {
                    name: AttributeName(name.to_string()),
                    index: None,
                    value,
                })
                .collect();
                success(
                    operation,
                    GetAttributesResponsePayload {
                        unique_identifier,
                        attributes: (!attributes.is_empty()).then_some(attributes),
                    },
                )
            }
            RequestPayload::Encrypt(payload) => {
                let Some(unique_identifier) = payload.unique_identifier else {
                    return failure(
                        Some(operation),
                        ResultReason::MissingData,
                        "a unique identifier is required",
                    );
                };
                let keys = self.lock_keys();
                let Some(key) = keys.get(&unique_identifier.0) else {
                    return failure(
                        Some(operation),
                        ResultReason::ItemNotFound,
                        "object not found",
                    );
                };
                if key.state != State::Active {
                    return failure(
                        Some(operation),
                        ResultReason::PermissionDenied,
                        "object is not active",
                    );
                }
                if key.usage_mask & USAGE_MASK_ENCRYPT == 0 {
                    return failure(
                        Some(operation),
                        ResultReason::PermissionDenied,
                        "usage mask does not permit encrypt",
                    );
                }
                let parameters = payload.cryptographic_parameters.unwrap_or_default();
                if parameters.block_cipher_mode != Some(BlockCipherMode::Gcm) {
                    return failure(
                        Some(operation),
                        ResultReason::InvalidField,
                        "block cipher mode must be GCM",
                    );
                }
                if parameters.tag_length.is_some_and(|length| length.0 != 16) {
                    return failure(
                        Some(operation),
                        ResultReason::InvalidField,
                        "tag length must be 16 bytes",
                    );
                }
                // KMIP 1.4 section 3.6: IV Length is required for GCM.
                let Some(requested_iv_len) = parameters
                    .iv_length
                    .and_then(|bits| usize::try_from(bits.0).ok())
                    .filter(|bits| bits % 8 == 0)
                    .map(|bits| bits / 8)
                else {
                    return failure(
                        Some(operation),
                        ResultReason::MissingData,
                        "IV length in bits is required for GCM",
                    );
                };
                // The mock's cipher helper supports the two IV sizes real
                // servers produce; anything else is refused, not panicked on.
                if !matches!(requested_iv_len, 12 | 16) {
                    return failure(
                        Some(operation),
                        ResultReason::InvalidField,
                        "IV length must be 96 or 128 bits",
                    );
                }
                let iv = match payload.iv_counter_nonce {
                    Some(iv) => iv.0,
                    None if parameters.random_iv == Some(RandomIv(true)) => {
                        let iv_len = self.behavior.iv_len.unwrap_or(requested_iv_len);
                        rand::random::<[u8; 16]>()[..iv_len].to_vec()
                    }
                    None => {
                        return failure(
                            Some(operation),
                            ResultReason::MissingData,
                            "an IV is required unless Random IV is requested",
                        );
                    }
                };
                let sealed = seal(&key.material, &iv, &payload.data.0);
                let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
                success(
                    operation,
                    EncryptResponsePayload {
                        unique_identifier,
                        data: Data(ciphertext.to_vec()),
                        iv_counter_nonce: (!self.behavior.omit_iv).then_some(IvCounterNonce(iv)),
                        correlation_value: None,
                        authenticated_encryption_tag: (!self.behavior.omit_tag)
                            .then(|| AuthenticatedEncryptionTag(tag.to_vec())),
                    },
                )
            }
            RequestPayload::Decrypt(payload) => {
                let Some(unique_identifier) = payload.unique_identifier else {
                    return failure(
                        Some(operation),
                        ResultReason::MissingData,
                        "a unique identifier is required",
                    );
                };
                let keys = self.lock_keys();
                let Some(key) = keys.get(&unique_identifier.0) else {
                    return failure(
                        Some(operation),
                        ResultReason::ItemNotFound,
                        "object not found",
                    );
                };
                if key.state != State::Active {
                    return failure(
                        Some(operation),
                        ResultReason::PermissionDenied,
                        "object is not active",
                    );
                }
                let (Some(iv), Some(tag)) = (
                    payload.iv_counter_nonce,
                    payload.authenticated_encryption_tag,
                ) else {
                    return failure(
                        Some(operation),
                        ResultReason::MissingData,
                        "an IV and an authentication tag are required",
                    );
                };
                let mut sealed = payload.data.0.clone();
                sealed.extend_from_slice(&tag.0);
                match open(&key.material, &iv.0, &sealed) {
                    Some(plaintext) => success(
                        operation,
                        DecryptResponsePayload {
                            unique_identifier,
                            data: Data(plaintext),
                            correlation_value: None,
                        },
                    ),
                    None => failure(
                        Some(operation),
                        ResultReason::CryptographicFailure,
                        "authentication failed",
                    ),
                }
            }
        }
    }

    fn lock_keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, MockKey>> {
        self.keys
            .lock()
            .expect("mock key store mutex poisoned; a handler panicked")
    }
}

fn header() -> ResponseHeader {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    ResponseHeader {
        protocol_version: PROTOCOL_VERSION_1_4,
        time_stamp: TimeStamp(now),
        nonce: None,
        attestation_types: None,
        client_correlation_value: None,
        server_correlation_value: None,
        batch_count: BatchCount(1),
    }
}

fn success<P: Serialize>(operation: Operation, payload: P) -> Vec<u8> {
    let message = ResponseMessage {
        header: header(),
        batch_items: vec![ResponseBatchItem {
            operation: Some(operation),
            unique_batch_item_id: None,
            result_status: ResultStatus::Success,
            result_reason: None,
            result_message: None,
            asynchronous_correlation_value: None,
            payload: Some(payload),
        }],
    };
    kmip_ttlv::to_vec(&message).expect("mock success response serializes")
}

fn failure(operation: Option<Operation>, reason: ResultReason, message: &str) -> Vec<u8> {
    let message = ResponseMessage::<IgnoredPayload> {
        header: header(),
        batch_items: vec![ResponseBatchItem {
            operation,
            unique_batch_item_id: None,
            result_status: ResultStatus::OperationFailed,
            result_reason: Some(reason),
            result_message: Some(ResultMessage(message.to_string())),
            asynchronous_correlation_value: None,
            payload: None,
        }],
    };
    kmip_ttlv::to_vec(&message).expect("mock failure response serializes")
}

/// AES-256-GCM with a 12- or 16-byte IV; returns ciphertext followed by the
/// 16-byte tag.
fn seal(key: &[u8; 32], iv: &[u8], plaintext: &[u8]) -> Vec<u8> {
    fn with<N: ArrayLength<u8>>(key: &[u8; 32], iv: &[u8], plaintext: &[u8]) -> Vec<u8> {
        AesGcm::<Aes256, N>::new(key.into())
            .encrypt(GenericArray::from_slice(iv), plaintext)
            .expect("AES-GCM seal in the mock KMIP server")
    }
    match iv.len() {
        12 => with::<U12>(key, iv, plaintext),
        16 => with::<U16>(key, iv, plaintext),
        other => panic!("the mock KMIP server supports 12- or 16-byte IVs, not {other}"),
    }
}

/// The inverse of [`seal`]; `None` when authentication fails.
fn open(key: &[u8; 32], iv: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    fn with<N: ArrayLength<u8>>(key: &[u8; 32], iv: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        AesGcm::<Aes256, N>::new(key.into())
            .decrypt(GenericArray::from_slice(iv), sealed)
            .ok()
    }
    match iv.len() {
        12 => with::<U12>(key, iv, sealed),
        16 => with::<U16>(key, iv, sealed),
        _ => None,
    }
}
