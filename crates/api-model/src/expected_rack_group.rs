/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::fmt;

use carbide_uuid::rack::{RackGroupId, RackId};
use serde::{Deserialize, Serialize};

use crate::metadata::{Metadata, default_metadata_for_deserializer};
use crate::rack_type::RackCapabilityType;

/// An externally assigned topology identifier for an expected rack group.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RackGroupTopology(String);

impl RackGroupTopology {
    /// Wrap an external topology name; API validation is performed at the RPC boundary.
    pub fn new(topology: impl Into<String>) -> Self {
        Self(topology.into())
    }

    /// Borrow the external topology name without changing its spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RackGroupTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A device expected to participate in the rack group's NVLink domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedRackGroupMember {
    /// Core rack capability type, serialized under the inventory field `type`.
    #[serde(rename = "type")]
    pub device_type: RackCapabilityType,
    /// External manufacturer name, paired with type and ID to identify a member.
    pub manufacturer: String,
    /// External device identifier, distinct from a rack ID.
    pub id: String,
}

/// A logical group of expected racks and devices in one NVLink domain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ExpectedRackGroup {
    /// Identifier assigned by the external inventory system.
    pub rack_group_id: RackGroupId,
    /// NVLink topology declared for the group.
    pub topology: RackGroupTopology,
    /// Expected racks spanned by the group.
    pub rack_ids: Vec<RackId>,
    /// Devices expected to participate in the NVLink domain.
    pub members: Vec<ExpectedRackGroupMember>,
    /// Descriptive attributes such as name, manufacturer, and location labels.
    /// Omission during deserialization supplies `Metadata::default()`.
    #[serde(default = "default_metadata_for_deserializer")]
    pub metadata: Metadata,
}
