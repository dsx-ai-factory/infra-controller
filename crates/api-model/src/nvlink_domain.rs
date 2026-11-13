/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Administrator-owned declarations, independent of NMX-C observed domain identities.

use carbide_uuid::rack::RackGroupId;
use chrono::{DateTime, Utc};
use config_version::Versioned;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::controller_outcome::PersistentStateHandlerOutcome;
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
    pub component_type: RackCapabilityType,
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
