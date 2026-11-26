/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Administrator-owned declarations, independent of NMX-C observed domain identities.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use carbide_uuid::rack::RackGroupId;
use chrono::{DateTime, Utc};
use config_version::Versioned;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::expected_rack_group::ExpectedRackGroup;
// These Rack subtypes also define persisted Domain JSON; preserve its decoding compatibility.
use crate::rack::{MaintenanceScope, RackMaintenanceState, RackValidationState};
use crate::rack_type::RackCapabilityType;

/// An immutable declaration of compute and NVSwitch tray membership.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct NvLinkDomain {
    /// Core-generated declaration identity, not an observed NMX-C domain UUID.
    pub id: Uuid,
    /// Physical inventory group containing the declared trays' racks.
    pub rack_group_id: RackGroupId,
    /// Optional operator-facing name; names need not be unique.
    pub name: Option<String>,
    /// Positions ordered by rack index, slot index, and component type.
    #[sqlx(json)]
    pub members: Vec<NvLinkDomainMember>,
}

/// A tray position within the domain's rack group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvLinkDomainMember {
    /// Zero-based position in the rack group's ordered racks array.
    pub rack_index: u32,
    /// Caller-supplied tray slot index.
    pub slot_index: u32,
    /// Compute or Switch; power shelves cannot join an NVLink domain.
    pub component_type: NvLinkDomainMemberType,
}

/// Map each rack's lowest compute cartridge to slot 1, preserving physical offsets.
pub fn derive_managed_members(
    group: &ExpectedRackGroup,
) -> Result<Vec<NvLinkDomainMember>, String> {
    let mut positions = Vec::new();
    for (index, rack) in group.racks.iter().enumerate() {
        let Some(first) = rack
            .members
            .iter()
            .filter(|member| member.device_type == RackCapabilityType::Compute)
            .min_by_key(|member| member.cartridge_index)
        else {
            if rack
                .members
                .iter()
                .any(|member| member.device_type == RackCapabilityType::Switch)
            {
                return Err(format!(
                    "rack {} has a switch but no compute cartridge to anchor slot 1",
                    rack.rack_id
                ));
            }
            continue;
        };
        let cartridge_index = |member: &crate::expected_rack_group::ExpectedRackGroupMember| {
            member.cartridge_index.ok_or_else(|| {
                format!(
                    "rack {} member {} has no cartridge index",
                    rack.rack_id, member.id
                )
            })
        };
        let base = cartridge_index(first)?;
        let rack_index =
            u32::try_from(index).map_err(|_| format!("rack index {index} exceeds u32"))?;
        let mut occupied = HashSet::new();
        for member in &rack.members {
            let component_type = match member.device_type {
                RackCapabilityType::Compute => NvLinkDomainMemberType::Compute,
                RackCapabilityType::Switch => NvLinkDomainMemberType::Switch,
                RackCapabilityType::PowerShelf => continue,
            };
            let cartridge = cartridge_index(member)?;
            let slot_index = slot_index_from_cartridge_index(cartridge, base).ok_or_else(|| {
                format!(
                    "rack {} cartridge {cartridge} relative to base {base} exceeds slot range",
                    rack.rack_id
                )
            })?;
            if !occupied.insert((slot_index, component_type.clone())) {
                return Err(format!(
                    "rack {} declares multiple {component_type} members at slot {slot_index}",
                    rack.rack_id
                ));
            }
            positions.push(NvLinkDomainMember {
                rack_index,
                slot_index,
                component_type,
            });
        }
    }
    Ok(positions)
}

fn slot_index_from_cartridge_index(cartridge: u32, base: u32) -> Option<u32> {
    cartridge
        .checked_sub(base)?
        .checked_add(1)
        .filter(|slot| i32::try_from(*slot).is_ok())
}

/// Component types that can participate in an NVLink Domain.
/// Parsing and serialization use the case-sensitive names Compute and Switch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NvLinkDomainMemberType {
    /// A compute tray.
    Compute,
    /// An NVSwitch tray.
    Switch,
}

impl FromStr for NvLinkDomainMemberType {
    type Err = serde::de::value::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::deserialize(serde::de::value::StrDeserializer::<Self::Err>::new(value))
    }
}

impl fmt::Display for NvLinkDomainMemberType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compute => f.write_str("Compute"),
            Self::Switch => f.write_str("Switch"),
        }
    }
}

/// Persisted Domain lifecycle using the Rack controller's state organization.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NvLinkDomainState {
    /// A newly created Domain awaiting member discovery.
    #[default]
    Created,
    /// The controller is waiting for the declared member trays.
    Discovering,
    /// Validation progress after bringup or maintenance.
    Validating {
        /// Shared validation substate representation.
        validating_state: RackValidationState,
    },
    /// The Domain has completed ingestion and validation.
    Ready,
    /// Execution shared by first-time ingestion and requested maintenance.
    Maintenance {
        /// Shared firmware, NVOS, power, and fabric workflow substates.
        maintenance_state: RackMaintenanceState,
    },
    /// A lifecycle failure awaiting recovery.
    Error {
        /// Diagnostic cause of the failure.
        cause: String,
    },
    /// Deletion was requested; member positions remain claimed during cleanup.
    Deleting,
}

/// Persisted requests consumed by the Domain lifecycle controller.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvLinkDomainConfig {
    /// Derive default Domain membership from its rack group during initialization.
    #[serde(default)]
    pub managed: bool,
    /// Requested member-scoped maintenance; omission means no pending request.
    #[serde(default)]
    pub maintenance_requested: Option<MaintenanceScope>,
    /// Requests termination of the current maintenance operation.
    #[serde(default)]
    pub maintenance_termination_requested: bool,
}

/// Declaration and runtime state loaded together for one controller iteration.
#[derive(Clone, Debug)]
pub struct NvLinkDomainSnapshot {
    /// Domain identity and declared member positions.
    pub domain: NvLinkDomain,
    /// Requests and their configuration version.
    pub config: Versioned<NvLinkDomainConfig>,
    /// Lifecycle state and its independent controller version.
    pub controller_state: Versioned<NvLinkDomainState>,
    /// Most recently persisted controller result.
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    /// Deletion intent; positions remain reserved until teardown completes.
    pub deleted: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests;
