/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::HashSet;

use model::expected_rack_group::{
    ExpectedRackGroup, ExpectedRackGroupMember, ExpectedRackGroupRack, RackGroupTopology,
};
use model::metadata::Metadata;

use crate as rpc;
use crate::errors::RpcDataConversionError;

/// Validate the external identifier on both record and lookup requests.
pub fn validate_rack_group_id(
    id: &carbide_uuid::rack::RackGroupId,
) -> Result<(), RpcDataConversionError> {
    if id.as_str().trim().is_empty() || id.as_str().chars().count() > 128 {
        return Err(RpcDataConversionError::InvalidArgument(
            "rack_group_id must contain 1 to 128 characters and not be blank".to_string(),
        ));
    }
    Ok(())
}

impl From<ExpectedRackGroup> for rpc::forge::ExpectedRackGroup {
    fn from(group: ExpectedRackGroup) -> Self {
        Self {
            rack_group_id: Some(group.rack_group_id),
            topology: group.topology.to_string(),
            racks: group.racks.into_iter().map(Into::into).collect(),
            metadata: Some(group.metadata.into()),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedRackGroup> for ExpectedRackGroup {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::ExpectedRackGroup) -> Result<Self, Self::Error> {
        let invalid = |message: &str| RpcDataConversionError::InvalidArgument(message.to_string());
        let rack_group_id = value
            .rack_group_id
            .ok_or(RpcDataConversionError::MissingArgument("rack_group_id"))?;
        validate_rack_group_id(&rack_group_id)?;
        if value.topology.trim().is_empty() || value.topology.chars().count() > 128 {
            return Err(invalid(
                "topology must contain 1 to 128 characters and not be blank",
            ));
        }
        let mut rack_ids = HashSet::new();
        let mut members = HashSet::new();
        let mut racks = Vec::with_capacity(value.racks.len());
        for rack in value.racks {
            let rack_id = rack
                .rack_id
                .ok_or(RpcDataConversionError::MissingArgument("racks.rack_id"))?;
            if rack_id.as_str().trim().is_empty() || !rack_ids.insert(rack_id.clone()) {
                return Err(invalid(
                    "racks must contain non-blank, unique rack identifiers",
                ));
            }
            let mut devices = Vec::with_capacity(rack.members.len());
            for member in rack.members {
                if member.manufacturer.trim().is_empty() || member.id.trim().is_empty() {
                    return Err(invalid("member manufacturer and id must not be blank"));
                }
                let device_type = member
                    .r#type
                    .parse()
                    .map_err(|_| invalid("member type must be Compute, Switch or PowerShelf"))?;
                if !members.insert((
                    member.r#type.clone(),
                    member.manufacturer.clone(),
                    member.id.clone(),
                )) {
                    return Err(invalid("duplicate device member across racks"));
                }
                devices.push(ExpectedRackGroupMember {
                    device_type,
                    manufacturer: member.manufacturer,
                    id: member.id,
                });
            }
            racks.push(ExpectedRackGroupRack {
                rack_id,
                members: devices,
            });
        }
        let metadata = Metadata::try_from(value.metadata.unwrap_or_default())?;
        metadata
            .validate(false)
            .map_err(|e| invalid(&e.to_string()))?;
        Ok(Self {
            rack_group_id,
            topology: RackGroupTopology::new(value.topology),
            racks,
            metadata,
        })
    }
}

impl From<ExpectedRackGroupRack> for rpc::forge::ExpectedRackGroupRack {
    fn from(rack: ExpectedRackGroupRack) -> Self {
        Self {
            rack_id: Some(rack.rack_id),
            members: rack
                .members
                .into_iter()
                .map(|m| rpc::forge::ExpectedRackGroupMember {
                    r#type: m.device_type.to_string(),
                    manufacturer: m.manufacturer,
                    id: m.id,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_uuid::rack::{RackGroupId, RackId};

    use super::*;

    fn wire() -> rpc::forge::ExpectedRackGroup {
        rpc::forge::ExpectedRackGroup {
            rack_group_id: Some(RackGroupId::new("54f74aea-76eb-4f0a-aab2-b607136f4d35")),
            topology: "gb200_nvl72r1_c2g4".to_string(),
            racks: vec![
                rpc::forge::ExpectedRackGroupRack {
                    rack_id: Some(RackId::new("rack-02")),
                    members: vec![rpc::forge::ExpectedRackGroupMember {
                        r#type: "Compute".to_string(),
                        manufacturer: "NVIDIA".to_string(),
                        id: "device-01".to_string(),
                    }],
                },
                rpc::forge::ExpectedRackGroupRack {
                    rack_id: Some(RackId::new("rack-01")),
                    members: vec![],
                },
            ],
            metadata: None,
        }
    }

    #[test]
    fn expected_rack_group_conversion() {
        let source = wire();
        let group = ExpectedRackGroup::try_from(source.clone()).unwrap();
        let back: rpc::forge::ExpectedRackGroup = group.into();
        assert_eq!(back.racks, source.racks);
        assert_eq!(back.topology, source.topology);
        type Mutation = fn(&mut rpc::forge::ExpectedRackGroup);
        let cases: &[(&str, Mutation)] = &[
            ("missing identity", |v| v.rack_group_id = None),
            ("blank topology", |v| v.topology = " ".to_string()),
            ("duplicate rack", |v| v.racks.push(v.racks[0].clone())),
            ("duplicate device across racks", |v| {
                let m = v.racks[0].members[0].clone();
                v.racks[1].members.push(m);
            }),
            ("missing rack identity", |v| v.racks[0].rack_id = None),
            ("blank device identity", |v| {
                v.racks[0].members[0].id.clear()
            }),
            ("REST spelling rejected", |v| {
                v.racks[0].members[0].r#type = "NVSwitch".into()
            }),
            ("lowercase spelling rejected", |v| {
                v.racks[0].members[0].r#type = "switch".into()
            }),
            ("unknown device type", |v| {
                v.racks[0].members[0].r#type = "Other".into()
            }),
        ];
        for (name, modify) in cases {
            let mut input = wire();
            modify(&mut input);
            assert!(ExpectedRackGroup::try_from(input).is_err(), "{name}");
        }
        let mut empty = wire();
        empty.racks.clear();
        assert!(ExpectedRackGroup::try_from(empty).is_ok());
    }

    #[test]
    fn expected_rack_group_id_boundaries() {
        for (id, valid) in [
            ("".to_string(), false),
            (" \t".to_string(), false),
            ("é".repeat(128), true),
            ("x".repeat(129), false),
        ] {
            let mut input = wire();
            input.rack_group_id = Some(RackGroupId::new(&id));
            assert_eq!(
                validate_rack_group_id(input.rack_group_id.as_ref().unwrap()).is_ok(),
                valid
            );
            assert_eq!(ExpectedRackGroup::try_from(input).is_ok(), valid);
        }
    }

    #[test]
    fn expected_rack_group_metadata_boundaries() {
        for (name_len, description_len, valid) in
            [(256, 1024, true), (257, 1024, false), (256, 1025, false)]
        {
            let mut input = wire();
            input.metadata = Some(rpc::forge::Metadata {
                name: "n".repeat(name_len),
                description: "d".repeat(description_len),
                labels: vec![],
            });
            assert_eq!(
                ExpectedRackGroup::try_from(input).is_ok(),
                valid,
                "name={name_len}, description={description_len}"
            );
        }
    }

    #[test]
    fn expected_rack_group_member_types_round_trip() {
        for name in ["Compute", "Switch", "PowerShelf"] {
            let mut input = wire();
            input.racks[0].members[0].r#type = name.into();
            let group = ExpectedRackGroup::try_from(input.clone()).unwrap();
            let stored = serde_json::to_value(&group.racks[0].members).unwrap();
            assert_eq!(stored[0]["type"], name);
            let members: Vec<ExpectedRackGroupMember> = serde_json::from_value(stored).unwrap();
            assert_eq!(members, group.racks[0].members);
            let output: rpc::forge::ExpectedRackGroup = group.into();
            assert_eq!(output.racks[0].members, input.racks[0].members);
        }
    }
}
