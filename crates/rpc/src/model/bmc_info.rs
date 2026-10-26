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

use std::net::IpAddr;

use mac_address::MacAddress;
use model::bmc_info::{BmcInfo, UserRoles};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::BmcInfo> for BmcInfo {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::BmcInfo) -> Result<Self, RpcDataConversionError> {
        let mac: Option<MacAddress> = if let Some(mac_address) = value.mac {
            Some(
                mac_address
                    .parse()
                    .map_err(|_| RpcDataConversionError::InvalidMacAddress(mac_address))?,
            )
        } else {
            None
        };

        Ok(BmcInfo {
            machine_interface_id: value.machine_interface_id,
            ip: match value.ip.as_deref() {
                None | Some("") => None,
                Some(ip) => Some(ip.parse::<IpAddr>().map_err(|_| {
                    RpcDataConversionError::InvalidArgument(format!("Invalid BMC IP: {ip}"))
                })?),
            },
            port: value.port.map(|p| p as u16),
            mac,
            version: value.version,
            firmware_version: value.firmware_version,
        })
    }
}

impl From<BmcInfo> for rpc::BmcInfo {
    fn from(value: BmcInfo) -> Self {
        rpc::BmcInfo {
            machine_interface_id: value.machine_interface_id,
            ip: value.ip.map(|ip| ip.to_string()),
            port: value.port.map(|p| p as u32),
            mac: value.mac.map(|mac| mac.to_string()),
            version: value.version,
            firmware_version: value.firmware_version,
        }
    }
}

impl From<&BmcInfo> for rpc::BmcEndpoint {
    fn from(value: &BmcInfo) -> Self {
        Self {
            machine_interface_id: value.machine_interface_id,
            ip: value.ip.map(|ip| ip.to_string()),
            port: value.port.map(u32::from),
            mac: value.mac.map(|mac| mac.to_string()),
        }
    }
}

impl From<&BmcInfo> for rpc::BmcStatus {
    fn from(value: &BmcInfo) -> Self {
        Self {
            version: value.version.clone(),
            firmware_version: value.firmware_version.clone(),
        }
    }
}

impl From<rpc::UserRoles> for UserRoles {
    fn from(action: rpc::UserRoles) -> Self {
        match action {
            rpc::UserRoles::User => UserRoles::User,
            rpc::UserRoles::Administrator => UserRoles::Administrator,
            rpc::UserRoles::Operator => UserRoles::Operator,
            rpc::UserRoles::Noaccess => UserRoles::Noaccess,
        }
    }
}

impl From<UserRoles> for rpc::UserRoles {
    fn from(action: UserRoles) -> Self {
        match action {
            UserRoles::User => rpc::UserRoles::User,
            UserRoles::Administrator => rpc::UserRoles::Administrator,
            UserRoles::Operator => rpc::UserRoles::Operator,
            UserRoles::Noaccess => rpc::UserRoles::Noaccess,
        }
    }
}
