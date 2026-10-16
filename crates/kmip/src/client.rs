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
//! time, bounded deadlines, and a single retry for the stale-connection case.

use std::fmt;
use std::sync::Arc;

use rustls_pki_types::ServerName;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use zeroize::Zeroizing;

use crate::config::{self, KmipClientConfig};
use crate::error::{KmipError, TlsMaterialError};
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
    ResultStatus, State, TagLength, UniqueIdentifier,
};

/// Cryptographic Usage Mask bit permitting Encrypt, KMIP 1.4 section 9.1.3.3.1.
pub const USAGE_MASK_ENCRYPT: i32 = 0x0000_0004;

/// Cryptographic Usage Mask bit permitting Decrypt, KMIP 1.4 section 9.1.3.3.1.
pub const USAGE_MASK_DECRYPT: i32 = 0x0000_0008;

/// Length in bytes of the AES-GCM authentication tag requested on Encrypt.
const GCM_TAG_LEN: i32 = 16;

/// Length in bits of the AES-GCM IV the server is asked to generate.
const GCM_IV_LEN_BITS: i32 = 96;

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
    pub major: i32,
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
    pub state: Option<State>,
    pub algorithm: Option<CryptographicAlgorithm>,
    /// Cryptographic Length in bits.
    pub length_bits: Option<i32>,
    /// Cryptographic Usage Mask; test bits with [`USAGE_MASK_ENCRYPT`] and
    /// [`USAGE_MASK_DECRYPT`].
    pub usage_mask: Option<i32>,
}

/// The output of an authenticated Encrypt: the server-generated IV, the
/// ciphertext and the authentication tag, each stored verbatim. Their lengths
/// are whatever the server produced; nothing here assumes a 12-byte IV.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AeadCiphertext {
    pub iv: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub tag: Vec<u8>,
}

/// A client for one KMIP server.
///
/// Requests are serialized on one cached TLS connection, opened lazily and
/// reopened after a failure. Waiting for the connection is bounded by
/// `request_timeout`, and each attempt is bounded by the configured connect
/// and request deadlines, so a call takes at most
/// `request_timeout + 2 × (connect_timeout + request_timeout)` when it queues
/// behind another caller and the single retry fires. The retry happens only
/// when a reused connection fails before any response byte arrives, which is
/// the stale-connection case; every operation here leaves server state
/// unchanged, so repeating one is safe. Dropping a call mid-flight drops its
/// connection, and the next call reconnects.
pub struct KmipClient {
    config: KmipClientConfig,
    host: String,
    port: u16,
    server_name: ServerName<'static>,
    connection: Mutex<Option<Connection>>,
}

struct Connection {
    stream: TlsStream<TcpStream>,
}

impl fmt::Debug for KmipClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KmipClient")
            .field("endpoint", &self.config.endpoint)
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

impl KmipClient {
    /// Validates the configuration and the TLS material on disk without
    /// touching the network: deadlines must be within their hard maxima, the
    /// endpoint and server name must parse, and the CA bundle, client
    /// certificate and key must be readable, valid PEM and mutually
    /// consistent. The first connection is opened by the first operation.
    pub fn new(config: KmipClientConfig) -> Result<Self, KmipError> {
        config::validate_timeouts(&config)?;
        let (host, port) = config::parse_endpoint(&config.endpoint)?;
        let name = config.server_name.clone().unwrap_or_else(|| host.clone());
        let server_name = ServerName::try_from(name.as_str())
            .map(|server_name| server_name.to_owned())
            .map_err(|_| TlsMaterialError::ServerName { name })?;
        let material = tls::read_material_blocking(&config)?;
        tls::build_client_config(&config, &material)?;
        Ok(Self {
            config,
            host,
            port,
            server_name,
            connection: Mutex::new(None),
        })
    }

    /// The configured endpoint, for log fields.
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// Asks the server for every protocol version it supports.
    pub async fn discover_versions(&self) -> Result<Vec<ProtocolVersion>, KmipError> {
        let response: DiscoverVersionsResponsePayload = self
            .roundtrip(Operation::DiscoverVersions, &discover_versions_request())
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
        let response: QueryResponsePayload =
            self.roundtrip(Operation::Query, &query_request()).await?;
        Ok(QueryInfo {
            operations: response.operations.unwrap_or_default(),
            vendor_identification: response.vendor_identification.map(|vendor| vendor.0),
        })
    }

    /// Reads the state, algorithm, length and usage mask of the object with
    /// `unique_identifier`. An unknown identifier is a refusal for which
    /// [`KmipError::is_item_not_found`] is true.
    pub async fn get_key_attributes(
        &self,
        unique_identifier: &str,
    ) -> Result<KeyAttributes, KmipError> {
        let request = get_attributes_request(unique_identifier);
        let response: GetAttributesResponsePayload =
            self.roundtrip(Operation::GetAttributes, &request).await?;
        key_attributes_from(response.attributes.unwrap_or_default())
    }

    /// Encrypts `plaintext` under the key `unique_identifier` with AES-GCM, a
    /// 16-byte tag and a server-generated IV. The server must return the IV
    /// and the tag as separate fields; a response without either is a
    /// protocol violation.
    pub async fn encrypt(
        &self,
        unique_identifier: &str,
        plaintext: &[u8],
    ) -> Result<AeadCiphertext, KmipError> {
        let request = encrypt_request(unique_identifier, plaintext);
        let response: EncryptResponsePayload = self.roundtrip(Operation::Encrypt, &request).await?;
        let violation = |detail: &str| KmipError::Protocol {
            operation: Operation::Encrypt,
            detail: detail.to_string(),
        };
        let iv = response
            .iv_counter_nonce
            .map(|iv| iv.0)
            .filter(|iv| !iv.is_empty())
            .ok_or_else(|| violation("encrypt response carries no IV"))?;
        let tag = response
            .authenticated_encryption_tag
            .map(|tag| tag.0)
            .filter(|tag| !tag.is_empty())
            .ok_or_else(|| violation("encrypt response carries no authentication tag"))?;
        Ok(AeadCiphertext {
            iv,
            ciphertext: response.data.into_vec(),
            tag,
        })
    }

    /// Decrypts an [`AeadCiphertext`] produced by [`KmipClient::encrypt`]
    /// under the key `unique_identifier`. A tampered ciphertext, IV or tag is
    /// a refusal with Result Reason `CryptographicFailure`.
    pub async fn decrypt(
        &self,
        unique_identifier: &str,
        ciphertext: &AeadCiphertext,
    ) -> Result<Zeroizing<Vec<u8>>, KmipError> {
        let request = decrypt_request(unique_identifier, ciphertext)?;
        let response: DecryptResponsePayload = self.roundtrip(Operation::Decrypt, &request).await?;
        Ok(response.data.into_zeroizing())
    }

    /// Sends one request and decodes its response, reconnecting once if a
    /// reused connection turns out to be dead.
    async fn roundtrip<Request: Serialize, Response: DeserializeOwned>(
        &self,
        operation: Operation,
        payload: &Request,
    ) -> Result<Response, KmipError> {
        let request = Zeroizing::new(encode_request(operation, payload).map_err(|error| {
            KmipError::Protocol {
                operation,
                detail: format!("failed to encode request: {error}"),
            }
        })?);
        // Waiting for a caller ahead of us counts against the request deadline,
        // so a queue behind a slow server cannot stall callers indefinitely.
        let mut slot = tokio::time::timeout(self.config.request_timeout, self.connection.lock())
            .await
            .map_err(|_elapsed| KmipError::RequestTimeout {
                operation,
                timeout: self.config.request_timeout,
            })?;
        // The connection leaves the slot for the exchange so that dropping this
        // future mid-flight drops the connection with it.
        let (mut connection, mut reused) = match slot.take() {
            Some(connection) => (connection, true),
            None => (self.connect().await?, false),
        };
        loop {
            let exchange = exchange(&mut connection.stream, &request);
            match tokio::time::timeout(self.config.request_timeout, exchange).await {
                Ok(Ok(frame)) => {
                    *slot = Some(connection);
                    return decode_response(operation, &frame);
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
                Err(_elapsed) => {
                    return Err(KmipError::RequestTimeout {
                        operation,
                        timeout: self.config.request_timeout,
                    });
                }
            }
        }
    }

    /// Opens a new mutual-TLS connection, re-reading the PEM files so rotated
    /// material is picked up, within the connect deadline.
    async fn connect(&self) -> Result<Connection, KmipError> {
        let material = tls::read_material(&self.config).await?;
        let tls_config = Arc::new(tls::build_client_config(&self.config, &material)?);
        let connector = TlsConnector::from(tls_config);
        let attempt = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
            tcp.set_nodelay(true)?;
            connector.connect(self.server_name.clone(), tcp).await
        };
        match tokio::time::timeout(self.config.connect_timeout, attempt).await {
            Ok(Ok(stream)) => {
                tracing::debug!(
                    endpoint_address = %self.config.endpoint,
                    "connected to KMIP server"
                );
                Ok(Connection { stream })
            }
            Ok(Err(source)) => Err(KmipError::Connect {
                endpoint: self.config.endpoint.clone(),
                source,
            }),
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
        FrameError::Protocol(detail) => KmipError::Protocol { operation, detail },
        FrameError::TooLarge { length, max } => KmipError::ResponseTooLarge { length, max },
    }
}

/// The Discover Versions request: no list, so the server reports every
/// version it supports.
pub(crate) fn discover_versions_request() -> DiscoverVersionsRequestPayload {
    DiscoverVersionsRequestPayload {
        protocol_versions: None,
    }
}

/// The Query request for the operation list and the vendor identification.
pub(crate) fn query_request() -> QueryRequestPayload {
    QueryRequestPayload {
        query_functions: vec![
            QueryFunction::QueryOperations,
            QueryFunction::QueryServerInformation,
        ],
    }
}

/// The Get Attributes request for the four attributes in [`KeyAttributes`].
pub(crate) fn get_attributes_request(unique_identifier: &str) -> GetAttributesRequestPayload {
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
/// Attributes the client did not ask for are ignored; one it did ask for
/// arriving with an unexpected type is a protocol violation rather than a
/// silently missing value.
pub(crate) fn key_attributes_from(attributes: Vec<Attribute>) -> Result<KeyAttributes, KmipError> {
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
            | AttributeValue::OtherStructure(_) => {
                if KEY_ATTRIBUTE_NAMES.contains(&attribute.name.0.as_str()) {
                    return Err(KmipError::Protocol {
                        operation: Operation::GetAttributes,
                        detail: format!("attribute {:?} has an unexpected type", attribute.name.0),
                    });
                }
            }
        }
    }
    Ok(folded)
}

/// The single-part AES-GCM Encrypt request with a server-generated 96-bit IV.
pub(crate) fn encrypt_request(unique_identifier: &str, plaintext: &[u8]) -> EncryptRequestPayload {
    EncryptRequestPayload {
        unique_identifier: Some(UniqueIdentifier(unique_identifier.to_string())),
        cryptographic_parameters: Some(aes_gcm_parameters(true, GCM_IV_LEN_BITS, GCM_TAG_LEN)),
        data: Data(plaintext.to_vec()),
        iv_counter_nonce: None,
    }
}

/// The single-part AES-GCM Decrypt request for `ciphertext`.
pub(crate) fn decrypt_request(
    unique_identifier: &str,
    ciphertext: &AeadCiphertext,
) -> Result<DecryptRequestPayload, KmipError> {
    let too_long = |what: &str| KmipError::Protocol {
        operation: Operation::Decrypt,
        detail: format!("{what} is too long to describe"),
    };
    let tag_length =
        i32::try_from(ciphertext.tag.len()).map_err(|_| too_long("authentication tag"))?;
    let iv_length_bits = i32::try_from(ciphertext.iv.len() * 8).map_err(|_| too_long("IV"))?;
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

/// Serializes a single-item KMIP 1.4 request message carrying `payload`.
pub(crate) fn encode_request<P: Serialize>(
    operation: Operation,
    payload: &P,
) -> kmip_ttlv::error::Result<Vec<u8>> {
    // The bound fits comfortably in an i32; the field is signed on the wire.
    let maximum_response_size = wire::MaximumResponseSize(MAX_RESPONSE_BYTES as i32);
    let message = RequestMessage {
        header: RequestHeader {
            protocol_version: PROTOCOL_VERSION_1_4,
            maximum_response_size: Some(maximum_response_size),
            batch_count: BatchCount(1),
        },
        batch_items: vec![RequestBatchItem { operation, payload }],
    };
    kmip_ttlv::to_vec(&message)
}

/// Decodes a response frame into the payload of `operation`, turning a
/// non-Success result into [`KmipError::Refused`].
pub(crate) fn decode_response<Response: DeserializeOwned>(
    operation: Operation,
    frame: &[u8],
) -> Result<Response, KmipError> {
    let message: ResponseMessage<Response> = match kmip_ttlv::from_slice(frame) {
        Ok(message) => message,
        Err(decode_error) => {
            // A refusal may carry a payload shaped unlike the expected one;
            // read the result status with the payload skipped before giving
            // up on the frame.
            if let Ok(outline) = kmip_ttlv::from_slice::<ResponseMessage<IgnoredPayload>>(frame)
                && let Ok(item) = single_item(operation, outline)
                && item.result_status != ResultStatus::Success
            {
                return Err(refusal(operation, item));
            }
            return Err(KmipError::Protocol {
                operation,
                detail: format!("failed to decode response: {decode_error}"),
            });
        }
    };
    let item = single_item(operation, message)?;
    if item.result_status != ResultStatus::Success {
        return Err(refusal(operation, item));
    }
    item.payload.ok_or_else(|| KmipError::Protocol {
        operation,
        detail: "successful response carries no payload".to_string(),
    })
}

/// Checks the envelope and returns the one batch item it must contain.
fn single_item<P>(
    operation: Operation,
    message: ResponseMessage<P>,
) -> Result<ResponseBatchItem<P>, KmipError> {
    let violation = |detail: String| KmipError::Protocol { operation, detail };
    if message.header.batch_count.0 != 1 {
        return Err(violation(format!(
            "response announces {} batch items, expected 1",
            message.header.batch_count.0
        )));
    }
    let mut items = message.batch_items;
    if items.len() != 1 {
        return Err(violation(format!(
            "response carries {} batch items, expected 1",
            items.len()
        )));
    }
    let item = items.remove(0);
    if let Some(echoed) = item.operation
        && echoed != operation
    {
        return Err(violation(format!(
            "response is for {echoed}, expected {operation}"
        )));
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
