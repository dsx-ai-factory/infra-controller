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
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

use crate::client::{USAGE_MASK_DECRYPT, USAGE_MASK_ENCRYPT};
use crate::config::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, KmipClientConfig};
use crate::framing::{self, MAX_RESPONSE_BYTES, TAG_REQUEST_MESSAGE, TAG_RESPONSE_MESSAGE};
use crate::wire::{
    Attribute, AttributeName, AttributeValue, AuthenticatedEncryptionTag, BatchCount,
    BlockCipherMode, CryptographicAlgorithm, Data, DecryptRequestPayload, DecryptResponsePayload,
    DiscoverVersionsRequestPayload, DiscoverVersionsResponsePayload, EncryptRequestPayload,
    EncryptResponsePayload, GetAttributesRequestPayload, GetAttributesResponsePayload,
    IgnoredPayload, IvCounterNonce, ObjectType, Operation, PROTOCOL_VERSION_1_4, ProtocolVersion,
    ProtocolVersionMajor, ProtocolVersionMinor, QueryFunction, QueryRequestPayload,
    QueryResponsePayload, RandomIv, RequestMessage, RequestPayload, ResponseBatchItem,
    ResponseHeader, ResponseMessage, ResultMessage, ResultReason, ResultStatus, ServerInformation,
    State, TimeStamp, UniqueBatchItemId, UniqueIdentifier, VendorIdentification,
};

/// A way the server can corrupt an otherwise successful Encrypt response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptFault {
    /// Leave the IV out.
    OmitIv,
    /// Send a zero-length IV.
    EmptyIv,
    /// Leave the authentication tag out.
    OmitTag,
    /// Send a zero-length authentication tag.
    EmptyTag,
    /// Send only the first 8 bytes of the authentication tag.
    ShortTag,
    /// Append one extra byte to the ciphertext.
    LongCiphertext,
}

/// A way the server can answer with a response that does not match the
/// request it is answering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseFault {
    /// Name a different object in every response that names one.
    WrongUniqueIdentifier,
    /// Leave the request's Unique Batch Item ID out of the response.
    OmitBatchItemId,
    /// Declare protocol version 1.2 in the response header.
    WrongProtocolVersion,
}

/// Switches for server behaviors the client must tolerate or reject.
#[derive(Clone, Debug)]
pub struct MockBehavior {
    /// Length of the IV the server generates for Encrypt, 12 or 16 bytes,
    /// overriding the request's IV Length the way PyKMIP 0.11.0 does: it
    /// generates a block-size IV for AES whatever length was requested.
    /// `None` honors the requested IV Length.
    pub iv_len: Option<usize>,
    /// Corrupt every Encrypt response this way.
    pub encrypt_fault: Option<EncryptFault>,
    /// Make every response disagree with its request this way.
    pub response_fault: Option<ResponseFault>,
    /// Close each connection after this many responses on it.
    pub close_after_responses: Option<usize>,
    /// Stop answering after this many responses in total: every later request
    /// closes its connection without a reply.
    pub answer_at_most: Option<usize>,
    /// After this many responses in total, write only the first four bytes of
    /// each later response and close the connection.
    pub partial_responses_after: Option<usize>,
    /// Sleep this long before writing a response.
    pub response_delay: Option<Duration>,
    /// Which responses, counted from zero across all connections,
    /// `response_delay` applies to; `None` delays every response.
    pub delayed_responses: Option<Range<usize>>,
    /// Answer every request with a header announcing a message larger than
    /// the client accepts, then close the connection.
    pub oversized_response: bool,
    /// Refuse every operation with this reason and message.
    pub refuse_with: Option<(ResultReason, &'static str)>,
    /// The operations Query reports; must not contain [`Operation::Other`],
    /// which has no wire value.
    pub advertised_operations: Vec<Operation>,
    /// The `(major, minor)` versions Discover Versions reports.
    pub advertised_versions: Vec<(i32, i32)>,
}

impl Default for MockBehavior {
    fn default() -> Self {
        Self {
            iv_len: None,
            encrypt_fault: None,
            response_fault: None,
            close_after_responses: None,
            answer_at_most: None,
            partial_responses_after: None,
            response_delay: None,
            delayed_responses: None,
            oversized_response: false,
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
    /// The 256-bit key material used for AES-GCM.
    pub material: [u8; 32],
    /// The reported State; only an `Active` key encrypts and decrypts.
    pub state: State,
    /// The reported Cryptographic Algorithm; the server encrypts with AES
    /// regardless.
    pub algorithm: CryptographicAlgorithm,
    /// The reported Cryptographic Length in bits; the material is 256 bits
    /// regardless.
    pub length_bits: i32,
    /// The Cryptographic Usage Mask; Encrypt and Decrypt are refused without
    /// their bits.
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
    /// Requests the server decided to answer, across all connections, whether
    /// or not the response was then delayed, cut short or lost.
    answers: AtomicUsize,
    handler_panics: AtomicUsize,
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
    ca_der: rustls_pki_types::CertificateDer<'static>,
    server_cert_der: rustls_pki_types::CertificateDer<'static>,
    server_key_der: Vec<u8>,
    server_cert_pem: String,
    server_key_pem: String,
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
            ca_der,
            server_cert_der: server_cert.der().clone(),
            server_key_der: server_key.serialize_der(),
            server_cert_pem: server_cert.pem(),
            server_key_pem: server_key.serialize_pem(),
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

/// PEM files for a freshly generated CA, a server identity for `localhost`
/// and `127.0.0.1`, and a client identity with common name `nico-api`, for
/// tests that run a real KMIP server such as PyKMIP. A second set written
/// elsewhere is an identity the mock server, and clients trusting the first
/// CA, reject.
pub struct TestPkiFiles {
    /// The CA certificate, as a one-entry bundle.
    pub ca: PathBuf,
    /// The server certificate.
    pub server_cert: PathBuf,
    /// The server private key.
    pub server_key: PathBuf,
    /// The client certificate.
    pub client_cert: PathBuf,
    /// The client private key.
    pub client_key: PathBuf,
}

/// Writes a fresh test PKI into `dir` and returns the file paths.
pub fn write_test_pki(dir: &Path) -> TestPkiFiles {
    let pki = TestPki::generate();
    let files = TestPkiFiles {
        ca: dir.join("ca.pem"),
        server_cert: dir.join("server.pem"),
        server_key: dir.join("server.key"),
        client_cert: dir.join("client.pem"),
        client_key: dir.join("client.key"),
    };
    std::fs::write(&files.ca, &pki.ca_pem).expect("write CA bundle");
    std::fs::write(&files.server_cert, &pki.server_cert_pem).expect("write server cert");
    std::fs::write(&files.server_key, &pki.server_key_pem).expect("write server key");
    std::fs::write(&files.client_cert, &pki.client_cert_pem).expect("write client cert");
    std::fs::write(&files.client_key, &pki.client_key_pem).expect("write client key");
    files
}

/// A running mock KMIP server. Dropping it stops accepting connections and
/// aborts the connections it is serving.
pub struct MockKmipServer {
    addr: SocketAddr,
    pki: TestPki,
    state: Arc<ServerState>,
    accept_task: JoinHandle<()>,
}

impl Drop for MockKmipServer {
    fn drop(&mut self) {
        self.accept_task.abort();
        let panics = self.state.stats.handler_panics.load(Ordering::SeqCst);
        if panics > 0 && !std::thread::panicking() {
            panic!("{panics} mock KMIP server connection task(s) panicked");
        }
    }
}

impl MockKmipServer {
    /// Starts a server on an ephemeral loopback port.
    pub async fn start(behavior: MockBehavior) -> Self {
        assert!(
            !behavior.advertised_operations.contains(&Operation::Other),
            "Operation::Other has no wire value and cannot be advertised"
        );
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
        self.state
            .lock_keys()
            .insert(unique_identifier.to_string(), key);
    }

    /// Stores a fresh active AES-256 key under `unique_identifier`.
    pub fn add_aes256_key(&self, unique_identifier: &str) -> MockKey {
        let key = MockKey::aes256();
        self.add_key(unique_identifier, key.clone());
        key
    }

    /// Changes a stored key in place, for example to deactivate it. Panics
    /// when no key is stored under `unique_identifier`, so a mistyped
    /// identifier cannot leave a test silently exercising the wrong state.
    pub fn update_key(&self, unique_identifier: &str, update: impl FnOnce(&mut MockKey)) {
        let mut keys = self.state.lock_keys();
        let key = keys
            .get_mut(unique_identifier)
            .unwrap_or_else(|| panic!("the mock KMIP server holds no key {unique_identifier:?}"));
        update(key);
    }

    /// Writes the CA bundle, client certificate and client key into `dir` and
    /// returns a client configuration that reaches this server with them and
    /// the default deadlines.
    pub fn client_config_in(&self, dir: &Path) -> KmipClientConfig {
        let ca_bundle = dir.join("ca.pem");
        let client_cert = dir.join("client.pem");
        let client_key = dir.join("client.key");
        std::fs::write(&ca_bundle, &self.pki.ca_pem).expect("write CA bundle");
        std::fs::write(&client_cert, &self.pki.client_cert_pem).expect("write client cert");
        std::fs::write(&client_key, &self.pki.client_key_pem).expect("write client key");
        KmipClientConfig {
            endpoint: self.endpoint(),
            server_name: None,
            ca_bundle,
            client_cert,
            client_key,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// TCP connections accepted so far.
    pub fn connections(&self) -> usize {
        self.state.stats.connections.load(Ordering::SeqCst)
    }

    /// Request frames read so far.
    pub fn requests(&self) -> usize {
        self.state.stats.requests.load(Ordering::SeqCst)
    }
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, state: Arc<ServerState>) {
    // Connection tasks live in a set owned by this task, so aborting the
    // accept task (which `MockKmipServer::drop` does) aborts every connection
    // instead of leaving detached tasks holding sockets and server state.
    // Finished tasks are reaped at each accept so the set does not grow with
    // the connection count; panics are counted by the tasks themselves.
    let mut connections = JoinSet::new();
    loop {
        let tcp = match listener.accept().await {
            Ok((tcp, _peer)) => tcp,
            Err(_transient) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        while connections.try_join_next().is_some() {}
        state.stats.connections.fetch_add(1, Ordering::SeqCst);
        connections.spawn(serve_connection(tcp, acceptor.clone(), state.clone()));
    }
}

/// Answers requests on one connection until the client or a behavior switch
/// closes it.
async fn serve_connection(tcp: TcpStream, acceptor: TlsAcceptor, state: Arc<ServerState>) {
    let _panics = PanicCounter(&state.stats.handler_panics);
    let Ok(mut tls) = acceptor.accept(tcp).await else {
        return;
    };
    let behavior = &state.behavior;
    let mut served = 0;
    loop {
        let Ok(frame) =
            framing::read_frame(&mut tls, TAG_REQUEST_MESSAGE, MAX_RESPONSE_BYTES).await
        else {
            return;
        };
        state.stats.requests.fetch_add(1, Ordering::SeqCst);
        let answered = state.stats.answers.load(Ordering::SeqCst);
        if behavior
            .answer_at_most
            .is_some_and(|limit| answered >= limit)
        {
            tls.shutdown().await.ok();
            return;
        }
        if behavior.oversized_response {
            framing::write_frame(&mut tls, &oversized_header())
                .await
                .ok();
            tls.shutdown().await.ok();
            return;
        }
        let response = state.handle(&frame);
        state.stats.answers.fetch_add(1, Ordering::SeqCst);
        if let Some(delay) = behavior.response_delay
            && behavior
                .delayed_responses
                .as_ref()
                .is_none_or(|delayed| delayed.contains(&answered))
        {
            tokio::time::sleep(delay).await;
        }
        if behavior
            .partial_responses_after
            .is_some_and(|limit| answered >= limit)
        {
            framing::write_frame(&mut tls, &response[..4]).await.ok();
            tls.shutdown().await.ok();
            return;
        }
        if framing::write_frame(&mut tls, &response).await.is_err() {
            return;
        }
        served += 1;
        if behavior
            .close_after_responses
            .is_some_and(|limit| served >= limit)
        {
            tls.shutdown().await.ok();
            return;
        }
    }
}

/// Counts the panic of the connection task it lives in, as that task unwinds,
/// so `MockKmipServer::drop` reports it even when no later accept reaped the
/// task. An aborted task drops it without a panic in progress.
struct PanicCounter<'a>(&'a AtomicUsize);

impl Drop for PanicCounter<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A response header announcing a message one header longer than the client
/// accepts.
fn oversized_header() -> Vec<u8> {
    let mut header = TAG_RESPONSE_MESSAGE.to_vec();
    header.push(0x01);
    header.extend_from_slice(&MAX_RESPONSE_BYTES.to_be_bytes());
    header
}

/// The context one request is answered in: the behavior switches and what
/// the response must echo.
struct Reply<'a> {
    behavior: &'a MockBehavior,
    operation: Option<Operation>,
    batch_item_id: Option<UniqueBatchItemId>,
}

impl Reply<'_> {
    fn success<P: Serialize>(&self, payload: P) -> Vec<u8> {
        self.encode(ResultStatus::Success, None, None, Some(payload))
    }

    fn failure(&self, reason: ResultReason, message: &str) -> Vec<u8> {
        self.encode::<IgnoredPayload>(
            ResultStatus::OperationFailed,
            Some(reason),
            Some(ResultMessage(message.to_string())),
            None,
        )
    }

    fn encode<P: Serialize>(
        &self,
        status: ResultStatus,
        reason: Option<ResultReason>,
        message: Option<ResultMessage>,
        payload: Option<P>,
    ) -> Vec<u8> {
        let protocol_version = match self.behavior.response_fault {
            Some(ResponseFault::WrongProtocolVersion) => ProtocolVersion {
                major: ProtocolVersionMajor(1),
                minor: ProtocolVersionMinor(2),
            },
            _ => PROTOCOL_VERSION_1_4,
        };
        let unique_batch_item_id = match self.behavior.response_fault {
            Some(ResponseFault::OmitBatchItemId) => None,
            _ => self.batch_item_id.clone(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or_default();
        let response = ResponseMessage {
            header: ResponseHeader {
                protocol_version,
                time_stamp: TimeStamp(now),
                nonce: None,
                attestation_types: None,
                client_correlation_value: None,
                server_correlation_value: None,
                batch_count: BatchCount(1),
            },
            batch_items: vec![ResponseBatchItem {
                operation: self.operation,
                unique_batch_item_id,
                result_status: status,
                result_reason: reason,
                result_message: message,
                asynchronous_correlation_value: None,
                payload,
                message_extension: None,
            }],
        };
        kmip_ttlv::to_vec(&response).expect("mock response serializes")
    }

    /// The Unique Identifier to echo for the object a request named.
    fn echo(&self, object: UniqueIdentifier) -> UniqueIdentifier {
        match self.behavior.response_fault {
            Some(ResponseFault::WrongUniqueIdentifier) => {
                UniqueIdentifier(format!("{}-other", object.0))
            }
            _ => object,
        }
    }
}

impl ServerState {
    fn handle(&self, frame: &[u8]) -> Vec<u8> {
        let mut reply = Reply {
            behavior: &self.behavior,
            operation: None,
            batch_item_id: None,
        };
        let request: RequestMessage<RequestPayload> = match kmip_ttlv::from_slice(frame) {
            Ok(request) => request,
            Err(error) => {
                return reply.failure(
                    ResultReason::InvalidMessage,
                    &format!("malformed request: {error}"),
                );
            }
        };
        let Some(item) = request.batch_items.into_iter().next() else {
            return reply.failure(
                ResultReason::InvalidMessage,
                "request carries no batch item",
            );
        };
        reply.operation = Some(item.operation);
        reply.batch_item_id = item.unique_batch_item_id;
        if let Some((reason, message)) = self.behavior.refuse_with {
            return reply.failure(reason, message);
        }
        match item.payload {
            RequestPayload::DiscoverVersions(payload) => self.discover_versions(&reply, payload),
            RequestPayload::Query(payload) => self.query(&reply, payload),
            RequestPayload::GetAttributes(payload) => self.get_attributes(&reply, payload),
            RequestPayload::Encrypt(payload) => self.encrypt(&reply, payload),
            RequestPayload::Decrypt(payload) => self.decrypt(&reply, payload),
        }
    }

    fn discover_versions(
        &self,
        reply: &Reply<'_>,
        payload: DiscoverVersionsRequestPayload,
    ) -> Vec<u8> {
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
        reply.success(DiscoverVersionsResponsePayload {
            protocol_versions: (!offered.is_empty()).then_some(offered),
        })
    }

    fn query(&self, reply: &Reply<'_>, payload: QueryRequestPayload) -> Vec<u8> {
        let wants = |function| payload.query_functions.contains(&function);
        reply.success(QueryResponsePayload {
            operations: wants(QueryFunction::QueryOperations)
                .then(|| self.behavior.advertised_operations.clone()),
            object_types: wants(QueryFunction::QueryObjects)
                .then(|| vec![ObjectType::SymmetricKey]),
            vendor_identification: wants(QueryFunction::QueryServerInformation)
                .then(|| VendorIdentification("carbide-kmip mock server".to_string())),
            server_information: wants(QueryFunction::QueryServerInformation)
                .then_some(ServerInformation::default()),
        })
    }

    fn get_attributes(&self, reply: &Reply<'_>, payload: GetAttributesRequestPayload) -> Vec<u8> {
        let Some(unique_identifier) = payload.unique_identifier else {
            return reply.failure(ResultReason::MissingData, "a unique identifier is required");
        };
        let keys = self.lock_keys();
        let Some(key) = keys.get(&unique_identifier.0) else {
            return reply.failure(ResultReason::ItemNotFound, "object not found");
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
        reply.success(GetAttributesResponsePayload {
            unique_identifier: reply.echo(unique_identifier),
            attributes: (!attributes.is_empty()).then_some(attributes),
        })
    }

    fn encrypt(&self, reply: &Reply<'_>, payload: EncryptRequestPayload) -> Vec<u8> {
        let keys = self.lock_keys();
        let (unique_identifier, key) = match usable_key(
            reply,
            &keys,
            payload.unique_identifier,
            USAGE_MASK_ENCRYPT,
            "encrypt",
        ) {
            Ok(found) => found,
            Err(refusal) => return refusal,
        };
        let parameters = payload.cryptographic_parameters.unwrap_or_default();
        if parameters.block_cipher_mode != Some(BlockCipherMode::Gcm) {
            return reply.failure(ResultReason::InvalidField, "block cipher mode must be GCM");
        }
        if parameters.tag_length.is_some_and(|length| length.0 != 16) {
            return reply.failure(ResultReason::InvalidField, "tag length must be 16 bytes");
        }
        // KMIP 1.4 section 3.6: IV Length is required for GCM.
        let Some(requested_iv_len) = parameters
            .iv_length
            .and_then(|bits| usize::try_from(bits.0).ok())
            .filter(|bits| bits % 8 == 0)
            .map(|bits| bits / 8)
        else {
            return reply.failure(
                ResultReason::MissingData,
                "IV length in bits is required for GCM",
            );
        };
        let iv = match payload.iv_counter_nonce {
            Some(iv) => iv.0,
            None if parameters.random_iv == Some(RandomIv(true)) => {
                let iv_len = self.behavior.iv_len.unwrap_or(requested_iv_len);
                rand::random::<[u8; 16]>()
                    .get(..iv_len)
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default()
            }
            None => {
                return reply.failure(
                    ResultReason::MissingData,
                    "an IV is required unless Random IV is requested",
                );
            }
        };
        // The cipher helper supports the two IV sizes real servers produce;
        // anything else is refused rather than panicked on.
        let Some(sealed) = seal(&key.material, &iv, &payload.data.0) else {
            return reply.failure(ResultReason::InvalidField, "IV must be 96 or 128 bits");
        };
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let mut ciphertext = ciphertext.to_vec();
        let mut iv = Some(iv);
        let mut tag = Some(tag.to_vec());
        match self.behavior.encrypt_fault {
            None => {}
            Some(EncryptFault::OmitIv) => iv = None,
            Some(EncryptFault::EmptyIv) => iv = Some(Vec::new()),
            Some(EncryptFault::OmitTag) => tag = None,
            Some(EncryptFault::EmptyTag) => tag = Some(Vec::new()),
            Some(EncryptFault::ShortTag) => tag = tag.map(|tag| tag[..8].to_vec()),
            Some(EncryptFault::LongCiphertext) => ciphertext.push(0),
        }
        reply.success(EncryptResponsePayload {
            unique_identifier: reply.echo(unique_identifier),
            data: Data(ciphertext),
            iv_counter_nonce: iv.map(IvCounterNonce),
            correlation_value: None,
            authenticated_encryption_tag: tag.map(AuthenticatedEncryptionTag),
        })
    }

    fn decrypt(&self, reply: &Reply<'_>, payload: DecryptRequestPayload) -> Vec<u8> {
        let keys = self.lock_keys();
        let (unique_identifier, key) = match usable_key(
            reply,
            &keys,
            payload.unique_identifier,
            USAGE_MASK_DECRYPT,
            "decrypt",
        ) {
            Ok(found) => found,
            Err(refusal) => return refusal,
        };
        let (Some(iv), Some(tag)) = (
            payload.iv_counter_nonce,
            payload.authenticated_encryption_tag,
        ) else {
            return reply.failure(
                ResultReason::MissingData,
                "an IV and an authentication tag are required",
            );
        };
        // A strict server checks that the parameters describe the IV and tag
        // it was handed: IV Length in bits and Tag Length in bytes (KMIP 1.4
        // section 3.6).
        let parameters = payload.cryptographic_parameters.unwrap_or_default();
        if parameters.block_cipher_mode != Some(BlockCipherMode::Gcm) {
            return reply.failure(ResultReason::InvalidField, "block cipher mode must be GCM");
        }
        if parameters
            .iv_length
            .is_some_and(|bits| usize::try_from(bits.0).ok() != Some(iv.0.len() * 8))
        {
            return reply.failure(
                ResultReason::InvalidField,
                "IV length does not describe the IV",
            );
        }
        if parameters
            .tag_length
            .is_some_and(|length| usize::try_from(length.0).ok() != Some(tag.0.len()))
        {
            return reply.failure(
                ResultReason::InvalidField,
                "tag length does not describe the tag",
            );
        }
        if !matches!(iv.0.len(), 12 | 16) {
            return reply.failure(ResultReason::InvalidField, "IV must be 96 or 128 bits");
        }
        let mut sealed = payload.data.0.clone();
        sealed.extend_from_slice(&tag.0);
        match open(&key.material, &iv.0, &sealed) {
            Some(plaintext) => reply.success(DecryptResponsePayload {
                unique_identifier: reply.echo(unique_identifier),
                data: Data(plaintext),
                correlation_value: None,
            }),
            None => reply.failure(ResultReason::CryptographicFailure, "authentication failed"),
        }
    }

    fn lock_keys(&self) -> MutexGuard<'_, HashMap<String, MockKey>> {
        self.keys
            .lock()
            .expect("mock key store mutex poisoned; a handler panicked")
    }
}

/// Looks up the key an Encrypt or Decrypt names and checks that it may be
/// used for `verb`, with the refusal to send otherwise.
fn usable_key<'k>(
    reply: &Reply<'_>,
    keys: &'k HashMap<String, MockKey>,
    unique_identifier: Option<UniqueIdentifier>,
    usage_bit: i32,
    verb: &str,
) -> Result<(UniqueIdentifier, &'k MockKey), Vec<u8>> {
    let Some(unique_identifier) = unique_identifier else {
        return Err(reply.failure(ResultReason::MissingData, "a unique identifier is required"));
    };
    let Some(key) = keys.get(&unique_identifier.0) else {
        return Err(reply.failure(ResultReason::ItemNotFound, "object not found"));
    };
    if key.state != State::Active {
        return Err(reply.failure(ResultReason::PermissionDenied, "object is not active"));
    }
    if key.usage_mask & usage_bit == 0 {
        return Err(reply.failure(
            ResultReason::PermissionDenied,
            &format!("usage mask does not permit {verb}"),
        ));
    }
    Ok((unique_identifier, key))
}

/// AES-256-GCM with a 12- or 16-byte IV; returns ciphertext followed by the
/// 16-byte tag, or `None` for an IV length the helper does not support.
fn seal(key: &[u8; 32], iv: &[u8], plaintext: &[u8]) -> Option<Vec<u8>> {
    fn with<N: ArrayLength<u8>>(key: &[u8; 32], iv: &[u8], plaintext: &[u8]) -> Vec<u8> {
        AesGcm::<Aes256, N>::new(key.into())
            .encrypt(GenericArray::from_slice(iv), plaintext)
            .expect("AES-GCM seal in the mock KMIP server")
    }
    match iv.len() {
        12 => Some(with::<U12>(key, iv, plaintext)),
        16 => Some(with::<U16>(key, iv, plaintext)),
        _ => None,
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
