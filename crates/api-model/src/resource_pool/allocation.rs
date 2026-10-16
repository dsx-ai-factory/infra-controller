// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Active allocations from Integrated pools and remote providers.
//!
//! Each allocation record identifies one grant, even when its value or owner is
//! reused. Its source points to the inventory row or retained remote operation;
//! state comes from that source when we read it. Integrated release or confirmed
//! remote cleanup removes the allocation record.

use std::fmt;
use std::net::{IpAddr, Ipv6Addr};

use chrono::{DateTime, Utc};
use ipnetwork::Ipv6Network;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use uuid::Uuid;

use super::remote::RemoteOperationState;
use super::{OwnerType, ResourcePoolEntryState, ValueType};

/// `ResourcePoolAllocation` describes one grant NICo still tracks as active.
/// More than one allocation may have the same pool and owner. Use `allocation_id`
/// to identify a grant and `source` to find its inventory or remote operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePoolAllocation {
    /// NICo allocation UUID, replaced when a value is granted again.
    pub allocation_id: Uuid,
    /// NICo's logical pool name, e.g. `vpc-vni`.
    pub pool_name: String,
    /// Allocated integer, IP address, or IPv6 prefix, parsed from the stored value.
    pub value: ResourcePoolValue,
    /// Kind of entity holding the value.
    pub owner_type: OwnerType,
    /// Entity ID in the owner's existing string format.
    pub owner_id: String,
    /// Automatic or manual partition; `None` preserves an unknown legacy mode.
    pub assignment_mode: Option<AssignmentMode>,
    /// Original Integrated allocation time; `None` when unknown or remote.
    pub allocated_at: Option<DateTime<Utc>>,
    /// Time NICo created this allocation record, including migration backfill time.
    pub recorded_at: DateTime<Utc>,
    /// Inventory row or retained provider operation that issued the grant.
    pub source: AllocationSource,
    /// Attachment or cleanup progress derived from the source at read time.
    pub state: AllocationState,
}

/// `AllocationSource` links an allocation record to the record that owns its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationSource {
    /// A grant from NICo's local inventory.
    Integrated {
        /// Allocated `resource_pool` row; its deletion also removes this allocation record.
        resource_id: i64,
    },
    /// A grant issued by a remote provider.
    Remote {
        /// Original operation with the binding, provider handle, and routing policy.
        request_id: Uuid,
    },
}

/// `AllocationState` reports the source's progress without storing a second state.
/// Terminal remote operations have no active allocation record; seeing both is
/// an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationState {
    /// The remote provider assigned a value that NICo has not attached yet.
    Reserved,
    /// An Integrated grant or a remote assignment attached to its entity.
    Attached,
    /// NICo requested cancellation or release and still owes provider cleanup.
    CleanupRequested,
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
    /// `value_type` identifies the database value type, including the IP family.
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Integer(_) => ValueType::Integer,
            Self::IpAddress(IpAddr::V4(_)) => ValueType::Ipv4,
            Self::IpAddress(IpAddr::V6(_)) => ValueType::Ipv6,
            Self::Ipv6Prefix(_) => ValueType::Ipv6Prefix,
        }
    }

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

impl fmt::Display for ResourcePoolValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::IpAddress(value) => write!(formatter, "{value}"),
            Self::Ipv6Prefix(value) => write!(formatter, "{value}"),
        }
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
        let value_type: ValueType = row.try_get("value_type")?;
        let value: &str = row.try_get("value")?;
        let value = ResourcePoolValue::parse(value_type, value).map_err(|source| {
            sqlx::Error::ColumnDecode {
                index: "value".to_string(),
                source,
            }
        })?;
        let owner_type: &str = row.try_get("owner_type")?;
        let owner_type =
            owner_type
                .parse::<OwnerType>()
                .map_err(|error| sqlx::Error::ColumnDecode {
                    index: "owner_type".to_string(),
                    source: Box::new(error),
                })?;
        let auto_assign: Option<bool> = row.try_get("auto_assign")?;
        let assignment_mode = auto_assign.map(|automatic| {
            if automatic {
                AssignmentMode::Automatic
            } else {
                AssignmentMode::Manual
            }
        });

        let resource_id: Option<i64> = row.try_get("integrated_resource_id")?;
        let request_id: Option<Uuid> = row.try_get("remote_request_id")?;
        let (source, state) = match (resource_id, request_id) {
            (Some(resource_id), None) => {
                let state: Option<Json<ResourcePoolEntryState>> =
                    row.try_get("integrated_state")?;
                if !matches!(state, Some(Json(ResourcePoolEntryState::Allocated { .. }))) {
                    return Err(sqlx::Error::Decode(format!(
                        "allocation {allocation_id} references unallocated inventory {resource_id}"
                    ).into()));
                }
                (
                    AllocationSource::Integrated { resource_id },
                    AllocationState::Attached,
                )
            }
            (None, Some(request_id)) => {
                let state: Option<RemoteOperationState> = row.try_get("remote_state")?;
                let state = match state {
                    Some(RemoteOperationState::Pending) => AllocationState::Reserved,
                    Some(RemoteOperationState::Attached) => AllocationState::Attached,
                    Some(
                        RemoteOperationState::CancelRequested
                        | RemoteOperationState::ReleaseRequested,
                    ) => AllocationState::CleanupRequested,
                    Some(RemoteOperationState::Cancelled | RemoteOperationState::Released)
                    | None => {
                        return Err(sqlx::Error::Decode(format!(
                            "allocation {allocation_id} references inactive remote operation {request_id}"
                        ).into()));
                    }
                };
                (AllocationSource::Remote { request_id }, state)
            }
            _ => {
                return Err(sqlx::Error::Decode(
                    format!("allocation {allocation_id} must have exactly one source").into(),
                ));
            }
        };

        Ok(Self {
            allocation_id,
            pool_name: row.try_get("pool_name")?,
            value,
            owner_type,
            owner_id: row.try_get("owner_id")?,
            assignment_mode,
            allocated_at: row.try_get("allocated_at")?,
            recorded_at: row.try_get("recorded_at")?,
            source,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn address_and_prefix_assignments_keep_their_database_type_and_text() {
        value_scenarios!(
            run = |value: ResourcePoolValue| (value.value_type(), value.to_string());
            "address families stay distinct in storage" {
                ResourcePoolValue::IpAddress("192.0.2.7".parse().unwrap()) =>
                    (ValueType::Ipv4, "192.0.2.7".to_owned()),
                ResourcePoolValue::IpAddress("2001:db8::7".parse().unwrap()) =>
                    (ValueType::Ipv6, "2001:db8::7".to_owned()),
            }
            "prefix keeps its length" {
                ResourcePoolValue::Ipv6Prefix("2001:db8::/64".parse().unwrap()) =>
                    (ValueType::Ipv6Prefix, "2001:db8::/64".to_owned()),
            }
        );
    }
}
