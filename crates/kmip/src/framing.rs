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
//! TTLV message framing: one KMIP message is one top-level TTLV Structure whose
//! 8-byte header (3-byte tag, 1-byte type, 4-byte big-endian length) tells how
//! many value bytes follow.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

/// The largest response frame the client accepts, also sent to the server as
/// the request header's Maximum Response Size so a compliant server answers
/// `ResponseTooLarge` instead of streaming an oversized frame.
pub const MAX_RESPONSE_BYTES: u32 = 1024 * 1024;

pub(crate) const HEADER_LEN: usize = 8;
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TAG_REQUEST_MESSAGE: [u8; 3] = [0x42, 0x00, 0x78];
pub(crate) const TAG_RESPONSE_MESSAGE: [u8; 3] = [0x42, 0x00, 0x7B];
const TYPE_STRUCTURE: u8 = 0x01;

#[derive(Debug)]
pub(crate) enum FrameError {
    /// The peer failed or closed the connection before a single byte of the
    /// response arrived, so the request may never have been seen. This is the
    /// only outcome the client retries, and only on a reused connection.
    NoResponse(io::Error),
    /// The connection failed after the response had started.
    Io(io::Error),
    /// The bytes are not a TTLV message of the expected kind.
    Protocol(String),
    /// The header announced more value bytes than `max_len`; none were read.
    TooLarge { length: u32, max: u32 },
}

/// Writes one complete message.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &[u8],
) -> Result<(), FrameError> {
    writer
        .write_all(frame)
        .await
        .map_err(FrameError::NoResponse)?;
    writer.flush().await.map_err(FrameError::NoResponse)
}

/// Reads one complete message whose top-level tag is `expected_tag`, bounding
/// the value length by `max_len` before allocating. The returned buffer is
/// zeroized on drop because a frame can carry key material.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    expected_tag: [u8; 3],
    max_len: u32,
) -> Result<Zeroizing<Vec<u8>>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]).await {
            Ok(0) => {
                let error = io::Error::from(io::ErrorKind::UnexpectedEof);
                return Err(if filled == 0 {
                    FrameError::NoResponse(error)
                } else {
                    FrameError::Io(error)
                });
            }
            Ok(read) => filled += read,
            Err(error) if filled == 0 => return Err(FrameError::NoResponse(error)),
            Err(error) => return Err(FrameError::Io(error)),
        }
    }
    if header[..3] != expected_tag[..] {
        return Err(FrameError::Protocol(format!(
            "unexpected top-level tag 0x{:02X}{:02X}{:02X}",
            header[0], header[1], header[2]
        )));
    }
    if header[3] != TYPE_STRUCTURE {
        return Err(FrameError::Protocol(format!(
            "top-level item has type 0x{:02X}, expected a structure",
            header[3]
        )));
    }
    let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if length > max_len {
        return Err(FrameError::TooLarge {
            length,
            max: max_len,
        });
    }
    let mut frame = Zeroizing::new(vec![0u8; HEADER_LEN + length as usize]);
    frame[..HEADER_LEN].copy_from_slice(&header);
    reader
        .read_exact(&mut frame[HEADER_LEN..])
        .await
        .map_err(FrameError::Io)?;
    check_item_lengths(&frame).map_err(FrameError::Protocol)?;
    Ok(frame)
}

const TYPE_INTERVAL: u8 = 0x0A;

/// Maximum nesting of TTLV structures accepted in one frame; KMIP messages
/// nest a handful of levels.
const MAX_DEPTH: usize = 32;

/// Checks that every item in `frame` fits inside its enclosing structure, so
/// the TTLV decoder never allocates from an advertised length that the frame
/// cannot back. Items are 8-byte aligned: a primitive's value is padded to the
/// next multiple of 8, and a structure's length already covers its children's
/// padding.
pub(crate) fn check_item_lengths(frame: &[u8]) -> Result<(), String> {
    // Each entry is a byte range still to be walked, at its nesting depth.
    let mut pending = vec![(0usize, frame.len(), 0usize)];
    while let Some((mut offset, end, depth)) = pending.pop() {
        while offset < end {
            if end - offset < HEADER_LEN {
                return Err(format!("truncated item header at byte {offset}"));
            }
            let item_type = frame[offset + 3];
            let length = u32::from_be_bytes([
                frame[offset + 4],
                frame[offset + 5],
                frame[offset + 6],
                frame[offset + 7],
            ]) as usize;
            if !(TYPE_STRUCTURE..=TYPE_INTERVAL).contains(&item_type) {
                return Err(format!(
                    "item at byte {offset} has invalid type 0x{item_type:02X}"
                ));
            }
            let value_start = offset + HEADER_LEN;
            let padded = if item_type == TYPE_STRUCTURE {
                length
            } else {
                length.div_ceil(8) * 8
            };
            let Some(item_end) = value_start.checked_add(padded) else {
                return Err(format!("item at byte {offset} overflows the frame"));
            };
            if item_end > end {
                return Err(format!(
                    "item at byte {offset} claims {length} value bytes but only {} remain",
                    end - value_start
                ));
            }
            if item_type == TYPE_STRUCTURE {
                if depth + 1 > MAX_DEPTH {
                    return Err(format!("structures nest deeper than {MAX_DEPTH} levels"));
                }
                pending.push((value_start, item_end, depth + 1));
            }
            offset = item_end;
        }
    }
    Ok(())
}
