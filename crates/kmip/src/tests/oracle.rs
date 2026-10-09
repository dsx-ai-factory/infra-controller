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
//! A small TTLV encoder written from KMIP 1.4 section 9.1.1 without the serde
//! layer, so vectors built with it check the wire types independently.

use carbide_test_support::{Check, check_values};

const TYPE_STRUCTURE: u8 = 0x01;
const TYPE_INTEGER: u8 = 0x02;
const TYPE_LONG_INTEGER: u8 = 0x03;
const TYPE_ENUMERATION: u8 = 0x05;
const TYPE_BOOLEAN: u8 = 0x06;
const TYPE_TEXT_STRING: u8 = 0x07;
const TYPE_BYTE_STRING: u8 = 0x08;
const TYPE_DATE_TIME: u8 = 0x09;
const TYPE_INTERVAL: u8 = 0x0A;

/// One TTLV item: 3-byte tag, 1-byte type, 4-byte length of the unpadded
/// value, the value, then zero padding to an 8-byte boundary for every type
/// except Structure, whose children are already aligned.
fn item(tag: u32, item_type: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + value.len() + 8);
    out.extend_from_slice(&tag.to_be_bytes()[1..]);
    out.push(item_type);
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
    if item_type != TYPE_STRUCTURE {
        let padding = (8 - value.len() % 8) % 8;
        out.extend(std::iter::repeat_n(0u8, padding));
    }
    out
}

pub(super) fn structure(tag: u32, children: &[Vec<u8>]) -> Vec<u8> {
    item(tag, TYPE_STRUCTURE, &children.concat())
}

pub(super) fn integer(tag: u32, value: i32) -> Vec<u8> {
    item(tag, TYPE_INTEGER, &value.to_be_bytes())
}

pub(super) fn long_integer(tag: u32, value: i64) -> Vec<u8> {
    item(tag, TYPE_LONG_INTEGER, &value.to_be_bytes())
}

pub(super) fn enumeration(tag: u32, value: u32) -> Vec<u8> {
    item(tag, TYPE_ENUMERATION, &value.to_be_bytes())
}

pub(super) fn boolean(tag: u32, value: bool) -> Vec<u8> {
    item(tag, TYPE_BOOLEAN, &u64::from(value).to_be_bytes())
}

pub(super) fn text(tag: u32, value: &str) -> Vec<u8> {
    item(tag, TYPE_TEXT_STRING, value.as_bytes())
}

pub(super) fn bytes(tag: u32, value: &[u8]) -> Vec<u8> {
    item(tag, TYPE_BYTE_STRING, value)
}

pub(super) fn date_time(tag: u32, seconds: i64) -> Vec<u8> {
    item(tag, TYPE_DATE_TIME, &seconds.to_be_bytes())
}

pub(super) fn interval(tag: u32, seconds: u32) -> Vec<u8> {
    item(tag, TYPE_INTERVAL, &seconds.to_be_bytes())
}

fn from_spec_hex(spec: &str) -> Vec<u8> {
    hex::decode(spec.replace([' ', '|'], "")).expect("spec example hex")
}

// Verifies that the oracle reproduces the encoding examples of KMIP 1.4
// section 9.1.2, so the vectors it builds are an independent check of the
// serde wire types rather than a restatement of them.
#[test]
fn oracle_matches_the_specification_examples() {
    check_values(
        [
            Check {
                scenario: "integer 8",
                input: integer(0x420020, 8),
                expect: from_spec_hex("42 00 20 | 02 | 00 00 00 04 | 00 00 00 08 00 00 00 00"),
            },
            Check {
                scenario: "long integer 123456789000000000",
                input: long_integer(0x420020, 123_456_789_000_000_000),
                expect: from_spec_hex("42 00 20 | 03 | 00 00 00 08 | 01 B6 9B 4B A5 74 92 00"),
            },
            Check {
                scenario: "enumeration 255",
                input: enumeration(0x420020, 255),
                expect: from_spec_hex("42 00 20 | 05 | 00 00 00 04 | 00 00 00 FF 00 00 00 00"),
            },
            Check {
                scenario: "boolean true",
                input: boolean(0x420020, true),
                expect: from_spec_hex("42 00 20 | 06 | 00 00 00 08 | 00 00 00 00 00 00 00 01"),
            },
            Check {
                scenario: "text string Hello World",
                input: text(0x420020, "Hello World"),
                expect: from_spec_hex(
                    "42 00 20 | 07 | 00 00 00 0B | 48 65 6C 6C 6F 20 57 6F 72 6C 64 00 00 00 00 00",
                ),
            },
            Check {
                scenario: "byte string 01 02 03",
                input: bytes(0x420020, &[1, 2, 3]),
                expect: from_spec_hex("42 00 20 | 08 | 00 00 00 03 | 01 02 03 00 00 00 00 00"),
            },
            Check {
                scenario: "date-time 2008-03-14T11:56:40Z",
                input: date_time(0x420020, 0x47DA_67F8),
                expect: from_spec_hex("42 00 20 | 09 | 00 00 00 08 | 00 00 00 00 47 DA 67 F8"),
            },
            Check {
                scenario: "interval of 10 days",
                input: interval(0x420020, 864_000),
                expect: from_spec_hex("42 00 20 | 0A | 00 00 00 04 | 00 0D 2F 00 00 00 00 00"),
            },
            Check {
                scenario: "structure of an enumeration and an integer",
                input: structure(
                    0x420020,
                    &[enumeration(0x420004, 254), integer(0x420005, 255)],
                ),
                expect: from_spec_hex(
                    "42 00 20 | 01 | 00 00 00 20 | 42 00 04 | 05 | 00 00 00 04 | 00 00 00 FE \
                     00 00 00 00 | 42 00 05 | 02 | 00 00 00 04 | 00 00 00 FF 00 00 00 00",
                ),
            },
        ],
        |encoded| encoded,
    );
}
