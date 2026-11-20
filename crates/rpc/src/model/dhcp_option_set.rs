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
use std::str::FromStr;

use model::dhcp_option_set::{DhcpOptionSet, DhcpOptions, DhcpOptionsV4, DhcpOptionsV6};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl TryFrom<rpc::DhcpOptions> for DhcpOptions {
    type Error = RpcDataConversionError;

    fn try_from(options: rpc::DhcpOptions) -> Result<Self, Self::Error> {
        let v4 = match options.v4 {
            Some(v4) => Some(DhcpOptionsV4 {
                boot_url: v4.boot_url,
                dns_servers: parse_addresses(&v4.dns_servers)?,
            }),
            None => None,
        };
        let v6 = match options.v6 {
            Some(v6) => Some(DhcpOptionsV6 {
                boot_url: v6.boot_url,
                dns_servers: parse_addresses(&v6.dns_servers)?,
            }),
            None => None,
        };
        Ok(DhcpOptions { v4, v6 })
    }
}

fn parse_addresses<T: FromStr>(addresses: &[String]) -> Result<Vec<T>, RpcDataConversionError> {
    addresses
        .iter()
        .map(|address| {
            address
                .parse()
                .map_err(|_| RpcDataConversionError::InvalidIpAddress(address.clone()))
        })
        .collect()
}

impl From<DhcpOptionSet> for rpc::DhcpOptionSet {
    fn from(option_set: DhcpOptionSet) -> Self {
        rpc::DhcpOptionSet {
            id: Some(option_set.id),
            tenant_organization_id: option_set.tenant_organization_id.to_string(),
            metadata: Some(option_set.metadata.into()),
            options: Some(option_set.options.into()),
            version: option_set.version.to_string(),
            created_at: Some(option_set.created.to_string()),
        }
    }
}

impl From<DhcpOptions> for rpc::DhcpOptions {
    fn from(options: DhcpOptions) -> Self {
        rpc::DhcpOptions {
            v4: options.v4.map(|v4| rpc::DhcpOptionsV4 {
                boot_url: v4.boot_url,
                dns_servers: v4.dns_servers.iter().map(ToString::to_string).collect(),
            }),
            v6: options.v6.map(|v6| rpc::DhcpOptionsV6 {
                boot_url: v6.boot_url,
                dns_servers: v6.dns_servers.iter().map(ToString::to_string).collect(),
            }),
        }
    }
}
