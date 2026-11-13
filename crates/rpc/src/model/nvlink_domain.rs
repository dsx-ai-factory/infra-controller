/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Wire validation for administrator-owned NVLink domain declarations.

use std::collections::HashSet;

use model::nvlink_domain::{NvLinkDomain, NvLinkDomainMember};
use model::rack_type::RackCapabilityType;
use uuid::Uuid;

use crate::errors::RpcDataConversionError;
use crate::forge;
use crate::model::expected_rack_group::validate_rack_group_id;

impl TryFrom<forge::CreateNvLinkDomainRequest> for NvLinkDomain {
    type Error = RpcDataConversionError;

    fn try_from(value: forge::CreateNvLinkDomainRequest) -> Result<Self, Self::Error> {
        let invalid = |message: &str| RpcDataConversionError::InvalidArgument(message.into());
        let rack_group_id = value
            .rack_group_id
            .ok_or(RpcDataConversionError::MissingArgument("rack_group_id"))?;
        validate_rack_group_id(&rack_group_id)?;
        if value
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty() || name.chars().count() > 256)
        {
            return Err(invalid("name must be nonblank and at most 256 characters"));
        }
        if value.members.is_empty() {
            return Err(invalid("members must not be empty"));
        }
        let mut unique = HashSet::new();
        let mut members = Vec::with_capacity(value.members.len());
        for value in value.members {
            let rack_index = value
                .rack_index
                .ok_or(RpcDataConversionError::MissingArgument("rack_index"))?;
            let slot_index = value
                .slot_index
                .ok_or(RpcDataConversionError::MissingArgument("slot_index"))?;
            let component_type: RackCapabilityType = value
                .component_type
                .parse()
                .map_err(|_| invalid("component_type must be Compute or Switch"))?;
            if component_type == RackCapabilityType::PowerShelf {
                return Err(invalid("component_type must be Compute or Switch"));
            }
            if !unique.insert((rack_index, slot_index, value.component_type)) {
                return Err(invalid("duplicate member position"));
            }
            members.push(NvLinkDomainMember {
                rack_index,
                slot_index,
                component_type,
            });
        }
        members.sort_by_key(|m| (m.rack_index, m.slot_index, m.component_type.to_string()));
        Ok(Self {
            id: Uuid::new_v4(),
            rack_group_id,
            name: value.name,
            members,
        })
    }
}

impl From<NvLinkDomain> for forge::NvLinkDomain {
    fn from(domain: NvLinkDomain) -> Self {
        Self {
            id: Some(domain.id.into()),
            rack_group_id: Some(domain.rack_group_id),
            name: domain.name,
            members: domain
                .members
                .into_iter()
                .map(|m| forge::NvLinkDomainMember {
                    rack_index: Some(m.rack_index),
                    slot_index: Some(m.slot_index),
                    component_type: m.component_type.to_string(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_validation() {
        let request = forge::CreateNvLinkDomainRequest {
            rack_group_id: Some("group".parse().unwrap()),
            name: None,
            members: vec![
                forge::NvLinkDomainMember {
                    rack_index: Some(0),
                    slot_index: Some(0),
                    component_type: "Switch".into(),
                },
                forge::NvLinkDomainMember {
                    rack_index: Some(0),
                    slot_index: Some(0),
                    component_type: "Compute".into(),
                },
            ],
        };
        type Mutation = fn(&mut forge::CreateNvLinkDomainRequest);
        let cases: &[(&str, Mutation)] = &[
            ("missing group", |r| r.rack_group_id = None),
            ("empty membership", |r| r.members.clear()),
            ("missing rack index", |r| r.members[0].rack_index = None),
            ("missing slot index", |r| r.members[0].slot_index = None),
            ("invalid type", |r| {
                r.members[0].component_type = "switch".into()
            }),
            ("power shelf", |r| {
                r.members[0].component_type = "PowerShelf".into()
            }),
            ("duplicate position", |r| {
                r.members[0] = r.members[1].clone()
            }),
            ("blank name", |r| r.name = Some(" ".into())),
            ("long name", |r| r.name = Some("a".repeat(257))),
        ];
        for (name, mutate) in cases {
            let mut input = request.clone();
            mutate(&mut input);
            assert!(NvLinkDomain::try_from(input).is_err(), "{name}");
        }
        let domain = NvLinkDomain::try_from(request).unwrap();
        assert!(!domain.id.is_nil());
        assert_eq!(domain.name, None);
        assert_eq!(
            domain.members[0].component_type,
            RackCapabilityType::Compute
        );
        let wire: forge::NvLinkDomain = domain.into();
        assert_eq!(wire.members[0].rack_index, Some(0));
        assert_eq!(wire.members[0].slot_index, Some(0));
    }
}
