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

//! Record which backend each resource pool uses, e.g. `vpc-vni` being backed
//! by an Integrated or gRPC resource pool backend.

use std::fmt;

use uuid::Uuid;

use super::ValueType;

/// `INTEGRATED_AUTHORITY_ID` identifies the built-in allocator using the
/// `nico-api` database. Replicas and named Integrated backends that use the same
/// database share this value. `local-db` is not a globally unique site ID.
pub const INTEGRATED_AUTHORITY_ID: &str = "local-db";

/// `ResourcePoolBackendKind` identifies the implementation serving a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ResourcePoolBackendKind {
    /// Allocate from the resource pool integrated with the `nico-api` database backend.
    Integrated,
    /// Allocate through a remote gRPC provider once remote activation is supported.
    Grpc,
}

/// `ResourcePoolValueDomain` identifies the family of values a pool allocates.
/// Unlike a resource row's `ValueType`, it groups IPv4 and IPv6 as `IpAddress`
/// because one address pool can contain both. Adding the other address family
/// therefore preserves the pool's binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ResourcePoolValueDomain {
    /// Integer identifiers, such as VNIs and InfiniBand partition keys.
    Integer,
    /// Individual IPv4 or IPv6 addresses, including pools containing both.
    IpAddress,
    /// IPv6 network prefixes rather than individual addresses.
    Ipv6Prefix,
}

impl From<ValueType> for ResourcePoolValueDomain {
    fn from(value_type: ValueType) -> Self {
        match value_type {
            ValueType::Integer => Self::Integer,
            ValueType::Ipv4 | ValueType::Ipv6 => Self::IpAddress,
            ValueType::Ipv6Prefix => Self::Ipv6Prefix,
        }
    }
}

impl fmt::Display for ResourcePoolValueDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Integer => "integer",
            Self::IpAddress => "ip_address",
            Self::Ipv6Prefix => "ipv6_prefix",
        })
    }
}

/// `ResourcePoolSelection` describes which backend a pool requests after
/// resolving configuration and defaults. For example, `vpc-vni` can select a
/// named Integrated backend called `local`, or use Integrated by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePoolSelection {
    /// Pool name used by NICo, e.g. `vpc-vni`.
    pub pool_name: String,
    /// Family of values the pool allocates, e.g. `Integer` for `vpc-vni`.
    pub value_domain: ResourcePoolValueDomain,
    /// Backend's configuration name, e.g. `local` for `backend = "local"`.
    /// `None` selects the default Integrated backend.
    pub backend_name: Option<String>,
    /// Implementation that allocates values, e.g. `Integrated` for the local database.
    pub backend_kind: ResourcePoolBackendKind,
    /// Allocator's stable identity, e.g. `local-db` for every Integrated backend
    /// using this database. Renaming `local` does not change this identity.
    pub authority_id: String,
    /// Pool name passed to the backend, e.g. `vpc-vni`. Omitting `remote_pool`
    /// in configuration resolves to `pool_name`; Integrated requires that match.
    pub remote_pool: String,
}

/// `ResourcePoolBinding` records a pool's accepted backend selection and UUID.
/// We retain it even when the pool is empty or removed from configuration, so
/// later configuration cannot silently move that pool to another allocator.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ResourcePoolBinding {
    /// Stable UUID assigned when this pool's first backend selection is accepted.
    pub binding_id: Uuid,
    /// Pool name used by NICo, e.g. `vpc-vni`.
    pub pool_name: String,
    /// Value family fixed when the binding is created; address pools accept
    /// both IPv4 and IPv6.
    pub value_domain: ResourcePoolValueDomain,
    /// Backend's configuration name when first bound, e.g. `local`, or `None`
    /// for the default Integrated backend. Later alias changes leave this intact.
    pub backend_name: Option<String>,
    /// Implementation selected at first binding.
    pub backend_kind: ResourcePoolBackendKind,
    /// Allocator's stable identity, e.g. `local-db` for this `nico-api` database.
    pub authority_id: String,
    /// Pool name passed to the backend, fixed when the binding is created.
    pub remote_pool: String,
}

impl ResourcePoolBinding {
    /// `matches` checks whether a requested selection keeps the same pool,
    /// value domain, backend implementation, allocator identity, and remote pool.
    /// The configuration name is excluded because aliases can use the same allocator.
    pub fn matches(&self, requested: &ResourcePoolSelection) -> bool {
        self.pool_name == requested.pool_name
            && self.value_domain == requested.value_domain
            && self.backend_kind == requested.backend_kind
            && self.authority_id == requested.authority_id
            && self.remote_pool == requested.remote_pool
    }
}
