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
//! Wire vectors: the requests this client sends and the responses it decodes,
//! checked against frames built with the independent TTLV oracle.

use carbide_test_support::{Check, check_values};

use super::{
    Correlation, decode_response, decrypt_request, discover_versions_request, encode_request,
    encrypt_request, get_attributes_request, key_attributes_from, query_request,
};
use crate::tests::oracle::{
    boolean, bytes, date_time, enumeration, integer, interval, long_integer, structure, text,
};
use crate::wire::{
    DecryptResponsePayload, DiscoverVersionsResponsePayload, EncryptResponsePayload,
    GetAttributesResponsePayload, QueryResponsePayload,
};
use crate::{AeadCiphertext, KmipError, Operation};

const UID: &str = "kek-1";
const BATCH_ID: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0x2A];
const DEK: [u8; 32] = [0x11; 32];
const IV: [u8; 12] = [0x22; 12];
const LONG_IV: [u8; 16] = [0x22; 16];
const TAG: [u8; 16] = [0x33; 16];
const CIPHERTEXT: [u8; 32] = [0x44; 32];

const OP_GET_ATTRIBUTES: u32 = 0x0B;
const OP_QUERY: u32 = 0x18;
const OP_DISCOVER_VERSIONS: u32 = 0x1E;
const OP_ENCRYPT: u32 = 0x1F;
const OP_DECRYPT: u32 = 0x20;

const STATUS_SUCCESS: u32 = 0;
const STATUS_OPERATION_FAILED: u32 = 1;
const STATUS_OPERATION_PENDING: u32 = 2;

const V1_4: (i32, i32) = (1, 4);

fn protocol_version((major, minor): (i32, i32)) -> Vec<u8> {
    structure(
        0x420069,
        &[integer(0x42006A, major), integer(0x42006B, minor)],
    )
}

/// The request envelope this crate sends: KMIP 1.4, a 1 MiB maximum response
/// size, one batch item carrying the Unique Batch Item ID.
fn request(operation: u32, payload: &[Vec<u8>]) -> Vec<u8> {
    structure(
        0x420078,
        &[
            structure(
                0x420077,
                &[
                    protocol_version(V1_4),
                    integer(0x420050, 1_048_576),
                    integer(0x42000D, 1),
                ],
            ),
            structure(
                0x42000F,
                &[
                    enumeration(0x42005C, operation),
                    bytes(0x420093, &BATCH_ID),
                    structure(0x420079, payload),
                ],
            ),
        ],
    )
}

/// Cryptographic Parameters for AES-GCM, no padding, an IV of `iv_bits`, a
/// 16-byte tag, and a server-generated IV when `random_iv` is set.
fn aes_gcm_parameters(random_iv: bool, iv_bits: i32) -> Vec<u8> {
    let mut children = vec![
        enumeration(0x420011, 9),
        enumeration(0x42005F, 1),
        enumeration(0x420028, 3),
    ];
    if random_iv {
        children.push(boolean(0x4200C5, true));
    }
    children.push(integer(0x4200CD, iv_bits));
    children.push(integer(0x4200CE, 16));
    structure(0x42002B, &children)
}

fn ciphertext_with_iv(iv: &[u8]) -> AeadCiphertext {
    AeadCiphertext {
        iv: iv.to_vec(),
        ciphertext: CIPHERTEXT.to_vec(),
        tag: TAG.to_vec(),
    }
}

// The Decrypt rows pin that the parameters describe the IV actually carried:
// a server that returned a 16-byte IV must be told IV Length 128 on the way
// back, or a strict server refuses the unwrap.
#[test]
fn requests_encode_as_the_specification_lays_them_out() {
    check_values(
        [
            Check {
                scenario: "discover versions asks for every version",
                input: encode_request(
                    Operation::DiscoverVersions,
                    &BATCH_ID,
                    &discover_versions_request(),
                ),
                expect: hex::encode_upper(request(OP_DISCOVER_VERSIONS, &[])),
            },
            Check {
                scenario: "query asks for operations and server information",
                input: encode_request(Operation::Query, &BATCH_ID, &query_request()),
                expect: hex::encode_upper(request(
                    OP_QUERY,
                    &[enumeration(0x420074, 1), enumeration(0x420074, 3)],
                )),
            },
            Check {
                scenario: "get attributes names the four key attributes",
                input: encode_request(
                    Operation::GetAttributes,
                    &BATCH_ID,
                    &get_attributes_request(UID),
                ),
                expect: hex::encode_upper(request(
                    OP_GET_ATTRIBUTES,
                    &[
                        text(0x420094, UID),
                        text(0x42000A, "State"),
                        text(0x42000A, "Cryptographic Algorithm"),
                        text(0x42000A, "Cryptographic Length"),
                        text(0x42000A, "Cryptographic Usage Mask"),
                    ],
                )),
            },
            Check {
                scenario: "encrypt is single-part AES-GCM with a server-generated 96-bit IV",
                input: encode_request(Operation::Encrypt, &BATCH_ID, &encrypt_request(UID, &DEK)),
                expect: hex::encode_upper(request(
                    OP_ENCRYPT,
                    &[
                        text(0x420094, UID),
                        aes_gcm_parameters(true, 96),
                        bytes(0x4200C2, &DEK),
                    ],
                )),
            },
            Check {
                scenario: "decrypt carries the IV and the tag as separate fields",
                input: encode_request(
                    Operation::Decrypt,
                    &BATCH_ID,
                    &decrypt_request(UID, &ciphertext_with_iv(&IV)).expect("decrypt request"),
                ),
                expect: hex::encode_upper(request(
                    OP_DECRYPT,
                    &[
                        text(0x420094, UID),
                        aes_gcm_parameters(false, 96),
                        bytes(0x4200C2, &CIPHERTEXT),
                        bytes(0x42003D, &IV),
                        bytes(0x4200FF, &TAG),
                    ],
                )),
            },
            Check {
                scenario: "decrypt describes a 16-byte IV as 128 bits",
                input: encode_request(
                    Operation::Decrypt,
                    &BATCH_ID,
                    &decrypt_request(UID, &ciphertext_with_iv(&LONG_IV)).expect("decrypt request"),
                ),
                expect: hex::encode_upper(request(
                    OP_DECRYPT,
                    &[
                        text(0x420094, UID),
                        aes_gcm_parameters(false, 128),
                        bytes(0x4200C2, &CIPHERTEXT),
                        bytes(0x42003D, &LONG_IV),
                        bytes(0x4200FF, &TAG),
                    ],
                )),
            },
        ],
        |encoded| hex::encode_upper(encoded.expect("request serializes")),
    );
}

/// A response envelope: a header declaring `version` and carrying
/// `header_extras` between Time Stamp and Batch Count, then `items`.
fn response(
    version: (i32, i32),
    header_extras: &[Vec<u8>],
    batch_count: i32,
    items: &[Vec<u8>],
) -> Vec<u8> {
    let mut header = vec![
        protocol_version(version),
        date_time(0x420092, 1_700_000_000),
    ];
    header.extend_from_slice(header_extras);
    header.push(integer(0x42000D, batch_count));
    let mut children = vec![structure(0x42007A, &header)];
    children.extend_from_slice(items);
    structure(0x42007B, &children)
}

/// A batch item answering the request: operation, the echoed Unique Batch
/// Item ID, then `rest`.
fn item(operation: u32, rest: &[Vec<u8>]) -> Vec<u8> {
    let mut children = vec![enumeration(0x42005C, operation), bytes(0x420093, &BATCH_ID)];
    children.extend_from_slice(rest);
    structure(0x42000F, &children)
}

fn success_item(operation: u32, payload: &[Vec<u8>]) -> Vec<u8> {
    item(
        operation,
        &[
            enumeration(0x42007F, STATUS_SUCCESS),
            structure(0x42007C, payload),
        ],
    )
}

fn failure_item(operation: u32, reason: u32, message: &str, payload: Option<Vec<u8>>) -> Vec<u8> {
    let mut rest = vec![
        enumeration(0x42007F, STATUS_OPERATION_FAILED),
        enumeration(0x42007E, reason),
        text(0x42007D, message),
    ];
    rest.extend(payload);
    item(operation, &rest)
}

fn attribute(name: &str, value: Vec<u8>) -> Vec<u8> {
    structure(0x420008, &[text(0x42000A, name), value])
}

/// A Message Extension whose Vendor Extension holds an Interval, a type the
/// deserializer cannot decode as a field and so must step over by length.
fn message_extension(critical: bool) -> Vec<u8> {
    structure(
        0x420051,
        &[
            text(0x42009D, "vendor"),
            boolean(0x420026, critical),
            structure(
                0x42009C,
                &[
                    text(0x54000A, "hint"),
                    interval(0x54000B, 60),
                    integer(0x54000C, 7),
                ],
            ),
        ],
    )
}

fn encrypt_payload() -> Vec<Vec<u8>> {
    vec![
        text(0x420094, UID),
        bytes(0x4200C2, &CIPHERTEXT),
        bytes(0x42003D, &IV),
        bytes(0x4200FF, &TAG),
    ]
}

fn decrypt_payload() -> Vec<Vec<u8>> {
    vec![text(0x420094, UID), bytes(0x4200C2, &DEK)]
}

type Decoder = fn(&[u8]) -> Result<String, String>;

/// The request every response row answers: `UID`, the fixed batch item ID
/// and, for Encrypt, a plaintext as long as `CIPHERTEXT`.
fn correlation(operation: Operation) -> Correlation<'static> {
    Correlation {
        batch_item_id: &BATCH_ID,
        unique_identifier: Some(UID),
        plaintext_len: Some(CIPHERTEXT.len()),
        ..Correlation::new(operation)
    }
}

/// Renders an error so a row pins which check fired, not just that one did.
fn describe(error: KmipError) -> String {
    match error {
        KmipError::Refused {
            operation,
            status,
            reason,
            message,
        } => format!("refused {operation} {status} {reason:?} {message:?}"),
        KmipError::Protocol { detail, .. } => format!("protocol violation: {detail}"),
        other => format!("unexpected {other:?}"),
    }
}

fn decode_encrypt(frame: &[u8]) -> Result<String, String> {
    decode_response::<EncryptResponsePayload>(correlation(Operation::Encrypt), frame)
        .map(|sealed| {
            format!(
                "iv={} ciphertext={} tag={}",
                hex::encode_upper(&sealed.iv),
                hex::encode_upper(&sealed.ciphertext),
                hex::encode_upper(&sealed.tag),
            )
        })
        .map_err(describe)
}

fn decode_decrypt(frame: &[u8]) -> Result<String, String> {
    decode_response::<DecryptResponsePayload>(correlation(Operation::Decrypt), frame)
        .map(|payload| {
            format!(
                "uid={} data={}",
                payload.unique_identifier.0,
                hex::encode_upper(&payload.data.0)
            )
        })
        .map_err(describe)
}

fn decode_query(frame: &[u8]) -> Result<String, String> {
    decode_response::<QueryResponsePayload>(correlation(Operation::Query), frame)
        .map(|payload| {
            format!(
                "operations={:?} vendor={:?}",
                payload.operations.unwrap_or_default(),
                payload.vendor_identification.map(|vendor| vendor.0)
            )
        })
        .map_err(describe)
}

fn decode_get_attributes(frame: &[u8]) -> Result<String, String> {
    decode_response::<GetAttributesResponsePayload>(correlation(Operation::GetAttributes), frame)
        .map(|payload| key_attributes_from(payload.attributes.unwrap_or_default()))
        .map(|attributes| {
            format!(
                "state={:?} algorithm={:?} length={:?} mask={:?}",
                attributes.state,
                attributes.algorithm,
                attributes.length_bits,
                attributes.usage_mask
            )
        })
        .map_err(describe)
}

fn decode_discover_versions(frame: &[u8]) -> Result<String, String> {
    decode_response::<DiscoverVersionsResponsePayload>(
        correlation(Operation::DiscoverVersions),
        frame,
    )
    .map(|payload| {
        let versions: Vec<_> = payload
            .protocol_versions
            .unwrap_or_default()
            .into_iter()
            .map(|version| (version.major.0, version.minor.0))
            .collect();
        format!("{versions:?}")
    })
    .map_err(describe)
}

// Response decoding against oracle-built frames: successful payloads of each
// operation, the refusal mapping, tolerance of optional items and
// non-critical extensions, and every check that ties a response to its
// request.
#[test]
fn responses_decode_or_are_classified() {
    let key_attributes =
        "state=Some(Active) algorithm=Some(Aes) length=Some(256) mask=Some(12)".to_string();
    let mut truncated = response(
        V1_4,
        &[],
        1,
        &[success_item(OP_ENCRYPT, &encrypt_payload())],
    );
    truncated.truncate(truncated.len() - 8);

    check_values(
        [
            Check {
                scenario: "encrypt success carries the IV and tag",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(OP_ENCRYPT, &encrypt_payload())],
                    ),
                ),
                expect: Ok(format!(
                    "iv={} ciphertext={} tag={}",
                    hex::encode_upper(IV),
                    hex::encode_upper(CIPHERTEXT),
                    hex::encode_upper(TAG)
                )),
            },
            Check {
                scenario: "decrypt success carries the plaintext",
                input: (
                    decode_decrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(OP_DECRYPT, &decrypt_payload())],
                    ),
                ),
                expect: Ok(format!("uid={UID} data={}", hex::encode_upper(DEK))),
            },
            Check {
                scenario: "an operation failure is a refusal with reason and message",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[failure_item(OP_ENCRYPT, 1, "object not found", None)],
                    ),
                ),
                expect: Err("refused Encrypt OperationFailed Some(ItemNotFound) \
                             Some(\"object not found\")"
                    .to_string()),
            },
            Check {
                scenario: "a refusal with an unexpected payload is still a refusal",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[failure_item(
                            OP_ENCRYPT,
                            0x0C,
                            "denied",
                            Some(structure(0x42007C, &[integer(0x54000A, 1)])),
                        )],
                    ),
                ),
                expect: Err(
                    "refused Encrypt OperationFailed Some(PermissionDenied) Some(\"denied\")"
                        .to_string(),
                ),
            },
            Check {
                scenario: "a pending result without reason or message keeps its status",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[item(
                            OP_ENCRYPT,
                            &[enumeration(0x42007F, STATUS_OPERATION_PENDING)],
                        )],
                    ),
                ),
                expect: Err("refused Encrypt OperationPending None None".to_string()),
            },
            Check {
                scenario: "query lists an extension operation as Other and skips \
                           vendor server information",
                input: (
                    decode_query as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_QUERY,
                            &[
                                enumeration(0x42005C, 0x8000_0001),
                                enumeration(0x42005C, OP_ENCRYPT),
                                text(0x42009D, "Vendor X"),
                                structure(
                                    0x420088,
                                    &[text(0x54000A, "build 1"), integer(0x54000B, 7)],
                                ),
                            ],
                        )],
                    ),
                ),
                expect: Ok("operations=[Other, Encrypt] vendor=Some(\"Vendor X\")".to_string()),
            },
            Check {
                scenario: "get attributes selects each value type by attribute name",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_GET_ATTRIBUTES,
                            &[
                                text(0x420094, UID),
                                attribute("State", enumeration(0x42000B, 2)),
                                attribute("Cryptographic Algorithm", enumeration(0x42000B, 3)),
                                attribute("Cryptographic Length", integer(0x42000B, 256)),
                                attribute("Cryptographic Usage Mask", integer(0x42000B, 12)),
                            ],
                        )],
                    ),
                ),
                expect: Ok(key_attributes.clone()),
            },
            Check {
                scenario: "get attributes skips attributes that were not asked for",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_GET_ATTRIBUTES,
                            &[
                                text(0x420094, UID),
                                attribute("Object Type", enumeration(0x42000B, 2)),
                                attribute("State", enumeration(0x42000B, 2)),
                                attribute(
                                    "Name",
                                    structure(
                                        0x42000B,
                                        &[text(0x420055, "kek-1"), enumeration(0x420054, 1)],
                                    ),
                                ),
                                attribute("Cryptographic Algorithm", enumeration(0x42000B, 3)),
                                attribute("Initial Date", date_time(0x42000B, 1_700_000_000)),
                                attribute("x-rotation-count", integer(0x42000B, 3)),
                                attribute("Cryptographic Length", integer(0x42000B, 256)),
                                attribute("x-bytes-used", long_integer(0x42000B, 1 << 40)),
                                attribute("Sensitive", boolean(0x42000B, true)),
                                attribute("Object Group", text(0x42000B, "kek")),
                                attribute("x-fingerprint", bytes(0x42000B, &[0xAB, 0xCD])),
                                attribute("Cryptographic Usage Mask", integer(0x42000B, 12)),
                            ],
                        )],
                    ),
                ),
                expect: Ok(key_attributes),
            },
            Check {
                scenario: "an attribute carrying an index still decodes",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_GET_ATTRIBUTES,
                            &[
                                text(0x420094, UID),
                                structure(
                                    0x420008,
                                    &[
                                        text(0x42000A, "State"),
                                        integer(0x420009, 0),
                                        enumeration(0x42000B, 2),
                                    ],
                                ),
                            ],
                        )],
                    ),
                ),
                expect: Ok("state=Some(Active) algorithm=None length=None mask=None".to_string()),
            },
            Check {
                scenario: "attributes the server left out are reported as absent",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(OP_GET_ATTRIBUTES, &[text(0x420094, UID)])],
                    ),
                ),
                expect: Ok("state=None algorithm=None length=None mask=None".to_string()),
            },
            Check {
                scenario: "a requested attribute with a value of the wrong type does not decode",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_GET_ATTRIBUTES,
                            &[
                                text(0x420094, UID),
                                attribute("State", enumeration(0x42000B, 2)),
                                attribute("Cryptographic Length", long_integer(0x42000B, 256)),
                            ],
                        )],
                    ),
                ),
                expect: Err("protocol violation: response did not decode".to_string()),
            },
            Check {
                scenario: "an unrequested Interval attribute cannot be skipped (known limit)",
                input: (
                    decode_get_attributes as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_GET_ATTRIBUTES,
                            &[
                                text(0x420094, UID),
                                attribute("Lease Time", interval(0x42000B, 3600)),
                                attribute("State", enumeration(0x42000B, 2)),
                            ],
                        )],
                    ),
                ),
                expect: Err("protocol violation: response did not decode".to_string()),
            },
            Check {
                scenario: "optional header items are stepped over",
                input: (
                    decode_discover_versions as Decoder,
                    response(
                        V1_4,
                        &[
                            structure(
                                0x4200C8,
                                &[bytes(0x4200C9, b"id"), bytes(0x4200CA, b"nonce")],
                            ),
                            text(0x420106, "server-correlation"),
                        ],
                        1,
                        &[success_item(
                            OP_DISCOVER_VERSIONS,
                            &[protocol_version(V1_4), protocol_version((1, 2))],
                        )],
                    ),
                ),
                expect: Ok("[(1, 4), (1, 2)]".to_string()),
            },
            Check {
                scenario: "a non-critical message extension is skipped",
                input: (
                    decode_decrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[item(
                            OP_DECRYPT,
                            &[
                                enumeration(0x42007F, STATUS_SUCCESS),
                                structure(0x42007C, &decrypt_payload()),
                                message_extension(false),
                            ],
                        )],
                    ),
                ),
                expect: Ok(format!("uid={UID} data={}", hex::encode_upper(DEK))),
            },
            Check {
                scenario: "a critical message extension is a protocol violation",
                input: (
                    decode_decrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[item(
                            OP_DECRYPT,
                            &[
                                enumeration(0x42007F, STATUS_SUCCESS),
                                structure(0x42007C, &decrypt_payload()),
                                message_extension(true),
                            ],
                        )],
                    ),
                ),
                expect: Err(
                    "protocol violation: response carries a critical message extension \
                             from \"vendor\" that this client does not understand"
                        .to_string(),
                ),
            },
            Check {
                scenario: "discover versions answered in another protocol version still reports \
                           the list",
                input: (
                    decode_discover_versions as Decoder,
                    response(
                        (1, 2),
                        &[],
                        1,
                        &[success_item(
                            OP_DISCOVER_VERSIONS,
                            &[protocol_version((1, 2))],
                        )],
                    ),
                ),
                expect: Ok("[(1, 2)]".to_string()),
            },
            Check {
                scenario: "a refusal in another protocol version is still a refusal",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        (1, 2),
                        &[],
                        1,
                        &[failure_item(OP_ENCRYPT, 5, "unsupported", None)],
                    ),
                ),
                expect: Err(
                    "refused Encrypt OperationFailed Some(OperationNotSupported) \
                             Some(\"unsupported\")"
                        .to_string(),
                ),
            },
            Check {
                scenario: "a successful answer in another protocol version is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        (1, 2),
                        &[],
                        1,
                        &[success_item(OP_ENCRYPT, &encrypt_payload())],
                    ),
                ),
                expect: Err(
                    "protocol violation: response declares protocol version 1.2, \
                             expected 1.4"
                        .to_string(),
                ),
            },
            Check {
                scenario: "a batch count other than one is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        2,
                        &[success_item(OP_ENCRYPT, &encrypt_payload())],
                    ),
                ),
                expect: Err(
                    "protocol violation: response announces 2 batch items, expected 1".to_string(),
                ),
            },
            Check {
                scenario: "two batch items under a count of one is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[
                            success_item(OP_ENCRYPT, &encrypt_payload()),
                            success_item(OP_ENCRYPT, &encrypt_payload()),
                        ],
                    ),
                ),
                expect: Err(
                    "protocol violation: response carries 2 batch items, expected 1".to_string(),
                ),
            },
            Check {
                scenario: "a response for another operation is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(OP_DECRYPT, &decrypt_payload())],
                    ),
                ),
                expect: Err(
                    "protocol violation: response is for Decrypt, expected Encrypt".to_string(),
                ),
            },
            Check {
                scenario: "a refusal without the Unique Batch Item ID is still a refusal",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[structure(
                            0x42000F,
                            &[
                                enumeration(0x42005C, OP_ENCRYPT),
                                enumeration(0x42007F, STATUS_OPERATION_FAILED),
                                enumeration(0x42007E, 4),
                                text(0x42007D, "invalid message"),
                            ],
                        )],
                    ),
                ),
                expect: Err("refused Encrypt OperationFailed Some(InvalidMessage) \
                             Some(\"invalid message\")"
                    .to_string()),
            },
            Check {
                scenario: "a successful answer without the Unique Batch Item ID is a protocol \
                           violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[structure(
                            0x42000F,
                            &[
                                enumeration(0x42005C, OP_ENCRYPT),
                                enumeration(0x42007F, STATUS_SUCCESS),
                                structure(0x42007C, &encrypt_payload()),
                            ],
                        )],
                    ),
                ),
                expect: Err(
                    "protocol violation: response carries no Unique Batch Item ID".to_string(),
                ),
            },
            Check {
                scenario: "a response to another request's batch item is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[structure(
                            0x42000F,
                            &[
                                enumeration(0x42005C, OP_ENCRYPT),
                                bytes(0x420093, &[0, 0, 0, 0, 0, 0, 0, 0x2B]),
                                enumeration(0x42007F, STATUS_SUCCESS),
                                structure(0x42007C, &encrypt_payload()),
                            ],
                        )],
                    ),
                ),
                expect: Err(
                    "protocol violation: response carries another request's Unique \
                             Batch Item ID"
                        .to_string(),
                ),
            },
            Check {
                scenario: "a response naming another object is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(
                            OP_ENCRYPT,
                            &[
                                text(0x420094, "other"),
                                bytes(0x4200C2, &CIPHERTEXT),
                                bytes(0x42003D, &IV),
                                bytes(0x4200FF, &TAG),
                            ],
                        )],
                    ),
                ),
                expect: Err(
                    "protocol violation: response names object \"other\", expected \
                             \"kek-1\""
                        .to_string(),
                ),
            },
            Check {
                scenario: "a success without a payload is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[item(OP_ENCRYPT, &[enumeration(0x42007F, STATUS_SUCCESS)])],
                    ),
                ),
                expect: Err(
                    "protocol violation: successful response carries no payload".to_string()
                ),
            },
            Check {
                scenario: "a success whose payload lacks a required field is not a refusal",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        V1_4,
                        &[],
                        1,
                        &[success_item(OP_ENCRYPT, &[text(0x420094, UID)])],
                    ),
                ),
                expect: Err("protocol violation: response did not decode".to_string()),
            },
            Check {
                scenario: "a truncated frame is a protocol violation",
                input: (decode_encrypt as Decoder, truncated),
                expect: Err("protocol violation: response did not decode".to_string()),
            },
        ],
        |(decode, frame)| decode(&frame),
    );
}
