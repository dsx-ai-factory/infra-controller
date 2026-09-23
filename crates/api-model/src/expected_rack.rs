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

use std::collections::HashMap;

use carbide_uuid::rack::{RackId, RackProfileId};
use serde::Deserialize;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::expected_rack_group::ExpectedRackGroup;
use crate::metadata::{Metadata, default_metadata_for_deserializer};
use crate::rack_type::RackCapabilityType;

/// Derives a profile name from the topology and manufacturers of one declared rack.
pub fn derive_rack_profile_id(
    group: &ExpectedRackGroup,
    rack_id: &RackId,
) -> Result<RackProfileId, String> {
    let rack = group
        .racks
        .iter()
        .find(|rack| &rack.rack_id == rack_id)
        .ok_or_else(|| {
            format!(
                "rack {rack_id} is not declared in group {}",
                group.rack_group_id
            )
        })?;
    let manufacturer = |kind: RackCapabilityType| -> Result<Option<&str>, String> {
        let mut selected = None;
        for member in rack
            .members
            .iter()
            .filter(|member| member.device_type == kind)
        {
            let value = member.manufacturer.as_str();
            if value.trim().is_empty() {
                return Err(format!("rack {rack_id} has a blank {kind} manufacturer"));
            }
            if selected.is_some_and(|previous| previous != value) {
                return Err(format!("rack {rack_id} has multiple {kind} manufacturers"));
            }
            selected = Some(value);
        }
        Ok(selected)
    };
    let compute = manufacturer(RackCapabilityType::Compute)?
        .ok_or_else(|| format!("rack {rack_id} has no Compute members"))?;
    let switch = manufacturer(RackCapabilityType::Switch)?
        .ok_or_else(|| format!("rack {rack_id} has no Switch members"))?;
    let power_shelf = manufacturer(RackCapabilityType::PowerShelf)?.unwrap_or("NO_POWERSHELF");
    Ok(RackProfileId::new(format!(
        "{}_{}_{}_{}",
        group.topology.as_str().to_uppercase(),
        compute,
        switch,
        power_shelf
    )))
}

/// ExpectedRack represents a rack that has been declared and is expected to
/// be fully populated with compute trays, switches, and power shelves. The
/// rack_profile_id references a RackProfile in the Carbide config file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExpectedRack {
    /// rack_id is the rack identifier, which comes from the DCIM.
    pub rack_id: RackId,

    /// rack_profile_id is the identifier of the rack profile (e.g. "NVL72").
    /// This maps to a RackProfile in the Carbide config file, which defines
    /// the rack hardware type, topology, and rack capabilities.
    pub rack_profile_id: RackProfileId,

    /// User-defined metadata for the rack. Physical-chassis and
    /// physical-location attributes are recorded as well-known label keys
    /// on this Metadata (see api-model::rack for the well-known keys).
    #[serde(default = "default_metadata_for_deserializer")]
    pub metadata: Metadata,
}

impl<'r> FromRow<'r, PgRow> for ExpectedRack {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("metadata_labels")?;
        let metadata = Metadata {
            name: row.try_get("metadata_name")?,
            description: row.try_get("metadata_description")?,
            labels: labels.0,
        };

        Ok(ExpectedRack {
            rack_id: row.try_get("rack_id")?,
            rack_profile_id: row.try_get("rack_profile_id")?,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expected_rack_group::{
        ExpectedRackGroupMember, ExpectedRackGroupRack, RackGroupTopology,
    };

    #[test]
    fn derive_profile() {
        use RackCapabilityType::{Compute, PowerShelf, Switch};
        let cases = [
            (
                "mixed manufacturers",
                vec![
                    (Compute, "WiWynn"),
                    (Switch, "NVIDIA"),
                    (PowerShelf, "WiWynn"),
                ],
                Some("GB200_NVL72R1_C2G4_WiWynn_NVIDIA_WiWynn"),
            ),
            (
                "no power shelf",
                vec![(Compute, "NVIDIA"), (Compute, "NVIDIA"), (Switch, "NVIDIA")],
                Some("GB200_NVL72R1_C2G4_NVIDIA_NVIDIA_NO_POWERSHELF"),
            ),
            ("missing compute", vec![(Switch, "NVIDIA")], None),
            ("missing switch", vec![(Compute, "NVIDIA")], None),
            (
                "mixed type manufacturers",
                vec![(Compute, "NVIDIA"), (Compute, "WiWynn"), (Switch, "NVIDIA")],
                None,
            ),
            (
                "blank manufacturer",
                vec![(Compute, " "), (Switch, "NVIDIA")],
                None,
            ),
        ];
        for (name, members, expected) in cases {
            let rack_id: RackId = "rack-01".parse().unwrap();
            let mut group = ExpectedRackGroup {
                topology: RackGroupTopology::new("gb200_nvl72r1_c2g4"),
                racks: vec![ExpectedRackGroupRack {
                    rack_id: rack_id.clone(),
                    members: members
                        .into_iter()
                        .enumerate()
                        .map(
                            |(index, (device_type, manufacturer))| ExpectedRackGroupMember {
                                device_type,
                                manufacturer: manufacturer.into(),
                                id: index.to_string(),
                            },
                        )
                        .collect(),
                }],
                ..Default::default()
            };
            group.racks.push(ExpectedRackGroupRack {
                rack_id: RackId::new("other-rack"),
                members: vec![ExpectedRackGroupMember {
                    device_type: Compute,
                    manufacturer: "other-vendor".into(),
                    id: "other-device".into(),
                }],
            });
            let result = derive_rack_profile_id(&group, &rack_id);
            assert_eq!(
                result.as_ref().ok().map(|id| id.as_str()),
                expected,
                "{name}: {result:?}"
            );
        }
    }
}
