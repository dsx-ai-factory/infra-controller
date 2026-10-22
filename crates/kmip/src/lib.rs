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
//! The client speaks the OASIS KMIP 1.4 TTLV wire protocol, and only that
//! version, over mutually authenticated TLS, and implements only the
//! operations needed to use an AES-256 key held by the server as a key
//! encryption key: `DiscoverVersions`, `Query`, `GetAttributes`, single-part
//! `Encrypt` and single-part `Decrypt` in AES-GCM with a server-generated IV.
//! It never creates, retrieves, rotates, revokes or destroys keys; those
//! remain operator-controlled lifecycle operations on the KMIP server, as does
//! keeping the key non-exportable.
//!
//! The client reports facts about the server (supported versions and
//! operations, key attributes) and leaves policy decisions, such as refusing a
//! key that is not `Active` or a server whose `discover_versions()` lacks 1.4,
//! to its caller. The public enumerations derive serde for the TTLV codec
//! only; their serialized form is TTLV, not a configuration format.
//!
//! Wire encoding uses the [`kmip_ttlv`] serde (de)serializer; the message model
//! lives in the private `wire` module. The `test-support` feature adds an
//! in-process mock KMIP server and test PKI files for tests in dependents.

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
    MAX_CONNECT_TIMEOUT, MAX_CONNECTION_IDLE, MAX_REQUEST_TIMEOUT,
};
pub use error::{KmipError, TimeoutPhase, TlsFile, TlsMaterialError};
pub use framing::MAX_RESPONSE_BYTES;
pub use wire::{CryptographicAlgorithm, Operation, ResultReason, ResultStatus, State};
