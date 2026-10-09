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
//! Wire vectors: the requests this crate sends and the responses it decodes,
//! checked against frames built with the independent TTLV oracle.

use carbide_test_support::{Check, check_values};

use super::oracle::{
    boolean, bytes, date_time, enumeration, integer, interval, long_integer, structure, text,
};
use crate::client::{
    decode_response, decrypt_request, discover_versions_request, encode_request, encrypt_request,
    get_attributes_request, key_attributes_from, query_request,
};
use crate::wire::{
    DecryptResponsePayload, DiscoverVersionsResponsePayload, EncryptResponsePayload,
    GetAttributesResponsePayload, QueryResponsePayload,
};
use crate::{AeadCiphertext, KmipError, Operation};

const UID: &str = "kek-1";
const DEK: [u8; 32] = [0x11; 32];
const IV: [u8; 12] = [0x22; 12];
const TAG: [u8; 16] = [0x33; 16];
const CIPHERTEXT: [u8; 32] = [0x44; 32];

const OP_GET_ATTRIBUTES: u32 = 0x0B;
const OP_QUERY: u32 = 0x18;
const OP_DISCOVER_VERSIONS: u32 = 0x1E;
const OP_ENCRYPT: u32 = 0x1F;
const OP_DECRYPT: u32 = 0x20;

fn protocol_version(major: i32, minor: i32) -> Vec<u8> {
    structure(
        0x420069,
        &[integer(0x42006A, major), integer(0x42006B, minor)],
    )
}

/// The request envelope this crate sends: KMIP 1.4, a 1 MiB maximum response
/// size, one batch item.
fn request(operation: u32, payload: &[Vec<u8>]) -> Vec<u8> {
    structure(
        0x420078,
        &[
            structure(
                0x420077,
                &[
                    protocol_version(1, 4),
                    integer(0x420050, 1_048_576),
                    integer(0x42000D, 1),
                ],
            ),
            structure(
                0x42000F,
                &[
                    enumeration(0x42005C, operation),
                    structure(0x420079, payload),
                ],
            ),
        ],
    )
}

/// Cryptographic Parameters for AES-GCM, no padding, a 96-bit IV, a 16-byte
/// tag, and a server-generated IV when `random_iv` is set.
fn aes_gcm_parameters(random_iv: bool) -> Vec<u8> {
    let mut children = vec![
        enumeration(0x420011, 9),
        enumeration(0x42005F, 1),
        enumeration(0x420028, 3),
    ];
    if random_iv {
        children.push(boolean(0x4200C5, true));
    }
    children.push(integer(0x4200CD, 96));
    children.push(integer(0x4200CE, 16));
    structure(0x42002B, &children)
}

// Verifies that each request the client sends is laid out as KMIP 1.4
// specifies: tags, types, field order, padding, and the fixed header.
#[test]
fn requests_encode_as_the_specification_lays_them_out() {
    let ciphertext = AeadCiphertext {
        iv: IV.to_vec(),
        ciphertext: CIPHERTEXT.to_vec(),
        tag: TAG.to_vec(),
    };
    check_values(
        [
            Check {
                scenario: "discover versions asks for every version",
                input: encode_request(Operation::DiscoverVersions, &discover_versions_request()),
                expect: hex::encode_upper(request(OP_DISCOVER_VERSIONS, &[])),
            },
            Check {
                scenario: "query asks for operations and server information",
                input: encode_request(Operation::Query, &query_request()),
                expect: hex::encode_upper(request(
                    OP_QUERY,
                    &[enumeration(0x420074, 1), enumeration(0x420074, 3)],
                )),
            },
            Check {
                scenario: "get attributes names the four key attributes",
                input: encode_request(Operation::GetAttributes, &get_attributes_request(UID)),
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
                input: encode_request(Operation::Encrypt, &encrypt_request(UID, &DEK)),
                expect: hex::encode_upper(request(
                    OP_ENCRYPT,
                    &[
                        text(0x420094, UID),
                        aes_gcm_parameters(true),
                        bytes(0x4200C2, &DEK),
                    ],
                )),
            },
            Check {
                scenario: "decrypt carries the IV and the tag as separate fields",
                input: encode_request(
                    Operation::Decrypt,
                    &decrypt_request(UID, &ciphertext).expect("decrypt request"),
                ),
                expect: hex::encode_upper(request(
                    OP_DECRYPT,
                    &[
                        text(0x420094, UID),
                        aes_gcm_parameters(false),
                        bytes(0x4200C2, &CIPHERTEXT),
                        bytes(0x42003D, &IV),
                        bytes(0x4200FF, &TAG),
                    ],
                )),
            },
        ],
        |encoded| hex::encode_upper(encoded.expect("request serializes")),
    );
}

/// A response envelope: a KMIP 1.4 header carrying `header_extras` between
/// Time Stamp and Batch Count, then `items`.
fn response(header_extras: &[Vec<u8>], batch_count: i32, items: &[Vec<u8>]) -> Vec<u8> {
    let mut header = vec![protocol_version(1, 4), date_time(0x420092, 1_700_000_000)];
    header.extend_from_slice(header_extras);
    header.push(integer(0x42000D, batch_count));
    let mut children = vec![structure(0x42007A, &header)];
    children.extend_from_slice(items);
    structure(0x42007B, &children)
}

fn success_item(operation: u32, payload: &[Vec<u8>]) -> Vec<u8> {
    structure(
        0x42000F,
        &[
            enumeration(0x42005C, operation),
            enumeration(0x42007F, 0),
            structure(0x42007C, payload),
        ],
    )
}

fn failure_item(operation: u32, reason: u32, message: &str, payload: Option<Vec<u8>>) -> Vec<u8> {
    let mut children = vec![
        enumeration(0x42005C, operation),
        enumeration(0x42007F, 1),
        enumeration(0x42007E, reason),
        text(0x42007D, message),
    ];
    children.extend(payload);
    structure(0x42000F, &children)
}

fn attribute(name: &str, value: Vec<u8>) -> Vec<u8> {
    structure(0x420008, &[text(0x42000A, name), value])
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

fn describe(error: KmipError) -> String {
    match error {
        KmipError::Refused {
            reason, message, ..
        } => format!("refused {reason:?} {message:?}"),
        KmipError::Protocol { .. } => "protocol violation".to_string(),
        other => format!("unexpected {other:?}"),
    }
}

fn decode_encrypt(frame: &[u8]) -> Result<String, String> {
    decode_response::<EncryptResponsePayload>(Operation::Encrypt, frame)
        .map(|payload| {
            format!(
                "uid={} iv={} ciphertext={} tag={}",
                payload.unique_identifier.0,
                payload
                    .iv_counter_nonce
                    .as_ref()
                    .map_or("-".to_string(), |iv| hex::encode_upper(&iv.0)),
                hex::encode_upper(&payload.data.0),
                payload
                    .authenticated_encryption_tag
                    .as_ref()
                    .map_or("-".to_string(), |tag| hex::encode_upper(&tag.0)),
            )
        })
        .map_err(describe)
}

fn decode_decrypt(frame: &[u8]) -> Result<String, String> {
    decode_response::<DecryptResponsePayload>(Operation::Decrypt, frame)
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
    decode_response::<QueryResponsePayload>(Operation::Query, frame)
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
    decode_response::<GetAttributesResponsePayload>(Operation::GetAttributes, frame)
        .and_then(|payload| key_attributes_from(payload.attributes.unwrap_or_default()))
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
    decode_response::<DiscoverVersionsResponsePayload>(Operation::DiscoverVersions, frame)
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

// Verifies response decoding against oracle-built frames: successful payloads
// of each operation, the refusal mapping, tolerance of optional header items
// and trailing extensions, and the protocol-violation boundaries.
#[test]
fn responses_decode_or_are_classified() {
    let message_extension = structure(
        0x420051,
        &[text(0x42009D, "vendor"), boolean(0x420026, false)],
    );
    let mut truncated = response(&[], 1, &[success_item(OP_ENCRYPT, &encrypt_payload())]);
    truncated.truncate(truncated.len() - 8);

    check_values(
        [
            Check {
                scenario: "encrypt success carries the IV and tag",
                input: (
                    decode_encrypt as Decoder,
                    response(&[], 1, &[success_item(OP_ENCRYPT, &encrypt_payload())]),
                ),
                expect: Ok(format!(
                    "uid={UID} iv={} ciphertext={} tag={}",
                    hex::encode_upper(IV),
                    hex::encode_upper(CIPHERTEXT),
                    hex::encode_upper(TAG)
                )),
            },
            Check {
                scenario: "decrypt success carries the plaintext",
                input: (
                    decode_decrypt as Decoder,
                    response(&[], 1, &[success_item(OP_DECRYPT, &decrypt_payload())]),
                ),
                expect: Ok(format!("uid={UID} data={}", hex::encode_upper(DEK))),
            },
            Check {
                scenario: "an operation failure is a refusal with reason and message",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        &[],
                        1,
                        &[failure_item(OP_ENCRYPT, 1, "object not found", None)],
                    ),
                ),
                expect: Err("refused Some(ItemNotFound) Some(\"object not found\")".to_string()),
            },
            Check {
                scenario: "a refusal with an unexpected payload is still a refusal",
                input: (
                    decode_encrypt as Decoder,
                    response(
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
                expect: Err("refused Some(PermissionDenied) Some(\"denied\")".to_string()),
            },
            Check {
                scenario: "query lists an extension operation as Other and skips \
                           vendor server information",
                input: (
                    decode_query as Decoder,
                    response(
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
                expect: Ok(
                    "state=Some(Active) algorithm=Some(Aes) length=Some(256) mask=Some(12)"
                        .to_string(),
                ),
            },
            Check {
                scenario: "get attributes skips attributes that were not asked for",
                input: (
                    decode_get_attributes as Decoder,
                    response(
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
                expect: Ok(
                    "state=Some(Active) algorithm=Some(Aes) length=Some(256) mask=Some(12)"
                        .to_string(),
                ),
            },
            Check {
                scenario: "a requested attribute with an unexpected type is a protocol violation",
                input: (
                    decode_get_attributes as Decoder,
                    response(
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
                expect: Err("protocol violation".to_string()),
            },
            Check {
                scenario: "an unrequested Interval attribute cannot be skipped (known limit)",
                input: (
                    decode_get_attributes as Decoder,
                    response(
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
                expect: Err("protocol violation".to_string()),
            },
            Check {
                scenario: "optional header items are stepped over",
                input: (
                    decode_discover_versions as Decoder,
                    response(
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
                            &[protocol_version(1, 4), protocol_version(1, 2)],
                        )],
                    ),
                ),
                expect: Ok("[(1, 4), (1, 2)]".to_string()),
            },
            Check {
                scenario: "a trailing message extension is skipped",
                input: (
                    decode_decrypt as Decoder,
                    response(
                        &[],
                        1,
                        &[structure(
                            0x42000F,
                            &[
                                enumeration(0x42005C, OP_DECRYPT),
                                enumeration(0x42007F, 0),
                                structure(0x42007C, &decrypt_payload()),
                                message_extension,
                            ],
                        )],
                    ),
                ),
                expect: Ok(format!("uid={UID} data={}", hex::encode_upper(DEK))),
            },
            Check {
                scenario: "a batch count other than one is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(&[], 2, &[success_item(OP_ENCRYPT, &encrypt_payload())]),
                ),
                expect: Err("protocol violation".to_string()),
            },
            Check {
                scenario: "a response for another operation is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(&[], 1, &[success_item(OP_DECRYPT, &decrypt_payload())]),
                ),
                expect: Err("protocol violation".to_string()),
            },
            Check {
                scenario: "a success without a payload is a protocol violation",
                input: (
                    decode_encrypt as Decoder,
                    response(
                        &[],
                        1,
                        &[structure(
                            0x42000F,
                            &[enumeration(0x42005C, OP_ENCRYPT), enumeration(0x42007F, 0)],
                        )],
                    ),
                ),
                expect: Err("protocol violation".to_string()),
            },
            Check {
                scenario: "a truncated frame is a protocol violation",
                input: (decode_encrypt as Decoder, truncated),
                expect: Err("protocol violation".to_string()),
            },
        ],
        |(decode, frame)| decode(&frame),
    );
}
