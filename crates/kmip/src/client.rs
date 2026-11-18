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
//! The client: one cached mutual-TLS connection, one request in flight at a
//! time, bounded deadlines, every response checked against its request, and a
//! single retry when a reused connection turns out to have been closed.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rustls_pki_types::ServerName;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use zeroize::Zeroizing;

use crate::config::{self, KmipClientConfig, MAX_CONNECTION_IDLE};
use crate::error::{KmipError, TimeoutPhase};
use crate::framing::{self, FrameError, MAX_RESPONSE_BYTES, TAG_RESPONSE_MESSAGE};
use crate::tls;
use crate::wire::{
    self, Attribute, AttributeName, AttributeValue, AuthenticatedEncryptionTag, BatchCount,
    BlockCipherMode, CryptographicAlgorithm, CryptographicParameters, Data, DecryptRequestPayload,
    DecryptResponsePayload, DiscoverVersionsRequestPayload, DiscoverVersionsResponsePayload,
    EncryptRequestPayload, EncryptResponsePayload, GetAttributesRequestPayload,
    GetAttributesResponsePayload, IgnoredPayload, IvCounterNonce, IvLength, Operation,
    PROTOCOL_VERSION_1_4, PaddingMethod, QueryFunction, QueryRequestPayload, QueryResponsePayload,
    RandomIv, RequestBatchItem, RequestHeader, RequestMessage, ResponseBatchItem, ResponseMessage,
    ResultStatus, State, TagLength, UniqueBatchItemId, UniqueIdentifier,
};

#[cfg(test)]
mod vectors;

/// Cryptographic Usage Mask bit permitting Encrypt, KMIP 1.4 section 9.1.3.3.1.
pub const USAGE_MASK_ENCRYPT: i32 = 0x0000_0004;

/// Cryptographic Usage Mask bit permitting Decrypt, KMIP 1.4 section 9.1.3.3.1.
pub const USAGE_MASK_DECRYPT: i32 = 0x0000_0008;

/// Length in bytes of the AES-GCM authentication tag requested on Encrypt and
/// required in its response.
const GCM_TAG_LEN: usize = 16;

/// Length in bits of the AES-GCM IV the server is asked to generate. A server
/// may answer with another length, which is kept verbatim.
const GCM_IV_LEN_BITS: i32 = 96;

/// The response bound as the request header carries it.
const MAXIMUM_RESPONSE_SIZE: wire::MaximumResponseSize =
    wire::MaximumResponseSize(MAX_RESPONSE_BYTES as i32);

/// The attribute names read by [`KmipClient::get_key_attributes`].
const KEY_ATTRIBUTE_NAMES: [&str; 4] = [
    "State",
    "Cryptographic Algorithm",
    "Cryptographic Length",
    "Cryptographic Usage Mask",
];

/// A KMIP protocol version as reported by Discover Versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProtocolVersion {
    /// Protocol Version Major.
    pub major: i32,
    /// Protocol Version Minor.
    pub minor: i32,
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// What the server reported for Query Operations and Query Server
/// Information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryInfo {
    /// Every operation the server advertises; values this crate does not
    /// model appear as [`Operation::Other`].
    pub operations: Vec<Operation>,
    /// The server's Vendor Identification string, when it sent one.
    pub vendor_identification: Option<String>,
}

/// The attributes of a key object read by [`KmipClient::get_key_attributes`].
/// Each is `None` when the server did not return it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyAttributes {
    /// The object's State.
    pub state: Option<State>,
    /// The object's Cryptographic Algorithm.
    pub algorithm: Option<CryptographicAlgorithm>,
    /// Cryptographic Length in bits.
    pub length_bits: Option<i32>,
    /// Cryptographic Usage Mask; test bits with [`USAGE_MASK_ENCRYPT`] and
    /// [`USAGE_MASK_DECRYPT`].
    pub usage_mask: Option<i32>,
}

/// The output of an authenticated Encrypt, each field stored verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AeadCiphertext {
    /// The server-generated IV. A 12-byte IV is requested, but the length is
    /// the server's choice; nothing here assumes 12.
    pub iv: Vec<u8>,
    /// The ciphertext, as long as the plaintext.
    pub ciphertext: Vec<u8>,
    /// The 16-byte authentication tag.
    pub tag: Vec<u8>,
}

/// A client for one KMIP server, speaking KMIP 1.4 only.
///
/// Requests are serialized on one cached TLS connection, opened lazily, closed
/// once idle for [`MAX_CONNECTION_IDLE`], and reopened after a failure. One
/// request is in flight at a time; callers queue on the connection with no
/// limit but their own request deadline, so sustained load above one request
/// per round trip fails queued callers with [`KmipError::RequestTimeout`],
/// and a caller that needs more throughput sizes its concurrency accordingly.
///
/// Deadlines: waiting for the connection is bounded by `request_timeout`;
/// re-reading the TLS files, the TCP connect and the TLS handshake together
/// by `connect_timeout`; writing a request and reading its complete response
/// by `request_timeout`. A call therefore takes at most
/// `3 × request_timeout + connect_timeout`: the queue wait, an exchange on a
/// reused connection that turns out to have been closed, the reconnect, and
/// the exchange that follows. That single retry happens only when a reused
/// connection fails before any response byte arrives; no operation here
/// changes key state, so repeating one is safe, though a server may count or
/// audit the repeat. A timeout, a failure after the response has started,
/// and any failure on a fresh connection are not retried.
///
/// Every response is checked against its request: operation, Unique Batch
/// Item ID and, for the object operations, Unique Identifier; a successful
/// answer to anything but Discover Versions must also declare protocol
/// version 1.4. A connection that produced a protocol violation or timed out
/// is discarded, as is the connection of a call dropped mid-flight; the next
/// call reconnects.
pub struct KmipClient {
    config: KmipClientConfig,
    host: String,
    port: u16,
    server_name: ServerName<'static>,
    connection: Mutex<Option<Connection>>,
    next_batch_item_id: AtomicU64,
    max_idle: Duration,
}

struct Connection {
    stream: TlsStream<TcpStream>,
    /// When the last exchange on this connection completed.
    last_used: Instant,
}

impl fmt::Debug for KmipClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KmipClient")
            .field("endpoint", &self.config.endpoint)
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

/// What a response must agree with to be the answer to a request.
#[derive(Clone, Copy)]
struct Correlation<'a> {
    operation: Operation,
    batch_item_id: &'a [u8],
    /// The object the request named, when it named one.
    unique_identifier: Option<&'a str>,
    /// The length of the plaintext an Encrypt request carried, which its
    /// ciphertext must match.
    plaintext_len: Option<usize>,
}

impl Correlation<'_> {
    /// Expectations for `operation` alone; the batch item ID is filled in
    /// when the request is sent.
    fn new(operation: Operation) -> Self {
        Self {
            operation,
            batch_item_id: &[],
            unique_identifier: None,
            plaintext_len: None,
        }
    }
}

/// A response payload: the object it names, when the operation has one, and
/// the checks and conversion that turn it into what the operation returns.
/// Both run before the connection goes back into the cache, so a payload
/// that fails them discards the connection like any other protocol
/// violation.
trait ResponsePayload: DeserializeOwned {
    /// What the operation hands to its caller.
    type Output;

    fn unique_identifier(&self) -> Option<&str> {
        None
    }

    /// Checks the payload against the request and converts it.
    fn finish(self, correlation: &Correlation<'_>) -> Result<Self::Output, KmipError>;
}

impl ResponsePayload for DiscoverVersionsResponsePayload {
    type Output = Self;

    fn finish(self, _correlation: &Correlation<'_>) -> Result<Self, KmipError> {
        Ok(self)
    }
}

impl ResponsePayload for QueryResponsePayload {
    type Output = Self;

    fn finish(self, _correlation: &Correlation<'_>) -> Result<Self, KmipError> {
        Ok(self)
    }
}

impl ResponsePayload for GetAttributesResponsePayload {
    type Output = Self;

    fn unique_identifier(&self) -> Option<&str> {
        Some(&self.unique_identifier.0)
    }

    fn finish(self, _correlation: &Correlation<'_>) -> Result<Self, KmipError> {
        Ok(self)
    }
}

impl ResponsePayload for EncryptResponsePayload {
    type Output = AeadCiphertext;

    fn unique_identifier(&self) -> Option<&str> {
        Some(&self.unique_identifier.0)
    }

    /// An Encrypt result is usable only with an IV, a 16-byte tag and
    /// ciphertext as long as the plaintext.
    fn finish(self, correlation: &Correlation<'_>) -> Result<AeadCiphertext, KmipError> {
        let operation = correlation.operation;
        let iv = self
            .iv_counter_nonce
            .map(|iv| iv.0)
            .filter(|iv| !iv.is_empty())
            .ok_or_else(|| violation(operation, "encrypt response carries no IV"))?;
        let tag = self
            .authenticated_encryption_tag
            .map(|tag| tag.0)
            .ok_or_else(|| {
                violation(operation, "encrypt response carries no authentication tag")
            })?;
        if tag.len() != GCM_TAG_LEN {
            return Err(violation(
                operation,
                format!(
                    "encrypt response carries a {}-byte authentication tag, expected {GCM_TAG_LEN}",
                    tag.len()
                ),
            ));
        }
        let ciphertext = self.data.into_vec();
        if let Some(plaintext_len) = correlation.plaintext_len
            && ciphertext.len() != plaintext_len
        {
            return Err(violation(
                operation,
                format!(
                    "encrypt response carries {} ciphertext bytes for {plaintext_len} plaintext bytes",
                    ciphertext.len()
                ),
            ));
        }
        Ok(AeadCiphertext {
            iv,
            ciphertext,
            tag,
        })
    }
}

impl ResponsePayload for DecryptResponsePayload {
    type Output = Self;

    fn unique_identifier(&self) -> Option<&str> {
        Some(&self.unique_identifier.0)
    }

    fn finish(self, _correlation: &Correlation<'_>) -> Result<Self, KmipError> {
        Ok(self)
    }
}

impl KmipClient {
    /// Validates the configuration and the TLS material on disk without
    /// touching the network: deadlines must be within their hard maxima, the
    /// endpoint and server name must parse, and the CA bundle, client
    /// certificate and key must be readable, valid PEM and mutually
    /// consistent. The files are read with blocking I/O, so call this during
    /// startup rather than from a hot async path. The first connection is
    /// opened by the first operation.
    pub fn new(config: KmipClientConfig) -> Result<Self, KmipError> {
        config::validate_timeouts(&config)?;
        let (host, port) = config::parse_endpoint(&config.endpoint)?;
        let server_name = match &config.server_name {
            Some(name) => config::server_name("server_name", name)?,
            None => config::server_name("endpoint", &host)?,
        };
        let material = tls::read_material_blocking(&config)?;
        tls::build_client_config(&config, &material)?;
        Ok(Self {
            config,
            host,
            port,
            server_name,
            connection: Mutex::new(None),
            next_batch_item_id: AtomicU64::new(1),
            max_idle: MAX_CONNECTION_IDLE,
        })
    }

    /// Lowers the idle bound so a test can watch a connection be recycled.
    #[cfg(test)]
    pub(crate) fn with_max_idle(mut self, max_idle: Duration) -> Self {
        self.max_idle = max_idle;
        self
    }

    /// The configured endpoint, for log fields.
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// Asks the server for every protocol version it supports. The client
    /// itself speaks only 1.4; a caller decides what to do with a server
    /// whose list lacks it.
    pub async fn discover_versions(&self) -> Result<Vec<ProtocolVersion>, KmipError> {
        let response = self
            .roundtrip::<_, DiscoverVersionsResponsePayload>(
                Correlation::new(Operation::DiscoverVersions),
                &discover_versions_request(),
            )
            .await?;
        Ok(response
            .protocol_versions
            .unwrap_or_default()
            .into_iter()
            .map(|version| ProtocolVersion {
                major: version.major.0,
                minor: version.minor.0,
            })
            .collect())
    }

    /// Asks the server which operations it supports and who makes it.
    pub async fn query_operations(&self) -> Result<QueryInfo, KmipError> {
        let response = self
            .roundtrip::<_, QueryResponsePayload>(
                Correlation::new(Operation::Query),
                &query_request(),
            )
            .await?;
        Ok(QueryInfo {
            operations: response.operations.unwrap_or_default(),
            vendor_identification: response.vendor_identification.map(|vendor| vendor.0),
        })
    }

    /// Reads the state, algorithm, length and usage mask of the object with
    /// `unique_identifier`. A server reports an unknown identifier as a
    /// refusal, normally with Result Reason `ItemNotFound` (see
    /// [`KmipError::is_item_not_found`]); whatever reason it reports is
    /// passed through unchanged. One of the four attributes arriving with a
    /// value of the wrong type is a protocol violation, as is any attribute
    /// with an Interval or Big Integer value: the TTLV deserializer does not
    /// decode those types. Other attributes the server adds are ignored.
    pub async fn get_key_attributes(
        &self,
        unique_identifier: &str,
    ) -> Result<KeyAttributes, KmipError> {
        let request = get_attributes_request(unique_identifier);
        let correlation = Correlation {
            unique_identifier: Some(unique_identifier),
            ..Correlation::new(Operation::GetAttributes)
        };
        let response = self
            .roundtrip::<_, GetAttributesResponsePayload>(correlation, &request)
            .await?;
        Ok(key_attributes_from(response.attributes.unwrap_or_default()))
    }

    /// Encrypts `plaintext` under the key `unique_identifier` with AES-GCM,
    /// asking for a server-generated IV and a 16-byte tag. The response must
    /// carry the IV and the tag as separate fields, a 16-byte tag, and
    /// ciphertext of the plaintext's length; anything else is a protocol
    /// violation. The IV's length is the server's choice and is kept verbatim.
    pub async fn encrypt(
        &self,
        unique_identifier: &str,
        plaintext: &[u8],
    ) -> Result<AeadCiphertext, KmipError> {
        let request = encrypt_request(unique_identifier, plaintext);
        let correlation = Correlation {
            unique_identifier: Some(unique_identifier),
            plaintext_len: Some(plaintext.len()),
            ..Correlation::new(Operation::Encrypt)
        };
        self.roundtrip::<_, EncryptResponsePayload>(correlation, &request)
            .await
    }

    /// Decrypts an [`AeadCiphertext`] produced by [`KmipClient::encrypt`]
    /// under the key `unique_identifier`. The server refuses a tampered
    /// ciphertext, IV or tag, reporting an authentication failure with Result
    /// Reason `CryptographicFailure`; whatever reason it reports is passed
    /// through unchanged.
    pub async fn decrypt(
        &self,
        unique_identifier: &str,
        ciphertext: &AeadCiphertext,
    ) -> Result<Zeroizing<Vec<u8>>, KmipError> {
        let request = decrypt_request(unique_identifier, ciphertext)?;
        let correlation = Correlation {
            unique_identifier: Some(unique_identifier),
            ..Correlation::new(Operation::Decrypt)
        };
        let response = self
            .roundtrip::<_, DecryptResponsePayload>(correlation, &request)
            .await?;
        Ok(response.data.into_zeroizing())
    }

    /// Sends one request and decodes its response, reconnecting once if a
    /// reused connection turns out to be dead.
    async fn roundtrip<Request: Serialize, Response: ResponsePayload>(
        &self,
        correlation: Correlation<'_>,
        payload: &Request,
    ) -> Result<Response::Output, KmipError> {
        let operation = correlation.operation;
        let batch_item_id = self
            .next_batch_item_id
            .fetch_add(1, Ordering::Relaxed)
            .to_be_bytes();
        let correlation = Correlation {
            batch_item_id: &batch_item_id,
            ..correlation
        };
        let request = Zeroizing::new(encode_request(operation, &batch_item_id, payload).map_err(
            |error| KmipError::Protocol {
                operation,
                detail: "request did not encode".to_string(),
                source: Some(Box::new(error)),
            },
        )?);
        let timed_out = |phase| KmipError::RequestTimeout {
            operation,
            timeout: self.config.request_timeout,
            phase,
        };
        // Waiting for a caller ahead of us counts against the request deadline,
        // so a queue behind a slow server cannot stall callers indefinitely.
        let mut slot = tokio::time::timeout(self.config.request_timeout, self.connection.lock())
            .await
            .map_err(|_elapsed| timed_out(TimeoutPhase::Queued))?;
        // The connection leaves the slot for the exchange so that dropping this
        // future mid-flight drops the connection with it.
        let (mut connection, mut reused) = match slot.take() {
            Some(connection) if connection.last_used.elapsed() <= self.max_idle => {
                (connection, true)
            }
            Some(idle) => {
                // Close the old session before opening another, for servers
                // that limit concurrent sessions per client.
                drop(idle);
                tracing::debug!(
                    endpoint_address = %self.config.endpoint,
                    %operation,
                    "cached KMIP connection has been idle too long; reconnecting"
                );
                (self.connect().await?, false)
            }
            None => (self.connect().await?, false),
        };
        loop {
            let exchange = exchange(&mut connection.stream, &request);
            match tokio::time::timeout(self.config.request_timeout, exchange).await {
                Ok(Ok(frame)) => {
                    let decoded = decode_response::<Response>(correlation, &frame);
                    // A refusal is a well-formed answer, so the connection stays
                    // usable; after a protocol violation the stream can no
                    // longer be trusted and the connection is dropped here.
                    if matches!(decoded, Ok(_) | Err(KmipError::Refused { .. })) {
                        connection.last_used = Instant::now();
                        *slot = Some(connection);
                    }
                    return decoded;
                }
                Ok(Err(FrameError::NoResponse(error))) if reused => {
                    tracing::debug!(
                        endpoint_address = %self.config.endpoint,
                        %operation,
                        error = %error,
                        "cached KMIP connection was closed; reconnecting"
                    );
                    connection = self.connect().await?;
                    reused = false;
                }
                Ok(Err(error)) => return Err(frame_error(operation, error)),
                Err(_elapsed) => return Err(timed_out(TimeoutPhase::InFlight)),
            }
        }
    }

    /// Opens a new mutual-TLS connection within the connect deadline,
    /// re-reading the PEM files first so rotated material is picked up.
    async fn connect(&self) -> Result<Connection, KmipError> {
        let attempt = async {
            let material = tls::read_material(&self.config).await?;
            let tls_config = Arc::new(tls::build_client_config(&self.config, &material)?);
            let connector = TlsConnector::from(tls_config);
            let handshake = async {
                let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
                tcp.set_nodelay(true)?;
                connector.connect(self.server_name.clone(), tcp).await
            };
            handshake.await.map_err(|source| KmipError::Connect {
                endpoint: self.config.endpoint.clone(),
                source,
            })
        };
        match tokio::time::timeout(self.config.connect_timeout, attempt).await {
            Ok(Ok(stream)) => {
                tracing::debug!(
                    endpoint_address = %self.config.endpoint,
                    "connected to KMIP server"
                );
                Ok(Connection {
                    stream,
                    last_used: Instant::now(),
                })
            }
            Ok(Err(error)) => Err(error),
            Err(_elapsed) => Err(KmipError::ConnectTimeout {
                endpoint: self.config.endpoint.clone(),
                timeout: self.config.connect_timeout,
            }),
        }
    }
}

/// Writes one request frame and reads one response frame.
async fn exchange(
    stream: &mut TlsStream<TcpStream>,
    request: &[u8],
) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    framing::write_frame(stream, request).await?;
    framing::read_frame(stream, TAG_RESPONSE_MESSAGE, MAX_RESPONSE_BYTES).await
}

fn frame_error(operation: Operation, error: FrameError) -> KmipError {
    match error {
        FrameError::NoResponse(source) | FrameError::Io(source) => {
            KmipError::Transport { operation, source }
        }
        FrameError::Protocol(detail) => violation(operation, detail),
        FrameError::TooLarge { length, max } => KmipError::ResponseTooLarge { length, max },
    }
}

/// A protocol violation detected by this crate's own checks.
fn violation(operation: Operation, detail: impl Into<String>) -> KmipError {
    KmipError::Protocol {
        operation,
        detail: detail.into(),
        source: None,
    }
}

/// The Discover Versions request: no list, so the server reports every
/// version it supports.
fn discover_versions_request() -> DiscoverVersionsRequestPayload {
    DiscoverVersionsRequestPayload {
        protocol_versions: None,
    }
}

/// The Query request for the operation list and the vendor identification.
fn query_request() -> QueryRequestPayload {
    QueryRequestPayload {
        query_functions: vec![
            QueryFunction::QueryOperations,
            QueryFunction::QueryServerInformation,
        ],
    }
}

/// The Get Attributes request for the four attributes in [`KeyAttributes`].
fn get_attributes_request(unique_identifier: &str) -> GetAttributesRequestPayload {
    GetAttributesRequestPayload {
        unique_identifier: Some(UniqueIdentifier(unique_identifier.to_string())),
        attribute_names: Some(
            KEY_ATTRIBUTE_NAMES
                .iter()
                .map(|name| AttributeName(name.to_string()))
                .collect(),
        ),
    }
}

/// Folds the attributes of a Get Attributes response into [`KeyAttributes`].
/// The four requested attributes are selected by name during decoding, so a
/// requested attribute with a value of the wrong type never reaches here: the
/// response fails to decode instead. Attributes the client did not ask for
/// decode by type and are ignored.
fn key_attributes_from(attributes: Vec<Attribute>) -> KeyAttributes {
    let mut folded = KeyAttributes::default();
    for attribute in attributes {
        match attribute.value {
            AttributeValue::State(state) => folded.state = Some(state),
            AttributeValue::CryptographicAlgorithm(algorithm) => {
                folded.algorithm = Some(algorithm);
            }
            AttributeValue::CryptographicLength(bits) => folded.length_bits = Some(bits),
            AttributeValue::CryptographicUsageMask(mask) => folded.usage_mask = Some(mask),
            AttributeValue::OtherInteger(_)
            | AttributeValue::OtherLongInteger(_)
            | AttributeValue::OtherEnumeration(_)
            | AttributeValue::OtherBoolean(_)
            | AttributeValue::OtherTextString(_)
            | AttributeValue::OtherByteString(_)
            | AttributeValue::OtherDateTime(_)
            | AttributeValue::OtherStructure(_) => {}
        }
    }
    folded
}

/// The single-part AES-GCM Encrypt request with a server-generated 96-bit IV.
fn encrypt_request(unique_identifier: &str, plaintext: &[u8]) -> EncryptRequestPayload {
    EncryptRequestPayload {
        unique_identifier: Some(UniqueIdentifier(unique_identifier.to_string())),
        cryptographic_parameters: Some(aes_gcm_parameters(
            true,
            GCM_IV_LEN_BITS,
            GCM_TAG_LEN as i32,
        )),
        data: Data(plaintext.to_vec()),
        iv_counter_nonce: None,
    }
}

/// The single-part AES-GCM Decrypt request for `ciphertext`, describing the
/// IV and tag it carries by their actual lengths.
fn decrypt_request(
    unique_identifier: &str,
    ciphertext: &AeadCiphertext,
) -> Result<DecryptRequestPayload, KmipError> {
    let too_long = |what: &str| {
        violation(
            Operation::Decrypt,
            format!("{what} is too long to describe in a request"),
        )
    };
    let tag_length =
        i32::try_from(ciphertext.tag.len()).map_err(|_| too_long("authentication tag"))?;
    let iv_length_bits = ciphertext
        .iv
        .len()
        .checked_mul(8)
        .and_then(|bits| i32::try_from(bits).ok())
        .ok_or_else(|| too_long("IV"))?;
    Ok(DecryptRequestPayload {
        unique_identifier: Some(UniqueIdentifier(unique_identifier.to_string())),
        cryptographic_parameters: Some(aes_gcm_parameters(false, iv_length_bits, tag_length)),
        data: Data(ciphertext.ciphertext.clone()),
        iv_counter_nonce: Some(IvCounterNonce(ciphertext.iv.clone())),
        authenticated_encryption_tag: Some(AuthenticatedEncryptionTag(ciphertext.tag.clone())),
    })
}

/// AES-GCM with no padding, an IV of `iv_length_bits` and a tag of
/// `tag_length` bytes; `random_iv` asks the server to generate the IV. IV
/// Length is required by KMIP 1.4 section 3.6 for modes with variable IV
/// lengths such as GCM.
fn aes_gcm_parameters(
    random_iv: bool,
    iv_length_bits: i32,
    tag_length: i32,
) -> CryptographicParameters {
    CryptographicParameters {
        block_cipher_mode: Some(BlockCipherMode::Gcm),
        padding_method: Some(PaddingMethod::None),
        cryptographic_algorithm: Some(CryptographicAlgorithm::Aes),
        random_iv: random_iv.then_some(RandomIv(true)),
        iv_length: Some(IvLength(iv_length_bits)),
        tag_length: Some(TagLength(tag_length)),
    }
}

/// Serializes a single-item KMIP 1.4 request message carrying `payload`
/// under `batch_item_id`.
fn encode_request<P: Serialize>(
    operation: Operation,
    batch_item_id: &[u8],
    payload: &P,
) -> kmip_ttlv::error::Result<Vec<u8>> {
    let message = RequestMessage {
        header: RequestHeader {
            protocol_version: PROTOCOL_VERSION_1_4,
            maximum_response_size: Some(MAXIMUM_RESPONSE_SIZE),
            batch_count: BatchCount(1),
        },
        batch_items: vec![RequestBatchItem {
            operation,
            unique_batch_item_id: Some(UniqueBatchItemId(batch_item_id.to_vec())),
            payload,
        }],
    };
    kmip_ttlv::to_vec(&message)
}

/// Decodes a response frame into the payload answering the request described
/// by `correlation`, turning a non-Success result into [`KmipError::Refused`].
fn decode_response<Response: ResponsePayload>(
    correlation: Correlation<'_>,
    frame: &[u8],
) -> Result<Response::Output, KmipError> {
    let operation = correlation.operation;
    let message: ResponseMessage<Response> = match kmip_ttlv::from_slice(frame) {
        Ok(message) => message,
        Err(decode_error) => {
            // A refusal may carry a payload that does not decode as the
            // expected type; read the result status with the payload skipped
            // before giving up on the frame.
            if let Ok(outline) = kmip_ttlv::from_slice::<ResponseMessage<IgnoredPayload>>(frame)
                && let Ok(item) = single_item(correlation, outline)
                && item.result_status != ResultStatus::Success
            {
                return Err(refusal(operation, item));
            }
            return Err(KmipError::Protocol {
                operation,
                detail: "response did not decode".to_string(),
                source: Some(Box::new(decode_error)),
            });
        }
    };
    // A refusal is reported whatever version the server answered in, and so
    // is a Discover Versions list: both exist to tell the caller about a
    // server that may not speak 1.4. Every other successful answer must.
    let version = message.header.protocol_version.clone();
    let item = single_item(correlation, message)?;
    if item.result_status != ResultStatus::Success {
        return Err(refusal(operation, item));
    }
    if operation != Operation::DiscoverVersions && version != PROTOCOL_VERSION_1_4 {
        return Err(violation(
            operation,
            format!(
                "response declares protocol version {}.{}, expected 1.4",
                version.major.0, version.minor.0
            ),
        ));
    }
    let payload = item
        .payload
        .ok_or_else(|| violation(operation, "successful response carries no payload"))?;
    if let (Some(expected), Some(actual)) =
        (correlation.unique_identifier, payload.unique_identifier())
        && expected != actual
    {
        return Err(violation(
            operation,
            format!("response names object {actual:?}, expected {expected:?}"),
        ));
    }
    payload.finish(&correlation)
}

/// Checks the envelope against the request and returns the one batch item it
/// must contain.
fn single_item<P>(
    correlation: Correlation<'_>,
    message: ResponseMessage<P>,
) -> Result<ResponseBatchItem<P>, KmipError> {
    let operation = correlation.operation;
    if message.header.batch_count.0 != 1 {
        return Err(violation(
            operation,
            format!(
                "response announces {} batch items, expected 1",
                message.header.batch_count.0
            ),
        ));
    }
    let mut items = message.batch_items;
    if items.len() != 1 {
        return Err(violation(
            operation,
            format!("response carries {} batch items, expected 1", items.len()),
        ));
    }
    let item = items.remove(0);
    if let Some(echoed) = item.operation
        && echoed != operation
    {
        return Err(violation(
            operation,
            format!("response is for {echoed}, expected {operation}"),
        ));
    }
    match (&item.unique_batch_item_id, item.result_status) {
        (Some(echoed), _) if echoed.0 != correlation.batch_item_id => {
            return Err(violation(
                operation,
                "response carries another request's Unique Batch Item ID",
            ));
        }
        (None, ResultStatus::Success) => {
            return Err(violation(
                operation,
                "response carries no Unique Batch Item ID",
            ));
        }
        // A refusal may come without the echo, for instance from a server
        // that rejected the whole message; its reason is worth more than the
        // check, and it carries no data that could be misattributed.
        _ => {}
    }
    if let Some(extension) = &item.message_extension
        && extension.criticality_indicator.0
    {
        return Err(violation(
            operation,
            format!(
                "response carries a critical message extension from {:?} that this client does \
                 not understand",
                extension.vendor_identification.0
            ),
        ));
    }
    Ok(item)
}

fn refusal<P>(operation: Operation, item: ResponseBatchItem<P>) -> KmipError {
    KmipError::Refused {
        operation,
        status: item.result_status,
        reason: item.result_reason,
        message: item.result_message.map(|message| message.0),
    }
}
