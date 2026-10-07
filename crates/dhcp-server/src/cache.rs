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

/// Cache DHCP responses from API server
///
/// We usually get about four DHCP requests from the same host in rapid succession, so this
/// prevents us asking the API server every time. Cache is optional, and contents should
/// be short lived.
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use lru::LruCache;
use rpc::forge::DhcpRecord;

/// Data in cache is only valid this long
const MACHINE_CACHE_TIMEOUT: Duration = Duration::from_secs(60);
/// How many entries to keep. After that we evict the entry used the longest ago.
pub const MACHINE_CACHE_SIZE: usize = 1000;
/// If the cache key comes out shorter than this something went wrong, don't use it.
const MIN_KEY_LEN: usize = 10;

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub(super) dhcp_record: DhcpRecord,
    timestamp: Instant,
}

/// Fetch an entry from the cache.
///
/// Result is owned by the caller, it is a clone of cached item.
/// Takes a global lock on the cache.
/// Returns None if we don't have that item in cache, or if we did but
/// it's no longer valid (e.g. too old).
pub(super) fn get(
    mac_address: &str,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
    vendor_id: &str,
    cache: &mut LruCache<String, CacheEntry>,
) -> Option<CacheEntry> {
    let key = &key(mac_address, link_address, circuit_id, remote_id, vendor_id);
    if key.len() < MIN_KEY_LEN {
        tracing::debug!(key = key.as_str(), "Unexpected cache key, skipping");
        return None;
    }
    if let Some(entry) = cache.get(key) {
        if !entry.has_expired() {
            return Some(entry.clone());
        } else {
            tracing::debug!(?mac_address, "removed expired cached response");
            let _removed = cache.pop_entry(key);
        }
    }
    None
}

/// Insert or update an item in the cache
pub(super) fn put(
    mac_address: &str,
    link_address: IpAddr,
    circuit_id: Option<String>,
    remote_id: Option<String>,
    vendor_id: &str,
    dhcp_record: DhcpRecord,
    machine_cache: &mut LruCache<String, CacheEntry>,
) {
    let key = key(
        mac_address,
        link_address,
        &circuit_id,
        &remote_id,
        vendor_id,
    );
    let new_entry = CacheEntry {
        timestamp: Instant::now(),
        dhcp_record,
    };
    machine_cache.put(key, new_entry);
}

//
// Internals
//

// Unique identifier for this entry
fn key(
    mac_address: &str,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
    vendor_id: &str,
) -> String {
    format!(
        "{}_{}_{}_{}_{}",
        mac_address,
        link_address,
        match circuit_id {
            Some(cid) => cid.as_str(),
            None => "",
        },
        match remote_id {
            Some(rid) => rid.as_str(),
            None => "",
        },
        vendor_id,
    )
}

impl CacheEntry {
    fn has_expired(&self) -> bool {
        self.timestamp.elapsed() >= MACHINE_CACHE_TIMEOUT
    }
}
