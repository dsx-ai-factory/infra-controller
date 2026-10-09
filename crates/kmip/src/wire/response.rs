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
//! Response messages. The envelope is generic over the payload: the client
//! deserializes it with the payload struct of the operation it sent, and the
//! mock server serializes it from the same struct.

use serde::{Deserialize, Serialize};

use super::common::{
    AsynchronousCorrelationValue, AttestationType, Attribute, AuthenticatedEncryptionTag,
    BatchCount, ClientCorrelationValue, CorrelationValue, Data, IvCounterNonce, NeverPresent,
    Nonce, ObjectType, Operation, ProtocolVersion, ResultMessage, ResultReason, ResultStatus,
    ServerCorrelationValue, ServerInformation, TimeStamp, UniqueBatchItemId, UniqueIdentifier,
    VendorIdentification,
};

/// Response Message, KMIP 1.4 section 7.1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007B")]
pub(crate) struct ResponseMessage<P> {
    #[serde(rename = "0x42007A")]
    pub(crate) header: ResponseHeader,
    #[serde(rename = "0x42000F")]
    pub(crate) batch_items: Vec<ResponseBatchItem<P>>,
}

/// Response Header, KMIP 1.4 section 7.2. Every optional field the
/// specification allows between Time Stamp and Batch Count is modelled so a
/// server that sends one does not derail positional matching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007A")]
pub(crate) struct ResponseHeader {
    #[serde(rename = "0x420069")]
    pub(crate) protocol_version: ProtocolVersion,
    #[serde(rename = "0x420092")]
    pub(crate) time_stamp: TimeStamp,
    #[serde(rename = "0x4200C8", skip_serializing_if = "Option::is_none")]
    pub(crate) nonce: Option<Nonce>,
    #[serde(rename = "0x4200C7", skip_serializing_if = "Option::is_none")]
    pub(crate) attestation_types: Option<Vec<AttestationType>>,
    #[serde(rename = "0x420105", skip_serializing_if = "Option::is_none")]
    pub(crate) client_correlation_value: Option<ClientCorrelationValue>,
    #[serde(rename = "0x420106", skip_serializing_if = "Option::is_none")]
    pub(crate) server_correlation_value: Option<ServerCorrelationValue>,
    #[serde(rename = "0x42000D")]
    pub(crate) batch_count: BatchCount,
}

/// Response Batch Item, KMIP 1.4 section 7.2. A trailing Message Extension is
/// skipped by the deserializer as an unknown item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42000F")]
pub(crate) struct ResponseBatchItem<P> {
    #[serde(rename = "0x42005C", skip_serializing_if = "Option::is_none")]
    pub(crate) operation: Option<Operation>,
    #[serde(rename = "0x420093", skip_serializing_if = "Option::is_none")]
    pub(crate) unique_batch_item_id: Option<UniqueBatchItemId>,
    #[serde(rename = "0x42007F")]
    pub(crate) result_status: ResultStatus,
    #[serde(rename = "0x42007E", skip_serializing_if = "Option::is_none")]
    pub(crate) result_reason: Option<ResultReason>,
    #[serde(rename = "0x42007D", skip_serializing_if = "Option::is_none")]
    pub(crate) result_message: Option<ResultMessage>,
    #[serde(rename = "0x420006", skip_serializing_if = "Option::is_none")]
    pub(crate) asynchronous_correlation_value: Option<AsynchronousCorrelationValue>,
    #[serde(rename = "0x42007C", skip_serializing_if = "Option::is_none")]
    pub(crate) payload: Option<P>,
}

/// Discover Versions response payload, KMIP 1.4 section 4.26.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct DiscoverVersionsResponsePayload {
    #[serde(rename = "0x420069", skip_serializing_if = "Option::is_none")]
    pub(crate) protocol_versions: Option<Vec<ProtocolVersion>>,
}

/// Query response payload, KMIP 1.4 section 4.25, up to Server Information;
/// later optional fields are skipped as unknown trailing items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct QueryResponsePayload {
    #[serde(rename = "0x42005C", skip_serializing_if = "Option::is_none")]
    pub(crate) operations: Option<Vec<Operation>>,
    #[serde(rename = "0x420057", skip_serializing_if = "Option::is_none")]
    pub(crate) object_types: Option<Vec<ObjectType>>,
    #[serde(rename = "0x42009D", skip_serializing_if = "Option::is_none")]
    pub(crate) vendor_identification: Option<VendorIdentification>,
    #[serde(rename = "0x420088", skip_serializing_if = "Option::is_none")]
    pub(crate) server_information: Option<ServerInformation>,
}

/// Get Attributes response payload, KMIP 1.4 section 4.11.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct GetAttributesResponsePayload {
    #[serde(rename = "0x420094")]
    pub(crate) unique_identifier: UniqueIdentifier,
    #[serde(rename = "0x420008", skip_serializing_if = "Option::is_none")]
    pub(crate) attributes: Option<Vec<Attribute>>,
}

/// Encrypt response payload, KMIP 1.4 section 4.29.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct EncryptResponsePayload {
    #[serde(rename = "0x420094")]
    pub(crate) unique_identifier: UniqueIdentifier,
    #[serde(rename = "0x4200C2")]
    pub(crate) data: Data,
    #[serde(rename = "0x42003D", skip_serializing_if = "Option::is_none")]
    pub(crate) iv_counter_nonce: Option<IvCounterNonce>,
    #[serde(rename = "0x4200D6", skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_value: Option<CorrelationValue>,
    #[serde(rename = "0x4200FF", skip_serializing_if = "Option::is_none")]
    pub(crate) authenticated_encryption_tag: Option<AuthenticatedEncryptionTag>,
}

/// Decrypt response payload, KMIP 1.4 section 4.30.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct DecryptResponsePayload {
    #[serde(rename = "0x420094")]
    pub(crate) unique_identifier: UniqueIdentifier,
    #[serde(rename = "0x4200C2")]
    pub(crate) data: Data,
    #[serde(rename = "0x4200D6", skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_value: Option<CorrelationValue>,
}

/// A payload whose contents are skipped. Used to read the result status of a
/// response whose payload did not decode as the expected type, and by the
/// mock server for failure responses that carry no payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "0x42007C")]
pub(crate) struct IgnoredPayload {
    #[serde(rename = "0x54FFFF", skip_serializing_if = "Option::is_none")]
    pub(crate) never_present: Option<NeverPresent>,
}
