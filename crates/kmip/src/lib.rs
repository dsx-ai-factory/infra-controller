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

//! A deliberately scoped KMIP 1.4 client.
//!
//! The client speaks the OASIS KMIP 1.4 TTLV wire protocol over mutually
//! authenticated TLS and implements only the operations needed to use a
//! server-held, non-exportable AES-256 key as a key encryption key:
//! `DiscoverVersions`, `Query`, `GetAttributes`, single-part `Encrypt` and
//! single-part `Decrypt` in AES-GCM with a server-generated IV. It never
//! creates, retrieves, rotates, revokes or destroys keys; those remain
//! operator-controlled lifecycle operations on the KMIP server.
//!
//! The client reports facts about the server (supported versions and
//! operations, key attributes) and leaves policy decisions, such as refusing a
//! key that is not `Active`, to its caller.
//!
//! Wire encoding uses the [`kmip_ttlv`] serde (de)serializer; the message model
//! lives in the private `wire` module. The `test-support` feature adds an
//! in-process mock KMIP server for tests.

mod client;
mod config;
mod error;
mod framing;
mod tls;
mod wire;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod tests;

pub use client::{
    AeadCiphertext, KeyAttributes, KmipClient, ProtocolVersion, QueryInfo, USAGE_MASK_DECRYPT,
    USAGE_MASK_ENCRYPT,
};
pub use config::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_PORT, DEFAULT_REQUEST_TIMEOUT, KmipClientConfig,
    MAX_CONNECT_TIMEOUT, MAX_REQUEST_TIMEOUT,
};
pub use error::{KmipError, TlsMaterialError};
pub use framing::MAX_RESPONSE_BYTES;
pub use wire::{CryptographicAlgorithm, Operation, ResultReason, ResultStatus, State};
