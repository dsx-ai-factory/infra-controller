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

// src/client/mod.rs
// Client module exports and re-exports to maintain existing API compatibility.
//
// Provides a clean interface by re-exporting the main client and supporting types
// while hiding the internal module structure from external users.

mod core;
mod handlers;
mod messages;
mod options;
mod registry;
mod topic_patterns;

pub use core::MqtteaClient;

pub use handlers::{ClosureAdapter, ErasedHandler};
pub use messages::ReceivedMessage;
pub use options::{ClientCredentials, ClientOptions, ClientTlsConfig, ClientTlsIdentity};
pub use topic_patterns::TopicPatterns;
