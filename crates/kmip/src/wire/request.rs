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
//! Request messages. The envelope is generic over the payload so a request
//! serializes from the concrete payload struct; the mock server deserializes
//! the same envelope with [`RequestPayload`] selecting the payload by the
//! Operation value.

use serde::{Deserialize, Serialize};

use super::common::{
    AttributeName, AuthenticatedEncryptionTag, BatchCount, CryptographicParameters, Data,
    IvCounterNonce, MaximumResponseSize, Operation, ProtocolVersion, ProtocolVersionMajor,
    ProtocolVersionMinor, QueryFunction, UniqueIdentifier,
};

/// The protocol version this crate speaks.
pub(crate) const PROTOCOL_VERSION_1_4: ProtocolVersion = ProtocolVersion {
    major: ProtocolVersionMajor(1),
    minor: ProtocolVersionMinor(4),
};

/// Request Message, KMIP 1.4 section 7.1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420078")]
pub(crate) struct RequestMessage<P> {
    #[serde(rename = "0x420077")]
    pub(crate) header: RequestHeader,
    #[serde(rename = "0x42000F")]
    pub(crate) batch_items: Vec<RequestBatchItem<P>>,
}

/// Request Header, KMIP 1.4 section 7.2, limited to the fields this crate
/// sends. Authentication is carried by the TLS client certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420077")]
pub(crate) struct RequestHeader {
    #[serde(rename = "0x420069")]
    pub(crate) protocol_version: ProtocolVersion,
    #[serde(rename = "0x420050", skip_serializing_if = "Option::is_none")]
    pub(crate) maximum_response_size: Option<MaximumResponseSize>,
    #[serde(rename = "0x42000D")]
    pub(crate) batch_count: BatchCount,
}

/// Request Batch Item, KMIP 1.4 section 7.2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42000F")]
pub(crate) struct RequestBatchItem<P> {
    #[serde(rename = "0x42005C")]
    pub(crate) operation: Operation,
    #[serde(rename = "0x420079")]
    pub(crate) payload: P,
}

/// Discover Versions request payload, KMIP 1.4 section 4.26. No list asks
/// the server for every version it supports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420079")]
pub(crate) struct DiscoverVersionsRequestPayload {
    #[serde(rename = "0x420069", skip_serializing_if = "Option::is_none")]
    pub(crate) protocol_versions: Option<Vec<ProtocolVersion>>,
}

/// Query request payload, KMIP 1.4 section 4.25.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420079")]
pub(crate) struct QueryRequestPayload {
    #[serde(rename = "0x420074")]
    pub(crate) query_functions: Vec<QueryFunction>,
}

/// Get Attributes request payload, KMIP 1.4 section 4.11.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420079")]
pub(crate) struct GetAttributesRequestPayload {
    #[serde(rename = "0x420094", skip_serializing_if = "Option::is_none")]
    pub(crate) unique_identifier: Option<UniqueIdentifier>,
    #[serde(rename = "0x42000A", skip_serializing_if = "Option::is_none")]
    pub(crate) attribute_names: Option<Vec<AttributeName>>,
}

/// Encrypt request payload, KMIP 1.4 section 4.29, single-part only: the
/// Correlation Value, Init Indicator, Final Indicator and Authenticated
/// Encryption Additional Data fields are never sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420079")]
pub(crate) struct EncryptRequestPayload {
    #[serde(rename = "0x420094", skip_serializing_if = "Option::is_none")]
    pub(crate) unique_identifier: Option<UniqueIdentifier>,
    #[serde(rename = "0x42002B", skip_serializing_if = "Option::is_none")]
    pub(crate) cryptographic_parameters: Option<CryptographicParameters>,
    #[serde(rename = "0x4200C2")]
    pub(crate) data: Data,
    #[serde(rename = "0x42003D", skip_serializing_if = "Option::is_none")]
    pub(crate) iv_counter_nonce: Option<IvCounterNonce>,
}

/// Decrypt request payload, KMIP 1.4 section 4.30, single-part only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x420079")]
pub(crate) struct DecryptRequestPayload {
    #[serde(rename = "0x420094", skip_serializing_if = "Option::is_none")]
    pub(crate) unique_identifier: Option<UniqueIdentifier>,
    #[serde(rename = "0x42002B", skip_serializing_if = "Option::is_none")]
    pub(crate) cryptographic_parameters: Option<CryptographicParameters>,
    #[serde(rename = "0x4200C2")]
    pub(crate) data: Data,
    #[serde(rename = "0x42003D", skip_serializing_if = "Option::is_none")]
    pub(crate) iv_counter_nonce: Option<IvCounterNonce>,
    #[serde(rename = "0x4200FF", skip_serializing_if = "Option::is_none")]
    pub(crate) authenticated_encryption_tag: Option<AuthenticatedEncryptionTag>,
}

/// A request payload selected by the Operation value seen earlier in the
/// batch item. Deserialization only; used by the mock server.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) enum RequestPayload {
    #[serde(rename = "if 0x42005C==0x0000001E")]
    DiscoverVersions(DiscoverVersionsRequestPayload),
    #[serde(rename = "if 0x42005C==0x00000018")]
    Query(QueryRequestPayload),
    #[serde(rename = "if 0x42005C==0x0000000B")]
    GetAttributes(GetAttributesRequestPayload),
    #[serde(rename = "if 0x42005C==0x0000001F")]
    Encrypt(EncryptRequestPayload),
    #[serde(rename = "if 0x42005C==0x00000020")]
    Decrypt(DecryptRequestPayload),
}
