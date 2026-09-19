/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::HashSet;

use model::expected_rack_group::{ExpectedRackGroup, ExpectedRackGroupMember, RackGroupTopology};
use model::metadata::Metadata;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<ExpectedRackGroup> for rpc::forge::ExpectedRackGroup {
    fn from(group: ExpectedRackGroup) -> Self {
        Self {
            rack_group_id: Some(group.rack_group_id),
            topology: group.topology.to_string(),
            rack_ids: group.rack_ids,
            members: group
                .members
                .into_iter()
                .map(|member| rpc::forge::ExpectedRackGroupMember {
                    r#type: member.device_type,
                    manufacturer: member.manufacturer,
                    id: member.id,
                })
                .collect(),
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
        if rack_group_id.as_str().trim().is_empty() || rack_group_id.as_str().chars().count() > 128
        {
            return Err(invalid(
                "rack_group_id must contain 1 to 128 characters and not be blank",
            ));
        }
        if value.topology.trim().is_empty() || value.topology.chars().count() > 128 {
            return Err(invalid(
                "topology must contain 1 to 128 characters and not be blank",
            ));
        }
        let mut racks = HashSet::new();
        for rack in &value.rack_ids {
            if rack.as_str().trim().is_empty() || !racks.insert(rack) {
                return Err(invalid(
                    "rack_ids must contain non-blank, unique identifiers",
                ));
            }
        }
        let mut members = HashSet::new();
        for member in &value.members {
            if member.r#type.trim().is_empty()
                || member.manufacturer.trim().is_empty()
                || member.id.trim().is_empty()
            {
                return Err(invalid(
                    "member type, manufacturer and id must not be blank",
                ));
            }
            if !members.insert((&member.r#type, &member.manufacturer, &member.id)) {
                return Err(invalid("duplicate device member"));
            }
        }
        let metadata = Metadata::try_from(value.metadata.unwrap_or_default())?;
        metadata
            .validate(false)
            .map_err(|e| invalid(&e.to_string()))?;
        Ok(Self {
            rack_group_id,
            topology: RackGroupTopology::new(value.topology),
            rack_ids: value.rack_ids,
            members: value
                .members
                .into_iter()
                .map(|member| ExpectedRackGroupMember {
                    device_type: member.r#type,
                    manufacturer: member.manufacturer,
                    id: member.id,
                })
                .collect(),
            metadata,
        })
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
            rack_ids: vec![RackId::new("rack-02"), RackId::new("rack-01")],
            members: vec![rpc::forge::ExpectedRackGroupMember {
                r#type: "compute-tray".to_string(),
                manufacturer: "NVIDIA".to_string(),
                id: "device-01".to_string(),
            }],
            metadata: None,
        }
    }

    #[test]
    fn expected_rack_group_conversion() {
        let source = wire();
        let group = ExpectedRackGroup::try_from(source.clone()).unwrap();
        let back: rpc::forge::ExpectedRackGroup = group.into();
        assert_eq!(back.rack_ids, source.rack_ids);
        assert_eq!(back.members, source.members);
        assert_eq!(back.topology, source.topology);
        let cases: &[(&str, fn(&mut rpc::forge::ExpectedRackGroup))] = &[
            ("missing identity", |v| v.rack_group_id = None),
            ("blank topology", |v| v.topology = " ".to_string()),
            ("duplicate rack", |v| v.rack_ids.push(v.rack_ids[0].clone())),
            ("duplicate device", |v| v.members.push(v.members[0].clone())),
            ("blank device identity", |v| v.members[0].id.clear()),
        ];
        for (name, modify) in cases {
            let mut input = wire();
            modify(&mut input);
            assert!(ExpectedRackGroup::try_from(input).is_err(), "{name}");
        }
        let mut empty = wire();
        empty.rack_ids.clear();
        empty.members.clear();
        assert!(ExpectedRackGroup::try_from(empty).is_ok());
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
}
