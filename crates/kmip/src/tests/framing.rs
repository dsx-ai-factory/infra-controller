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
//! Framing: the response bound is enforced before the body is read, and the
//! ways a connection can end are told apart for the retry decision.

use std::io::Cursor;

use carbide_test_support::value_scenarios;
use zeroize::Zeroizing;

use crate::framing::{FrameError, TAG_RESPONSE_MESSAGE, read_frame};

fn classify(result: Result<Zeroizing<Vec<u8>>, FrameError>) -> String {
    match result {
        Ok(frame) => format!("frame of {} bytes", frame.len()),
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

// Verifies the frame reader's contract with a 64-byte bound: whole frames
// come back intact, an oversized announcement is rejected from the header
// alone, and an ending connection is "no response" only before the first
// byte, which is what makes the client's single retry safe.
#[test]
fn frames_are_bounded_and_connection_endings_are_classified() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let response = |length: u32| header(TAG_RESPONSE_MESSAGE, 0x01, length);
    value_scenarios!(
        run = |bytes: Vec<u8>| classify(runtime.block_on(read_frame(
            &mut Cursor::new(bytes),
            TAG_RESPONSE_MESSAGE,
            64,
        )));
        "complete frames are returned whole, header included" {
            [response(16), header([0x42, 0x00, 0x0D], 0x02, 4), vec![0, 0, 0, 1, 0, 0, 0, 0]]
                .concat() => "frame of 24 bytes".to_string(),
            response(0) => "frame of 8 bytes".to_string(),
        }
        "an announced length above the bound is rejected without reading a body" {
            response(65) => "too large 65 > 64".to_string(),
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
    );
}
