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
//! Framing: the response bound is enforced before the body is read, the body
//! is checked item by item, and the ways a connection can end are told apart
//! for the retry decision.

use std::io::{self, Cursor};

use carbide_test_support::value_scenarios;
use zeroize::Zeroizing;

use crate::framing::{FrameError, TAG_RESPONSE_MESSAGE, read_frame, write_frame};

const BOUND: u32 = 512;

/// Renders the outcome; a returned frame must be the bytes that were sent.
fn classify(sent: &[u8], result: Result<Zeroizing<Vec<u8>>, FrameError>) -> String {
    match result {
        Ok(frame) if *frame == sent => format!("intact frame of {} bytes", frame.len()),
        Ok(frame) => format!("corrupted frame of {} bytes", frame.len()),
        Err(FrameError::NoResponse(_)) => "no response".to_string(),
        Err(FrameError::Io(_)) => "io failure".to_string(),
        Err(FrameError::Protocol(_)) => "protocol violation".to_string(),
        Err(FrameError::TooLarge { length, max }) => format!("too large {length} > {max}"),
    }
}

fn header(tag: [u8; 3], item_type: u8, length: u32) -> Vec<u8> {
    let mut header = tag.to_vec();
    header.push(item_type);
    header.extend_from_slice(&length.to_be_bytes());
    header
}

fn response(length: u32) -> Vec<u8> {
    header(TAG_RESPONSE_MESSAGE, 0x01, length)
}

/// A response made of `depth` structures nested one inside the other, the
/// innermost empty.
fn nested(depth: u32) -> Vec<u8> {
    let mut frame = Vec::new();
    for level in (0..depth).rev() {
        let tag = if level == 0 {
            TAG_RESPONSE_MESSAGE
        } else {
            [0x42, 0x00, 0x7A]
        };
        frame = [header(tag, 0x01, frame.len() as u32), frame].concat();
    }
    frame
}

// The frame reader's contract with a 512-byte bound: whole frames come back
// byte for byte, an oversized announcement is rejected from the header alone,
// an ending connection is "no response" only before the first byte (which is
// what makes the client's single retry safe), and the item walk rejects what
// the decoder must never see.
#[test]
fn frames_are_bounded_and_connection_endings_are_classified() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    value_scenarios!(
        run = |bytes: Vec<u8>| classify(&bytes, runtime.block_on(read_frame(
            &mut Cursor::new(bytes.clone()),
            TAG_RESPONSE_MESSAGE,
            BOUND,
        )));
        "complete frames are returned whole, header included" {
            [response(16), header([0x42, 0x00, 0x0D], 0x02, 4), vec![0, 0, 0, 1, 0, 0, 0, 0]]
                .concat() => "intact frame of 24 bytes".to_string(),
            response(0) => "intact frame of 8 bytes".to_string(),
            // Exactly the bound: a 504-byte body holding one 496-byte Byte String.
            [response(504), header([0x42, 0x00, 0xC2], 0x08, 496), vec![0u8; 496]].concat()
                => "intact frame of 512 bytes".to_string(),
        }
        "an announced length above the bound is rejected without reading a body" {
            response(505) => "too large 513 > 512".to_string(),
        }
        "a connection that ends before any byte is no response" {
            Vec::<u8>::new() => "no response".to_string(),
        }
        "a connection that ends mid-frame is an io failure" {
            response(16)[..6].to_vec() => "io failure".to_string(),
            [response(16), vec![0u8; 8]].concat() => "io failure".to_string(),
        }
        "anything but a response structure is a protocol violation" {
            header([0x42, 0x00, 0x78], 0x01, 0) => "protocol violation".to_string(),
            header(TAG_RESPONSE_MESSAGE, 0x02, 0) => "protocol violation".to_string(),
        }
        "a nested item that claims more bytes than its frame holds is a protocol violation" {
            // A 16-byte structure holding a Byte String that advertises 4 GiB.
            [response(16), header([0x42, 0x00, 0xC2], 0x08, u32::MAX), vec![0u8; 8]].concat()
                => "protocol violation".to_string(),
            // An inner structure longer than its parent.
            [response(16), header([0x42, 0x00, 0x7A], 0x01, 24), vec![0u8; 8]].concat()
                => "protocol violation".to_string(),
        }
        "a nested item with an invalid type is a protocol violation" {
            [response(16), header([0x42, 0x00, 0x0D], 0x00, 4), vec![0u8; 8]].concat()
                => "protocol violation".to_string(),
            [response(16), header([0x42, 0x00, 0x0D], 0x0B, 4), vec![0u8; 8]].concat()
                => "protocol violation".to_string(),
        }
        "a nested item header cut short is a protocol violation" {
            [response(4), vec![0x42, 0x00, 0x0D, 0x02]].concat()
                => "protocol violation".to_string(),
        }
        "structures may nest 32 deep and no deeper" {
            nested(32) => "intact frame of 256 bytes".to_string(),
            nested(33) => "protocol violation".to_string(),
        }
    );
}

// A reset, unlike a clean close, surfaces as a read error; it is classified
// by when it happens just like a close, and a failed write counts as no
// response because the request may never have left.
#[tokio::test]
async fn connection_resets_are_classified_by_when_they_happen() {
    let reset = || io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");

    let mut before_any_byte = tokio_test::io::Builder::new().read_error(reset()).build();
    assert!(matches!(
        read_frame(&mut before_any_byte, TAG_RESPONSE_MESSAGE, BOUND).await,
        Err(FrameError::NoResponse(_))
    ));

    let mut mid_header = tokio_test::io::Builder::new()
        .read(&TAG_RESPONSE_MESSAGE)
        .read_error(reset())
        .build();
    assert!(matches!(
        read_frame(&mut mid_header, TAG_RESPONSE_MESSAGE, BOUND).await,
        Err(FrameError::Io(_))
    ));

    let mut failing_writer = tokio_test::io::Builder::new().write_error(reset()).build();
    assert!(matches!(
        write_frame(&mut failing_writer, &response(0)).await,
        Err(FrameError::NoResponse(_))
    ));
}
