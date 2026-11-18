// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Saved requests used to register pool eligibility with a remote provider.

use uuid::Uuid;

/// `ResourcePoolRegistration` keeps the exact request for one binding revision.
/// Accepted revisions remain available for allocations that use them and for retries.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ResourcePoolRegistration {
    /// Binding whose remote provider receives this registration.
    pub binding_id: Uuid,
    /// Positive revision number, increasing within this binding.
    pub revision: i64,
    /// Stored protobuf registration request, decoded by the resource-pool library.
    pub request: Vec<u8>,
    /// Whether the provider's acceptance has been recorded locally.
    pub accepted: bool,
}
