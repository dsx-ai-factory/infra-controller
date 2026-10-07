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

use carbide_utils::none_if_empty::NoneIfEmpty;
use mac_address::MacAddress;
use model::expected_switch::{ExpectedSwitch, ExpectedSwitchRequest, LinkedExpectedSwitch};
use model::metadata::Metadata;
use uuid::Uuid;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<ExpectedSwitch> for rpc::forge::ExpectedSwitch {
    fn from(expected_switch: ExpectedSwitch) -> Self {
        rpc::forge::ExpectedSwitch {
            expected_switch_id: expected_switch
                .expected_switch_id
                .map(|u| crate::common::Uuid {
                    value: u.to_string(),
                }),
            bmc_mac_address: expected_switch.bmc_mac_address.to_string(),
            nvos_mac_addresses: expected_switch
                .nvos_mac_addresses
                .iter()
                .map(|m| m.to_string())
                .collect(),
            bmc_username: expected_switch.bmc_username,
            bmc_password: expected_switch.bmc_password,
            switch_serial_number: expected_switch.serial_number,
            nvos_username: expected_switch.nvos_username,
            nvos_password: expected_switch.nvos_password,
            bmc_ip_address: expected_switch
                .bmc_ip_address
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            nvos_ip_address: expected_switch.nvos_ip_address.map(|ip| ip.to_string()),
            metadata: Some(expected_switch.metadata.into()),
            rack_id: expected_switch.rack_id,
            bmc_retain_credentials: expected_switch.bmc_retain_credentials.filter(|&v| v),
        }
    }
}

impl TryFrom<rpc::forge::ExpectedSwitch> for ExpectedSwitch {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedSwitch) -> Result<Self, Self::Error> {
        let bmc_mac_address = MacAddress::try_from(rpc.bmc_mac_address.as_str())
            .map_err(|_| RpcDataConversionError::InvalidMacAddress(rpc.bmc_mac_address.clone()))?;
        let nvos_mac_addresses = rpc
            .nvos_mac_addresses
            .into_iter()
            .map(|s| {
                MacAddress::try_from(s.as_str())
                    .map_err(|_| RpcDataConversionError::InvalidMacAddress(s))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expected_switch_id = rpc
            .expected_switch_id
            .map(|u| {
                Uuid::parse_str(&u.value)
                    .map_err(|_| RpcDataConversionError::InvalidArgument(u.value))
            })
            .transpose()?;
        let metadata = Metadata::try_from(rpc.metadata.unwrap_or_default())?;
        let bmc_ip_address = if rpc.bmc_ip_address.is_empty() {
            None
        } else {
            rpc.bmc_ip_address.parse().ok()
        };
        let nvos_ip_address = rpc
            .nvos_ip_address
            .as_deref()
            .none_if_empty()
            .map(|s| {
                s.parse()
                    .map_err(|_| RpcDataConversionError::InvalidArgument(s.to_string()))
            })
            .transpose()?;

        Ok(ExpectedSwitch {
            expected_switch_id,
            bmc_mac_address,
            bmc_username: rpc.bmc_username,
            bmc_password: rpc.bmc_password,
            serial_number: rpc.switch_serial_number,
            nvos_username: rpc.nvos_username,
            nvos_password: rpc.nvos_password,
            bmc_ip_address,
            nvos_ip_address,
            metadata,
            rack_id: rpc.rack_id,
            nvos_mac_addresses,
            bmc_retain_credentials: rpc.bmc_retain_credentials,
        })
    }
}

impl TryFrom<rpc::forge::ExpectedSwitchRequest> for ExpectedSwitchRequest {
    type Error = RpcDataConversionError;

    fn try_from(rpc: rpc::forge::ExpectedSwitchRequest) -> Result<Self, Self::Error> {
        let expected_switch_id = rpc
            .expected_switch_id
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

        Ok(ExpectedSwitchRequest {
            expected_switch_id,
            bmc_mac_address,
        })
    }
}

impl From<LinkedExpectedSwitch> for rpc::forge::LinkedExpectedSwitch {
    fn from(l: LinkedExpectedSwitch) -> rpc::forge::LinkedExpectedSwitch {
        rpc::forge::LinkedExpectedSwitch {
            switch_serial_number: l.serial_number,
            bmc_mac_address: l.bmc_mac_address.to_string(),
            switch_id: l.switch_id,
            expected_switch_id: l.expected_switch_id.map(|id| crate::common::Uuid {
                value: id.to_string(),
            }),
            explored_endpoint_address: l.address.map(|addr| addr.to_string()),
            rack_id: l.rack_id,
        }
    }
}
