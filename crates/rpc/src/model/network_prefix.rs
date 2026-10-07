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

use model::network_prefix::{NetworkPrefix, NewNetworkPrefix};

use crate as rpc;
use crate::errors::RpcDataConversionError;

/// `encode_free_ip_count` returns the legacy count, optional wide count, and
/// saturation flag expected by `rpc::forge::NetworkPrefix`.
///
/// `None` keeps the wide field absent, distinguishing skipped accounting from
/// an exact zero. Counts above `u32::MAX` cap only the legacy field; counts
/// above `u64::MAX` cap the wide field and set the flag.
fn encode_free_ip_count(count: Option<u128>) -> (u32, Option<u64>, bool) {
    let Some(count) = count else {
        return (0, None, false);
    };

    let legacy_count = u32::try_from(count).unwrap_or(u32::MAX);
    match u64::try_from(count) {
        Ok(count) => (legacy_count, Some(count), false),
        Err(_) => (legacy_count, Some(u64::MAX), true),
    }
}

impl TryFrom<rpc::forge::NetworkPrefix> for NewNetworkPrefix {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::NetworkPrefix) -> Result<Self, Self::Error> {
        if let Some(_id) = value.id {
            return Err(RpcDataConversionError::IdentifierSpecifiedForNewObject(
                String::from("Network Prefix"),
            ));
        }

        if value.reserve_first < 0 {
            return Err(RpcDataConversionError::InvalidArgument(format!(
                "reserve_first must be non-negative, got {}",
                value.reserve_first
            )));
        }

        Ok(NewNetworkPrefix {
            prefix: value.prefix.parse()?,
            gateway: match value.gateway {
                Some(g) => Some(
                    g.parse()
                        .map_err(|_| RpcDataConversionError::InvalidIpAddress(g))?,
                ),
                None => None,
            },
            dhcpv6_link_address: None,
            num_reserved: value.reserve_first,
        })
    }
}

impl From<NetworkPrefix> for rpc::forge::NetworkPrefix {
    fn from(src: NetworkPrefix) -> Self {
        let (free_ip_count, free_ip_count_v2, free_ip_count_saturated) =
            encode_free_ip_count(src.num_free_ips);
        rpc::forge::NetworkPrefix {
            id: Some(src.id),
            prefix: src.prefix.to_string(),
            gateway: src.gateway.map(|v| v.to_string()),
            reserve_first: src.num_reserved,
            free_ip_count,
            svi_ip: src.svi_ip.map(|x| x.to_string()),
            free_ip_count_v2,
            free_ip_count_saturated,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn network_prefix_rejects_negative_reservations() {
        let prefix = rpc::forge::NetworkPrefix {
            prefix: "2001:db8::/64".to_string(),
            reserve_first: -1,
            ..Default::default()
        };
        let error = NewNetworkPrefix::try_from(prefix).unwrap_err();
        assert!(matches!(
            error,
            RpcDataConversionError::InvalidArgument(message)
                if message.contains("reserve_first") && message.contains("-1")
        ));
    }

    #[test]
    fn free_ip_count_encoding_is_explicit_about_presence_and_saturation() {
        value_scenarios!(run = encode_free_ip_count;
            "exact counts" {
                None => (0, None, false),
                Some(0) => (0, Some(0), false),
                Some(u32::MAX as u128) => (u32::MAX, Some(u32::MAX as u64), false),
                Some(u32::MAX as u128 + 1) => (u32::MAX, Some(u32::MAX as u64 + 1), false),
                Some(u64::MAX as u128) => (u32::MAX, Some(u64::MAX), false),
            }

            "count exceeds the protobuf field" {
                Some(u64::MAX as u128 + 1) => (u32::MAX, Some(u64::MAX), true),
                Some(u128::MAX) => (u32::MAX, Some(u64::MAX), true),
            }
        );
    }
}
