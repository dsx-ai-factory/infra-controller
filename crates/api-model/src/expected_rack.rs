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

use std::borrow::Cow;
use std::collections::HashMap;

use carbide_uuid::rack::{RackGroupId, RackId, RackProfileId};
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
            selected = Some(value);
        }
        Ok(selected)
    };
    manufacturer(RackCapabilityType::Compute)?
        .ok_or_else(|| format!("rack {rack_id} has no Compute members"))?;
    manufacturer(RackCapabilityType::Switch)?
        .ok_or_else(|| format!("rack {rack_id} has no Switch members"))?;
    let power_suffix = if manufacturer(RackCapabilityType::PowerShelf)?.is_some() {
        ""
    } else {
        "_NO_POWERSHELF"
    };
    let vendor = ["WIWYNN", "LENOVO", "SMC"]
        .into_iter()
        .find(|vendor| {
            rack.members.iter().any(|member| {
                member.manufacturer.eq_ignore_ascii_case(vendor)
                    || (*vendor == "SMC" && member.manufacturer.eq_ignore_ascii_case("Supermicro"))
            })
        })
        .unwrap_or("NVIDIA");
    let topology = profile_topology(group, rack_id);
    Ok(RackProfileId::new(format!(
        "{}_{}{power_suffix}",
        topology.to_uppercase(),
        vendor
    )))
}

fn profile_topology<'a>(group: &'a ExpectedRackGroup, rack_id: &RackId) -> Cow<'a, str> {
    let topology = group.topology.as_str();
    let Some(protocol) = group.protocol.as_ref() else {
        return Cow::Borrowed(topology);
    };
    if protocol.as_str() != "NVLINK_V6" {
        return Cow::Borrowed(topology);
    }
    let Some((platform, suffix)) = topology.split_once('_') else {
        return Cow::Borrowed(topology);
    };
    let Some(generation) = platform
        .strip_prefix("gb")
        .or_else(|| platform.strip_prefix("GB"))
    else {
        return Cow::Borrowed(topology);
    };
    if generation.is_empty() || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return Cow::Borrowed(topology);
    }
    let normalized = format!("vr_{suffix}");
    tracing::warn!(
        rack_group_id = %group.rack_group_id,
        %rack_id,
        protocol = %protocol,
        supplied_topology = topology,
        normalized_topology = normalized,
        "normalizing NVLINK_V6 rack profile topology"
    );
    Cow::Owned(normalized)
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

    /// External group selected together with the profile on creation.
    pub rack_group_id: Option<RackGroupId>,

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
            rack_group_id: row.try_get("rack_group_id")?,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expected_rack_group::{
        ExpectedRackGroupMember, ExpectedRackGroupRack, RackGroupProtocol, RackGroupTopology,
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
                Some("GB200_NVL72R1_C2G4_WIWYNN"),
            ),
            (
                "no power shelf",
                vec![(Compute, "NVIDIA"), (Compute, "NVIDIA"), (Switch, "NVIDIA")],
                Some("GB200_NVL72R1_C2G4_NVIDIA_NO_POWERSHELF"),
            ),
            ("missing compute", vec![(Switch, "NVIDIA")], None),
            ("missing switch", vec![(Compute, "NVIDIA")], None),
            (
                "mixed type manufacturers",
                vec![(Compute, "NVIDIA"), (Compute, "WiWynn"), (Switch, "NVIDIA")],
                Some("GB200_NVL72R1_C2G4_WIWYNN_NO_POWERSHELF"),
            ),
            (
                "wiwynn takes precedence across device types",
                vec![(Compute, "SMC"), (Switch, "LENOVO"), (PowerShelf, "wiwynn")],
                Some("GB200_NVL72R1_C2G4_WIWYNN"),
            ),
            (
                "lenovo takes precedence over supermicro",
                vec![(Compute, "Supermicro"), (Switch, "lenovo")],
                Some("GB200_NVL72R1_C2G4_LENOVO_NO_POWERSHELF"),
            ),
            (
                "supermicro alias",
                vec![(Compute, "NVIDIA"), (Switch, "SuperMicro")],
                Some("GB200_NVL72R1_C2G4_SMC_NO_POWERSHELF"),
            ),
            (
                "smc power shelf",
                vec![(Compute, "NVIDIA"), (Switch, "NVIDIA"), (PowerShelf, "smc")],
                Some("GB200_NVL72R1_C2G4_SMC"),
            ),
            (
                "unrecognized manufacturers default to nvidia",
                vec![(Compute, "other-vendor"), (Switch, "NVIDIA")],
                Some("GB200_NVL72R1_C2G4_NVIDIA_NO_POWERSHELF"),
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
                    manufacturer: "WIWYNN".into(),
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

    #[test]
    fn nvlink_v6_uses_vr_profile_topology() {
        let rack_id = RackId::new("rack-01");
        let group = ExpectedRackGroup {
            rack_group_id: RackGroupId::new("group-01"),
            topology: RackGroupTopology::new("gb300_nvl72r1_c2g4"),
            protocol: Some(RackGroupProtocol::new("NVLINK_V6")),
            racks: vec![ExpectedRackGroupRack {
                rack_id: rack_id.clone(),
                members: [RackCapabilityType::Compute, RackCapabilityType::Switch]
                    .into_iter()
                    .enumerate()
                    .map(|(index, device_type)| ExpectedRackGroupMember {
                        device_type,
                        manufacturer: "NVIDIA".into(),
                        id: index.to_string(),
                    })
                    .collect(),
            }],
            metadata: Default::default(),
        };

        let profile = derive_rack_profile_id(&group, &rack_id).unwrap();
        assert_eq!(profile.as_str(), "VR_NVL72R1_C2G4_NVIDIA_NO_POWERSHELF");
    }
}
