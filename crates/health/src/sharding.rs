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

use crate::endpoint::BmcEndpoint;

pub struct ShardManager {
    pub shard: usize,
    pub shards_count: usize,
}

impl ShardManager {
    /// Check if this shard should monitor a BMC endpoint.
    pub fn should_monitor(&self, endpoint: &BmcEndpoint) -> bool {
        self.should_monitor_key(&endpoint.hash_key())
    }

    pub fn should_monitor_key(&self, key: &str) -> bool {
        if self.shards_count == 1 {
            return true;
        }

        let hash = self.hash_key(key);
        let assigned_shard = hash % self.shards_count;
        assigned_shard == self.shard
    }

    /// FNV-1a 64-bit
    fn hash_key(&self, key: &str) -> usize {
        const FNV_PRIME: u64 = 1099511628211;
        const FNV_OFFSET_BASIS: u64 = 14695981039346656037;

        let mut hash = FNV_OFFSET_BASIS;
        for byte in key.as_bytes() {
            hash = hash.wrapping_mul(FNV_PRIME);
            hash ^= *byte as u64;
        }

        hash as usize
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::rack::RackId;
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::test_support::endpoint_with_creds;
    use crate::endpoint::{BmcAddr, BmcCredentials};

    fn endpoint(mac: &str, rack: Option<&str>) -> BmcEndpoint {
        endpoint_with_creds(
            BmcAddr {
                ip: "10.0.0.1".parse().unwrap(),
                port: Some(443),
                mac: Some(MacAddress::from_str(mac).unwrap()),
            },
            BmcCredentials::UsernamePassword {
                username: "admin".into(),
                password: None,
            },
            None,
            rack.map(RackId::new),
        )
    }

    #[test]
    fn test_single_shard() {
        let manager = ShardManager {
            shard: 0,
            shards_count: 1,
        };
        assert!(manager.should_monitor(&endpoint("42:9e:b1:bd:9d:dd", None)));
    }

    #[test]
    fn test_consistent_hashing() {
        let ep1 = endpoint("42:9e:b1:bd:9d:dd", None);
        let ep2 = endpoint("42:9e:b2:bd:9d:dd", None);

        let managers: Vec<_> = (0..3)
            .map(|shard| ShardManager {
                shard,
                shards_count: 3,
            })
            .collect();

        for (label, ep) in [("ep1", &ep1), ("ep2", &ep2)] {
            let count = managers.iter().filter(|m| m.should_monitor(ep)).count();
            assert_eq!(count, 1, "{label} should be monitored by exactly one shard");
        }
    }

    #[test]
    fn test_should_monitor_key_distribution() {
        for key in ["AA:BB:CC:DD:EE:FF", "11:22:33:44:55:66"] {
            let count = (0..3)
                .map(|shard| ShardManager {
                    shard,
                    shards_count: 3,
                })
                .filter(|m| m.should_monitor_key(key))
                .count();
            assert_eq!(
                count, 1,
                "Key {key} should be assigned to exactly one shard"
            );
        }
    }

    #[test]
    fn test_same_rack_id_same_shard() {
        let ep_a = endpoint("42:9e:b1:bd:9d:dd", Some("rack-7"));
        let ep_b = endpoint("42:9e:b2:bd:9d:dd", Some("rack-7"));

        let managers: Vec<_> = (0..3)
            .map(|shard| ShardManager {
                shard,
                shards_count: 3,
            })
            .collect();

        let shard_a = managers
            .iter()
            .position(|m| m.should_monitor(&ep_a))
            .expect("should be assigned");
        let shard_b = managers
            .iter()
            .position(|m| m.should_monitor(&ep_b))
            .expect("should be assigned");

        assert_eq!(
            shard_a, shard_b,
            "endpoints with the same rack_id should land on the same shard"
        );
    }
}
