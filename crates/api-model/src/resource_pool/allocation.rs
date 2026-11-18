// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Allocation records for Integrated pools and remote providers.
//!
//! A remote record starts before the provider call and keeps its original request
//! and result through cleanup, so retries can recognize work already completed.
//! Integrated records describe assigned inventory and are removed on release.

use std::net::{IpAddr, Ipv6Addr};

use chrono::{DateTime, Utc};
use ipnetwork::Ipv6Network;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use uuid::Uuid;

use super::remote::{AllocationResult, RemoteAllocationParameters};
use super::{OwnerType, ValueType};

/// `ResourcePoolAllocation` records an allocation's owner, source, and progress.
/// Remote records include pending requests and completed cleanup; their saved
/// results let callers recognize a retry without allocating another value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePoolAllocation {
    /// NICo allocation UUID, also used as the request UUID for a remote provider.
    pub allocation_id: Uuid,
    /// NICo's logical pool name, e.g. `vpc-vni`.
    pub pool_name: String,
    /// Kind of entity holding the value.
    pub owner_type: OwnerType,
    /// Entity ID in the owner's existing string format.
    pub owner_id: String,
    /// Attachment or cleanup progress; Integrated records are always attached.
    pub state: AllocationState,
    /// Time NICo saved the record, including migration backfill time.
    pub created_at: DateTime<Utc>,
    /// Time of the last applied write, including an identical provider-result retry.
    pub updated_at: DateTime<Utc>,
    /// Assigned inventory or the remote request and its saved result.
    pub source: AllocationSource,
}

/// `AllocationSource` keeps the details specific to the allocator that issued
/// the value. Remote requests may not have received a value yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationSource {
    /// A grant from NICo's local inventory.
    Integrated {
        /// Allocated `resource_pool` row; its deletion also removes this allocation record.
        resource_id: i64,
        /// Integer, address, or prefix assigned from the inventory row.
        value: ResourcePoolValue,
        /// Automatic or manual partition; absent when the inventory did not record it.
        assignment_mode: Option<AssignmentMode>,
        /// Original allocation time, when recorded by the inventory.
        allocated_at: Option<DateTime<Utc>>,
    },
    /// A request sent to a remote provider, including its result when known.
    Remote {
        /// Saved relationship between NICo's logical pool and its backend.
        binding_id: Uuid,
        /// Provider inputs and attachment checks saved before the provider call.
        request: Box<RemoteAllocationParameters>,
        /// Definite assignment or rejection; absent until either has been received.
        result: Option<AllocationResult>,
    },
}

/// `AllocationState` records NICo's attachment and cleanup progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AllocationState {
    /// A remote request is saved but has not attached an allocation. A definite
    /// failure stays here until cancellation is requested and confirmed.
    Pending,
    /// An Integrated grant or remote assignment attached to its entity.
    Attached,
    /// NICo must cancel the remote request, including any allocation that arrives late.
    CancelRequested,
    /// NICo must release the remote allocation after its entity stops using it.
    ReleaseRequested,
    /// The remote provider confirmed cancellation and prevents allocation replay.
    Cancelled,
    /// The remote provider confirmed release and prevents allocation replay.
    Released,
}

/// `ResourcePoolValue` stores a pool value without a transport-specific format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResourcePoolValue {
    /// An integer identifier using the full signed 64-bit range.
    Integer(i64),
    /// One IPv4 or IPv6 address.
    IpAddress(IpAddr),
    /// An IPv6 network allocated as a prefix rather than an individual address.
    Ipv6Prefix(Ipv6Network),
}

impl ResourcePoolValue {
    fn parse(
        value_type: ValueType,
        value: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(match value_type {
            ValueType::Integer => Self::Integer(value.parse()?),
            // Existing `IpAddr` handles can store either address family under `ipv4`.
            ValueType::Ipv4 => Self::IpAddress(value.parse()?),
            ValueType::Ipv6 => Self::IpAddress(IpAddr::V6(value.parse::<Ipv6Addr>()?)),
            ValueType::Ipv6Prefix => Self::Ipv6Prefix(value.parse()?),
        })
    }
}

/// `AssignmentMode` identifies the partition containing an assigned value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentMode {
    /// Available when the caller asks the allocator to choose a value.
    Automatic,
    /// Available when the caller requests a particular value.
    Manual,
}

impl<'r> sqlx::FromRow<'r, PgRow> for ResourcePoolAllocation {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let allocation_id: Uuid = row.try_get("allocation_id")?;
        let state: AllocationState = row.try_get("state")?;
        let owner_type: &str = row.try_get("owner_type")?;
        let owner_type =
            owner_type
                .parse::<OwnerType>()
                .map_err(|error| sqlx::Error::ColumnDecode {
                    index: "owner_type".to_string(),
                    source: Box::new(error),
                })?;
        let resource_id: Option<i64> = row.try_get("integrated_resource_id")?;
        let binding_id: Option<Uuid> = row.try_get("binding_id")?;
        let source = match (resource_id, binding_id) {
            (Some(resource_id), None) => {
                if state != AllocationState::Attached {
                    return Err(sqlx::Error::Decode(
                        format!("integrated allocation {allocation_id} must be attached").into(),
                    ));
                }
                let value_type: ValueType = row.try_get("value_type")?;
                let value: &str = row.try_get("value")?;
                let value = ResourcePoolValue::parse(value_type, value).map_err(|source| {
                    sqlx::Error::ColumnDecode {
                        index: "value".to_string(),
                        source,
                    }
                })?;
                let auto_assign: Option<bool> = row.try_get("auto_assign")?;
                let assignment_mode = auto_assign.map(|automatic| {
                    if automatic {
                        AssignmentMode::Automatic
                    } else {
                        AssignmentMode::Manual
                    }
                });
                AllocationSource::Integrated {
                    resource_id,
                    value,
                    assignment_mode,
                    allocated_at: row.try_get("allocated_at")?,
                }
            }
            (None, Some(binding_id)) => AllocationSource::Remote {
                binding_id,
                request: row
                    .try_get::<Json<Box<RemoteAllocationParameters>>, _>("remote_request")?
                    .0,
                result: row
                    .try_get::<Option<Json<AllocationResult>>, _>("result")?
                    .map(|result| result.0),
            },
            _ => {
                return Err(sqlx::Error::Decode(
                    format!("allocation {allocation_id} must have exactly one source").into(),
                ));
            }
        };

        Ok(Self {
            allocation_id,
            pool_name: row.try_get("pool_name")?,
            owner_type,
            owner_id: row.try_get("owner_id")?,
            state,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            source,
        })
    }
}
