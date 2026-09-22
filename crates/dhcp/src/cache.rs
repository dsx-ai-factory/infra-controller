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
///
/// The cache is a static because we are called from Kea's hooks, potentially from multiple threads.
use std::{
    net::{IpAddr, Ipv6Addr},
    sync::Mutex,
    time::{Duration, Instant},
};

use lazy_static::lazy_static;
use lru::LruCache;
use mac_address::MacAddress;
use rpc::forge as rpc;

use crate::machine::Machine;

/// Data in cache is only valid this long
const MACHINE_CACHE_TIMEOUT: Duration = Duration::from_secs(60);
// For negative caching, the TTL should be longer
const MACHINE_DISC_FAILED_CACHE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
// After this many discovery failures, negative caching stops calling the
// Carbide API and returns `TooManyFailuresError` to Kea.
pub(super) const MAX_DISCOVERY_FAILS: u32 = 5;
/// How many entries to keep. After that we evict the entry used the longest ago.
const MACHINE_CACHE_SIZE: usize = 1000;
/// If the cache key comes out shorter than this something went wrong, don't use it.
const MIN_KEY_LEN: usize = 10;

lazy_static! {
    static ref MACHINE_CACHE: Mutex<LruCache<String, CacheEntry>> = Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(MACHINE_CACHE_SIZE).unwrap()
    ));
    static ref INVALIDATED_V6_LEASES: Mutex<LruCache<String, Instant>> = Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(MACHINE_CACHE_SIZE).unwrap()
    ));
}

#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    timestamp: Instant,
    pub(super) status: CacheEntryStatus,
}

#[derive(Debug, Clone)]
pub(super) enum CacheEntryStatus {
    ValidEntry(Box<Machine>),
    DiscoveryFailing(u32),
    DiscoveryFailed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum CacheClass {
    Lease,
    OptionsOnly,
}

/// Cache namespace for entries that share identity fields but differ by protocol behavior.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) struct CacheScope {
    pub(super) address_family: rpc::AddressFamily,
    pub(super) cache_class: CacheClass,
}

/// Fetch an entry from the cache.
///
/// Result is owned by the caller, it is a clone of cached item.
/// Takes a global lock on the cache.
/// Returns None if we don't have that item in cache, or if we did but
/// it's no longer valid (e.g. too old).
pub(super) fn get(
    address_family: rpc::AddressFamily,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
    vendor_id: &str,
) -> Option<CacheEntry> {
    get_classed(
        address_family,
        CacheClass::Lease,
        mac_address,
        link_address,
        circuit_id,
        remote_id,
        vendor_id,
    )
}

/// Fetch an entry from the cache with a protocol-specific cache class.
pub(super) fn get_classed(
    address_family: rpc::AddressFamily,
    cache_class: CacheClass,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
    vendor_id: &str,
) -> Option<CacheEntry> {
    let key = &key(
        address_family,
        cache_class,
        mac_address,
        link_address,
        circuit_id,
        remote_id,
        vendor_id,
    );
    if key.len() < MIN_KEY_LEN {
        log::debug!("Unexpected cache key, skipping: '{key}'");
        return None;
    }
    let mut cache = MACHINE_CACHE.lock().unwrap();
    if let Some(entry) = cache.get(key) {
        if !entry.has_expired() {
            // Lease expiry tombstones are stronger than the normal cache TTL:
            // stale API responses must not be reused after reclaim starts.
            if address_family == rpc::AddressFamily::V6
                && cache_class == CacheClass::Lease
                && matches_invalidated_v6_lease(&entry.status, mac_address)
            {
                log::warn!(
                    "removed cached DHCPv6 response for recently expired lease: mac={mac_address}"
                );
                let _removed = cache.pop_entry(key);
                return None;
            }
            return Some(entry.clone());
        } else {
            log::debug!("removed expired cached response for {mac_address:?}");
            let _removed = cache.pop_entry(key);
        }
    }
    None
}

/// Fetch non-expired entries matching all key fields except vendor class.
///
/// This also performs opportunistic cache cleanup for expired or invalidated
/// entries found while scanning matching vendor variants.
pub(super) fn get_classed_any_vendor(
    address_family: rpc::AddressFamily,
    cache_class: CacheClass,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
) -> Vec<CacheEntry> {
    let prefix = key_prefix(
        address_family,
        cache_class,
        mac_address,
        link_address,
        circuit_id,
        remote_id,
    );
    let mut expired = Vec::new();
    let mut matches = Vec::new();
    let mut cache = MACHINE_CACHE.lock().unwrap();
    for (key, entry) in cache.iter() {
        if !key.starts_with(&prefix) {
            continue;
        }
        // CONFIRM may search across vendor variants; keep the expiry tombstone
        // semantics identical to the exact-key path.
        let invalidated_v6_lease = address_family == rpc::AddressFamily::V6
            && cache_class == CacheClass::Lease
            && matches_invalidated_v6_lease(&entry.status, mac_address);
        if entry.has_expired() || invalidated_v6_lease {
            expired.push(key.clone());
        } else {
            matches.push(entry.clone());
        }
    }
    // Remove stale matches after iteration so the LRU iterator is not mutated
    // while it is still borrowed.
    for key in expired {
        let _removed = cache.pop_entry(&key);
    }
    matches
}

/// Insert or update an item in the cache
pub(super) fn put(
    address_family: rpc::AddressFamily,
    mac_address: MacAddress,
    link_address: IpAddr,       // relay address
    circuit_id: Option<String>, // vlan id
    remote_id: Option<String>,
    vendor_id: &str,
    status: CacheEntryStatus,
) {
    put_classed(
        CacheScope {
            address_family,
            cache_class: CacheClass::Lease,
        },
        mac_address,
        link_address,
        circuit_id,
        remote_id,
        vendor_id,
        status,
    );
}

/// Insert or update an item in the cache with a protocol-specific cache class.
pub(super) fn put_classed(
    scope: CacheScope,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: Option<String>,
    remote_id: Option<String>,
    vendor_id: &str,
    status: CacheEntryStatus,
) {
    let CacheScope {
        address_family,
        cache_class,
    } = scope;

    if address_family == rpc::AddressFamily::V6
        && cache_class == CacheClass::Lease
        && matches_invalidated_v6_lease(&status, mac_address)
    {
        log::warn!("not caching DHCPv6 response for recently expired lease: mac={mac_address}");
        return;
    }

    let key = key(
        address_family,
        cache_class,
        mac_address,
        link_address,
        &circuit_id,
        &remote_id,
        vendor_id,
    );
    let new_entry = CacheEntry {
        timestamp: Instant::now(),
        status,
    };
    MACHINE_CACHE.lock().unwrap().put(key, new_entry);
}

/// Mark and remove cached DHCPv6 lease entries matching an expired API allocation.
pub(super) fn invalidate_v6_lease(address: Ipv6Addr, mac_address: MacAddress) -> usize {
    INVALIDATED_V6_LEASES.lock().unwrap().put(
        invalidated_v6_lease_key(address, mac_address),
        Instant::now(),
    );

    let key_prefix = format!(
        "{:?}_{:?}_{}_",
        rpc::AddressFamily::V6,
        CacheClass::Lease,
        mac_address
    );
    let mut matched_keys = Vec::new();
    let mut cache = MACHINE_CACHE.lock().unwrap();
    for (key, entry) in cache.iter() {
        if !key.starts_with(&key_prefix) {
            continue;
        }

        // Expiry is scoped to the API-owned address and hook-selected MAC.
        if let CacheEntryStatus::ValidEntry(machine) = &entry.status
            && machine.discovery_info.mac_address == mac_address
            && machine.inner.address.parse::<IpAddr>() == Ok(IpAddr::V6(address))
        {
            matched_keys.push(key.clone());
        }
    }

    let removed = matched_keys.len();
    for key in matched_keys {
        let _removed = cache.pop_entry(&key);
    }
    removed
}

/// Clear the recent-expiry tombstone for a DHCPv6 lease.
pub(super) fn clear_v6_lease_invalidation(address: Ipv6Addr, mac_address: MacAddress) -> bool {
    INVALIDATED_V6_LEASES
        .lock()
        .unwrap()
        .pop_entry(&invalidated_v6_lease_key(address, mac_address))
        .is_some()
}

/// Return whether a Machine points at a recently expired DHCPv6 lease.
pub(super) fn machine_matches_invalidated_v6_lease(machine: &Machine) -> bool {
    match machine.inner.address.parse::<IpAddr>() {
        Ok(IpAddr::V6(address)) => {
            is_v6_lease_invalidated(address, machine.discovery_info.mac_address)
        }
        Ok(IpAddr::V4(_)) | Err(_) => false,
    }
}

//
// Internals
//

fn matches_invalidated_v6_lease(status: &CacheEntryStatus, mac_address: MacAddress) -> bool {
    match status {
        CacheEntryStatus::ValidEntry(machine) => match machine.inner.address.parse::<IpAddr>() {
            Ok(IpAddr::V6(address)) => is_v6_lease_invalidated(address, mac_address),
            Ok(IpAddr::V4(_)) | Err(_) => false,
        },
        CacheEntryStatus::DiscoveryFailing(_) | CacheEntryStatus::DiscoveryFailed => false,
    }
}

fn is_v6_lease_invalidated(address: Ipv6Addr, mac_address: MacAddress) -> bool {
    let key = invalidated_v6_lease_key(address, mac_address);
    let mut invalidated = INVALIDATED_V6_LEASES.lock().unwrap();
    if let Some(timestamp) = invalidated.get(&key)
        && timestamp.elapsed() < MACHINE_CACHE_TIMEOUT
    {
        return true;
    }
    let _removed = invalidated.pop_entry(&key);
    false
}

fn invalidated_v6_lease_key(address: Ipv6Addr, mac_address: MacAddress) -> String {
    format!("{mac_address}_{address}")
}

// Unique identifier for this entry
fn key(
    address_family: rpc::AddressFamily,
    cache_class: CacheClass,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
    vendor_id: &str,
) -> String {
    format!(
        "{}{}",
        key_prefix(
            address_family,
            cache_class,
            mac_address,
            link_address,
            circuit_id,
            remote_id,
        ),
        vendor_id,
    )
}

fn key_prefix(
    address_family: rpc::AddressFamily,
    cache_class: CacheClass,
    mac_address: MacAddress,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
) -> String {
    format!(
        "{:?}_{:?}_{}_{}_{}_{}_",
        address_family,
        cache_class,
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
    )
}

impl CacheEntry {
    fn has_expired(&self) -> bool {
        match &self.status {
            CacheEntryStatus::ValidEntry(_machine) => {
                self.timestamp.elapsed() >= MACHINE_CACHE_TIMEOUT
            }
            _ => self.timestamp.elapsed() >= MACHINE_DISC_FAILED_CACHE_TIMEOUT,
        }
    }
}

impl CacheEntryStatus {
    pub(super) fn increment_fails(&self) -> CacheEntryStatus {
        match self {
            CacheEntryStatus::ValidEntry(_machine) => CacheEntryStatus::DiscoveryFailing(1),
            CacheEntryStatus::DiscoveryFailing(count) => {
                let new_count = count + 1;
                if new_count == MAX_DISCOVERY_FAILS {
                    CacheEntryStatus::DiscoveryFailed
                } else {
                    CacheEntryStatus::DiscoveryFailing(new_count)
                }
            }
            CacheEntryStatus::DiscoveryFailed => CacheEntryStatus::DiscoveryFailed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use ::rpc::forge as rpc;

    use super::*;
    use crate::discovery::Discovery;

    fn test_machine(mac_address: MacAddress, address: Ipv6Addr) -> Machine {
        Machine {
            inner: rpc::DhcpRecord {
                mac_address: mac_address.to_string(),
                address: address.to_string(),
                ..Default::default()
            },
            discovery_info: Discovery {
                relay_address: Ipv4Addr::UNSPECIFIED,
                mac_address,
                _client_system: None,
                vendor_class: None,
                link_select_address: None,
                circuit_id: None,
                remote_id: None,
                desired_address: None,
            },
            vendor_class: None,
        }
    }

    /// Verify identical discovery identity fields cannot collide across IP families.
    ///
    /// The shared hook process serves either protocol, so family must remain a
    /// first-class cache namespace even when all other key fields match.
    #[test]
    fn cache_key_separates_address_families() {
        let mac_address = "02:00:00:00:00:ac".parse::<MacAddress>().unwrap();
        let link_address = IpAddr::V6("2001:db8::1".parse().unwrap());
        let circuit_id = Some("65746830".to_string());
        let remote_id = Some("7261636b2d61".to_string());

        // Hold identity, routing, cache class, and vendor constant so only
        // the protocol-family discriminator can separate the entries.
        let v4_key = key(
            rpc::AddressFamily::V4,
            CacheClass::Lease,
            mac_address,
            link_address,
            &circuit_id,
            &remote_id,
            "HTTPClient",
        );
        let v6_key = key(
            rpc::AddressFamily::V6,
            CacheClass::Lease,
            mac_address,
            link_address,
            &circuit_id,
            &remote_id,
            "HTTPClient",
        );

        assert_ne!(v4_key, v6_key);
    }

    #[test]
    fn invalidated_v6_lease_blocks_stale_positive_cache_reinsert() {
        let mac_address = "02:00:00:00:00:aa".parse::<MacAddress>().unwrap();
        let address = "2001:db8::aa".parse::<Ipv6Addr>().unwrap();
        let link_address = IpAddr::V6("2001:db8::1".parse().unwrap());

        // Seed and invalidate a cached lease entry, as lease6_expire does.
        put_classed(
            CacheScope {
                address_family: rpc::AddressFamily::V6,
                cache_class: CacheClass::Lease,
            },
            mac_address,
            link_address,
            None,
            None,
            "",
            CacheEntryStatus::ValidEntry(Box::new(test_machine(mac_address, address))),
        );
        assert_eq!(invalidate_v6_lease(address, mac_address), 1);

        // A concurrent path trying to reinsert the stale API address is ignored
        // while the expiry tombstone is still live.
        put_classed(
            CacheScope {
                address_family: rpc::AddressFamily::V6,
                cache_class: CacheClass::Lease,
            },
            mac_address,
            link_address,
            None,
            None,
            "",
            CacheEntryStatus::ValidEntry(Box::new(test_machine(mac_address, address))),
        );
        assert!(
            get_classed(
                rpc::AddressFamily::V6,
                CacheClass::Lease,
                mac_address,
                link_address,
                &None,
                &None,
                "",
            )
            .is_none()
        );
    }

    #[test]
    fn cleared_v6_lease_invalidation_allows_positive_cache_reinsert() {
        let mac_address = "02:00:00:00:00:ab".parse::<MacAddress>().unwrap();
        let address = "2001:db8::ab".parse::<Ipv6Addr>().unwrap();
        let link_address = IpAddr::V6("2001:db8::1".parse().unwrap());

        // A disabled expiry response means the API kept ownership, so the
        // temporary tombstone must be removable.
        assert_eq!(invalidate_v6_lease(address, mac_address), 0);
        assert!(clear_v6_lease_invalidation(address, mac_address));

        // Once cleared, the same API-owned lease is allowed back into cache.
        put_classed(
            CacheScope {
                address_family: rpc::AddressFamily::V6,
                cache_class: CacheClass::Lease,
            },
            mac_address,
            link_address,
            None,
            None,
            "",
            CacheEntryStatus::ValidEntry(Box::new(test_machine(mac_address, address))),
        );
        assert!(
            get_classed(
                rpc::AddressFamily::V6,
                CacheClass::Lease,
                mac_address,
                link_address,
                &None,
                &None,
                "",
            )
            .is_some()
        );
    }
}
