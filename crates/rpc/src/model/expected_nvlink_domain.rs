/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Wire validation for administrator-owned NVLink domain declarations.

use std::collections::HashSet;

use mac_address::MacAddress;
use model::expected_nvlink_domain::ExpectedNvLinkDomain;
use uuid::Uuid;

use crate::errors::RpcDataConversionError;
use crate::forge;
use crate::model::expected_rack_group::validate_rack_group_id;

impl TryFrom<forge::CreateExpectedNvLinkDomainRequest> for ExpectedNvLinkDomain {
    type Error = RpcDataConversionError;

    fn try_from(value: forge::CreateExpectedNvLinkDomainRequest) -> Result<Self, Self::Error> {
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
        if value.member_bmc_mac_addresses.is_empty() {
            return Err(invalid("member_bmc_mac_addresses must not be empty"));
        }
        let mut unique = HashSet::new();
        let mut members = Vec::with_capacity(value.member_bmc_mac_addresses.len());
        for value in value.member_bmc_mac_addresses {
            let mac: MacAddress = value
                .parse()
                .map_err(|_| invalid("invalid member BMC MAC address"))?;
            if !unique.insert(mac) {
                return Err(invalid("duplicate member BMC MAC address"));
            }
            members.push(mac);
        }
        members.sort_by_key(|mac| mac.bytes());
        Ok(Self {
            id: Uuid::new_v4(),
            rack_group_id,
            name: value.name,
            member_bmc_mac_addresses: members,
        })
    }
}

impl From<ExpectedNvLinkDomain> for forge::ExpectedNvLinkDomain {
    fn from(domain: ExpectedNvLinkDomain) -> Self {
        Self {
            id: Some(domain.id.into()),
            rack_group_id: Some(domain.rack_group_id),
            name: domain.name,
            member_bmc_mac_addresses: domain
                .member_bmc_mac_addresses
                .into_iter()
                .map(|mac| mac.to_string())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_validation() {
        let request = forge::CreateExpectedNvLinkDomainRequest {
            rack_group_id: Some("group".parse().unwrap()),
            name: None,
            member_bmc_mac_addresses: vec!["00:11:22:33:44:66".into(), "00:11:22:33:44:55".into()],
        };
        type Mutation = fn(&mut forge::CreateExpectedNvLinkDomainRequest);
        let cases: &[(&str, Mutation)] = &[
            ("missing group", |r| r.rack_group_id = None),
            ("empty membership", |r| r.member_bmc_mac_addresses.clear()),
            ("invalid MAC", |r| {
                r.member_bmc_mac_addresses[0] = "invalid".into()
            }),
            ("normalized duplicate", |r| {
                r.member_bmc_mac_addresses[0] = "00-11-22-33-44-55".into()
            }),
            ("blank name", |r| r.name = Some(" ".into())),
            ("long name", |r| r.name = Some("a".repeat(257))),
        ];
        for (name, mutate) in cases {
            let mut input = request.clone();
            mutate(&mut input);
            assert!(ExpectedNvLinkDomain::try_from(input).is_err(), "{name}");
        }
        let domain = ExpectedNvLinkDomain::try_from(request).unwrap();
        assert!(!domain.id.is_nil());
        assert_eq!(domain.name, None);
        assert_eq!(
            domain.member_bmc_mac_addresses[0].bytes(),
            [0, 17, 34, 51, 68, 85]
        );
    }
}
