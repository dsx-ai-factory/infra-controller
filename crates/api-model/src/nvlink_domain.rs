/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Administrator-owned declarations, independent of NMX-C observed domain identities.

use carbide_uuid::rack::RackGroupId;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

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
