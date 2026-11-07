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

use mac_address::MacAddress;
use model::expected_power_shelf::{
    ExpectedPowerShelf, ExpectedPowerShelfRequest, LinkedExpectedPowerShelf,
};
use model::metadata::Metadata;
use uuid::Uuid;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<ExpectedPowerShelf> for rpc::forge::ExpectedPowerShelf {
    fn from(expected_power_shelf: ExpectedPowerShelf) -> Self {
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: expected_power_shelf.expected_power_shelf_id.map(|u| {
                crate::common::Uuid {
                    value: u.to_string(),
                }
            }),
            bmc_mac_address: expected_power_shelf.bmc_mac_address.to_string(),
            bmc_username: expected_power_shelf.bmc_username,
            bmc_password: expected_power_shelf.bmc_password,
            shelf_serial_number: expected_power_shelf.serial_number,
            bmc_ip_address: expected_power_shelf
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            metadata: Some(expected_power_shelf.metadata.into()),
            rack_id: expected_power_shelf.rack_id,
            bmc_retain_credentials: expected_power_shelf.bmc_retain_credentials.filter(|&v| v),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedPowerShelf> for ExpectedPowerShelf {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedPowerShelf) -> Result<Self, Self::Error> {
        let bmc_mac_address = MacAddress::try_from(rpc.bmc_mac_address.as_str())
            .map_err(|_| RpcDataConversionError::InvalidMacAddress(rpc.bmc_mac_address.clone()))?;
        let expected_power_shelf_id = rpc
            .expected_power_shelf_id
            .map(|u| {
                Uuid::parse_str(&u.value)
                    .map_err(|_| RpcDataConversionError::InvalidArgument(u.value))
            })
            .transpose()?;
        let bmc_ip_address = if rpc.bmc_ip_address.is_empty() {
            None
        } else {
            Some(
                rpc.bmc_ip_address
                    .parse()
                    .map_err(|_| RpcDataConversionError::InvalidIpAddress(rpc.bmc_ip_address))?,
            )
        };
        let metadata = Metadata::try_from(rpc.metadata.unwrap_or_default())?;

        Ok(ExpectedPowerShelf {
            expected_power_shelf_id,
            bmc_mac_address,
            bmc_username: rpc.bmc_username,
            bmc_password: rpc.bmc_password,
            serial_number: rpc.shelf_serial_number,
            bmc_ip_address,
            metadata,
            rack_id: rpc.rack_id,
            bmc_retain_credentials: rpc.bmc_retain_credentials,
        })
    }
}

impl TryFrom<rpc::forge::ExpectedPowerShelfRequest> for ExpectedPowerShelfRequest {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedPowerShelfRequest) -> Result<Self, Self::Error> {
        let expected_power_shelf_id = rpc
            .expected_power_shelf_id
            .map(|u| {
                Uuid::parse_str(&u.value)
                    .map_err(|_| RpcDataConversionError::InvalidArgument(u.value))
            })
            .transpose()?;
        let bmc_mac_address = if rpc.bmc_mac_address.is_empty() {
            None
        } else {
            Some(
                MacAddress::try_from(rpc.bmc_mac_address.as_str())
                    .map_err(|_| RpcDataConversionError::InvalidMacAddress(rpc.bmc_mac_address))?,
            )
        };

        Ok(ExpectedPowerShelfRequest {
            expected_power_shelf_id,
            bmc_mac_address,
        })
    }
}

impl From<LinkedExpectedPowerShelf> for rpc::forge::LinkedExpectedPowerShelf {
    fn from(l: LinkedExpectedPowerShelf) -> rpc::forge::LinkedExpectedPowerShelf {
        rpc::forge::LinkedExpectedPowerShelf {
            shelf_serial_number: l.serial_number,
            bmc_mac_address: l.bmc_mac_address.to_string(),
            power_shelf_id: l.power_shelf_id,
            expected_power_shelf_id: l.expected_power_shelf_id.map(|id| crate::common::Uuid {
                value: id.to_string(),
            }),
            explored_endpoint_address: l.address.map(|addr| addr.to_string()),
            rack_id: l.rack_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn expected_power_shelf_bmc_ip_conversion() {
        scenarios!(
            run = |address: &str| {
                ExpectedPowerShelf::try_from(rpc::forge::ExpectedPowerShelf {
                    bmc_mac_address: "02:00:00:00:00:01".to_string(),
                    bmc_ip_address: address.to_string(),
                    ..Default::default()
                })
                .map(|shelf| shelf.bmc_ip_address)
                .map_err(|error| match error {
                    RpcDataConversionError::InvalidIpAddress(address) => address,
                    other => panic!("unexpected conversion error: {other}"),
                })
            };
            "empty means no address" {
                "" => Yields(None),
            }
            "both address families are accepted" {
                "192.0.2.10" => Yields(Some("192.0.2.10".parse::<IpAddr>().unwrap())),
                "2001:db8::10" => Yields(Some("2001:db8::10".parse::<IpAddr>().unwrap())),
            }
            "malformed nonempty addresses retain the invalid input" {
                "not-an-ip" => FailsWith("not-an-ip".to_string()),
            }
        );
    }
}
