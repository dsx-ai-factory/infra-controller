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

use std::collections::HashSet;

use mac_address::MacAddress;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub ranges: Option<RangesConfig>,
    pub pool: Option<PoolConfig>,
}

impl Config {
    fn in_any_range(&self, v: MacAddress) -> bool {
        self.ranges.is_some_and(|r| r.in_any_range(v))
    }

    fn in_pool(&self, v: MacAddress) -> bool {
        self.pool.is_some_and(|p| {
            let base = p.base;
            let host_bits = p.host_bits;
            v.clear_mask_bits(host_bits) == base
        })
    }

    fn in_pool_or_ranges(&self, v: MacAddress) -> bool {
        self.in_pool(v) || self.in_any_range(v)
    }

    fn range_base_for(&self, v: MacAddress) -> Option<MacAddress> {
        if self.in_any_range(v) {
            self.ranges.map(|r| r.range_base_for(v))
        } else {
            None
        }
    }

    /// Number of addresses in row to be skipped to advance to first
    /// address outside of configured ranges.
    fn skip_to_next_outside_ranges(&self, v: MacAddress) -> Option<u64> {
        if self.in_any_range(v) {
            self.ranges
                .map(|r| r.base.to_u64() + (1_u64 << r.host_bits) - v.to_u64())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RangesConfig {
    // MAC address ranges base.
    base: MacAddress,
    // Number of lower bits covered by the whole ranges area.
    host_bits: usize,
    // Number of lower bits covered by each individual range.
    range_host_bits: usize,
}

impl RangesConfig {
    pub fn new(base: MacAddress, host_bits: usize, range_host_bits: usize) -> Result<Self, Error> {
        if host_bits > 48 {
            return Err(Error::InvalidHostBits(host_bits));
        }
        if range_host_bits > host_bits {
            return Err(Error::InvalidRangeHostBits {
                range_host_bits,
                host_bits,
            });
        }
        Ok(Self {
            base: base.bit_and(MacAddress::host_mask(host_bits).bit_not()),
            host_bits,
            range_host_bits,
        })
    }

    pub fn base(&self) -> MacAddress {
        self.base
    }

    fn in_any_range(&self, v: MacAddress) -> bool {
        v.clear_mask_bits(self.host_bits) == self.base
    }

    fn range_base_for(&self, v: MacAddress) -> MacAddress {
        v.clear_mask_bits(self.range_host_bits)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    // Individual MAC address pool base.
    base: MacAddress,
    // Number of lower bits that identify a MAC address inside the pool.
    host_bits: usize,
}

impl PoolConfig {
    pub fn new(base: MacAddress, host_bits: usize) -> Result<Self, Error> {
        if host_bits > 48 {
            return Err(Error::InvalidHostBits(host_bits));
        }
        Ok(Self {
            base: base.bit_and(MacAddress::host_mask(host_bits).bit_not()),
            host_bits,
        })
    }
    pub fn base(&self) -> MacAddress {
        self.base
    }
    pub fn host_bits(&self) -> usize {
        self.host_bits
    }
    pub fn contains(&self, address: MacAddress) -> bool {
        address.clear_mask_bits(self.host_bits) == self.base
    }
}

/// Defines data structure that is able to allocate unique
/// MacAddresses.
pub struct MacAddressPool {
    config: Config,
    allocated: HashSet<MacAddress>,
    allocated_ranges: HashSet<MacAddress>,
    pool_next_addr: u64,
    ranges_next_range: u64,
}

impl MacAddressPool {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            allocated: HashSet::new(),
            allocated_ranges: HashSet::new(),
            pool_next_addr: 1,
            ranges_next_range: 1,
        }
    }

    pub fn new_pool(config: PoolConfig) -> Self {
        Self::new(Config {
            pool: Some(config),
            ranges: None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MAC address {0} is already allocated")]
    AlreadyAllocated(MacAddress),
    #[error("trying to reserve MAC address {mac} is within allocated range {range}")]
    ReservationWithinAllocatedRange { mac: MacAddress, range: MacAddress },
    #[error("MAC address pool exhausted")]
    Exhausted,
    #[error("MAC address pool not configured")]
    PoolNotConfigured,
    #[error("MAC address ranges are not configured")]
    RangesNotConfigured,
    #[error("invalid host bits length: {0} (must be up to 48)")]
    InvalidHostBits(usize),
    #[error("invalid range host bits length: {range_host_bits} > {host_bits}")]
    InvalidRangeHostBits {
        range_host_bits: usize,
        host_bits: usize,
    },
    #[error("MAC address range {base}/{host_bits} is outside configured ranges")]
    RangeOutsideConfiguredRanges { base: MacAddress, host_bits: usize },
}

impl MacAddressPool {
    /// Reserve MacAddress (prevent from further allocation).
    pub fn reserve(&mut self, address: MacAddress) -> Result<(), Error> {
        if !self.config.in_pool_or_ranges(address) {
            return Ok(());
        }
        if let Some(range_base) = self.config.range_base_for(address)
            && self.allocated_ranges.contains(&range_base)
        {
            return Err(Error::ReservationWithinAllocatedRange {
                mac: address,
                range: range_base,
            });
        }

        if self.allocated.insert(address) {
            // If address within ranges pool => block whole
            // corresponding subrange.
            self.maybe_reserve_range(address);
            Ok(())
        } else {
            Err(Error::AlreadyAllocated(address))
        }
    }

    /// Allocate standalone address from common MAC address pool.
    pub fn allocate(&mut self) -> Result<MacAddress, Error> {
        let Some(pool_config) = self.config.pool else {
            return Err(Error::PoolNotConfigured);
        };

        let base = pool_config.base;
        let host_bits = pool_config.host_bits;
        let mut left_tries = 1_u64 << host_bits;
        loop {
            if left_tries == 0 {
                break Err(Error::Exhausted);
            }
            let cur = self.pool_next_addr;
            self.pool_next_addr = self.pool_next_addr.wrapping_add(1);
            left_tries -= 1;
            let addr = MacAddress::from_u64(cur)
                .bit_and(MacAddress::host_mask(host_bits))
                .bit_or(base);
            if let Some(skip) = self.config.skip_to_next_outside_ranges(addr) {
                let skip = skip - 1; // Adjust to already incremented pool_next_addr / decremented left_tries.
                self.pool_next_addr = self.pool_next_addr.wrapping_add(skip);
                if left_tries > skip {
                    left_tries -= skip;
                    continue;
                } else {
                    break Err(Error::Exhausted);
                }
            }
            if self.allocated.insert(addr) {
                self.maybe_reserve_range(addr);
                break Ok(addr);
            }
        }
    }

    pub fn allocate_range_config(&mut self) -> Result<PoolConfig, Error> {
        let Some(ranges_config) = self.config.ranges else {
            return Err(Error::RangesNotConfigured);
        };
        let ranges_count = 1_u64 << (ranges_config.host_bits - ranges_config.range_host_bits);
        let mut left_tries = ranges_count;

        loop {
            if left_tries == 0 {
                break Err(Error::Exhausted);
            }
            left_tries -= 1;

            let cur = self.ranges_next_range;
            self.ranges_next_range = self.ranges_next_range.wrapping_add(1);

            let range_base = MacAddress::from_u64(cur << ranges_config.range_host_bits)
                .bit_and(MacAddress::host_mask(ranges_config.host_bits))
                .bit_or(ranges_config.base);

            if self.allocated_ranges.insert(range_base) {
                break Ok(PoolConfig {
                    base: range_base,
                    host_bits: ranges_config.range_host_bits,
                });
            }
        }
    }

    pub fn reserve_range_config(&mut self, pool: PoolConfig) -> Result<(), Error> {
        let Some(ranges_config) = self.config.ranges else {
            return Err(Error::RangesNotConfigured);
        };

        if pool.host_bits != ranges_config.range_host_bits
            || !ranges_config.in_any_range(pool.base)
            || ranges_config.range_base_for(pool.base) != pool.base
        {
            return Err(Error::RangeOutsideConfiguredRanges {
                base: pool.base,
                host_bits: pool.host_bits,
            });
        }

        self.allocated_ranges.insert(pool.base);
        Ok(())
    }

    fn maybe_reserve_range(&mut self, addr: MacAddress) {
        if let Some(range_base) = self.config.range_base_for(addr) {
            self.allocated_ranges.insert(range_base);
        }
    }
}

trait MacAddressExt: Sized + Copy {
    fn host_mask(len: usize) -> Self;
    fn from_u64(v: u64) -> Self;
    fn to_u64(&self) -> u64;
    fn bit_or(&self, other: Self) -> Self {
        Self::from_u64(self.to_u64() | other.to_u64())
    }
    fn bit_not(&self) -> Self {
        Self::from_u64(!self.to_u64())
    }
    fn bit_and(&self, other: Self) -> Self {
        Self::from_u64(self.to_u64() & other.to_u64())
    }
    fn clear_mask_bits(&self, host_bits: usize) -> Self {
        Self::host_mask(host_bits).bit_not().bit_and(*self)
    }
}

impl MacAddressExt for MacAddress {
    fn host_mask(len: usize) -> Self {
        Self::from_u64((1_u64 << len) - 1)
    }

    fn from_u64(v: u64) -> Self {
        MacAddress::new([
            ((v >> 40) & 0xFF) as u8,
            ((v >> 32) & 0xFF) as u8,
            ((v >> 24) & 0xFF) as u8,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ])
    }

    fn to_u64(&self) -> u64 {
        let bytes = self.bytes();
        ((bytes[0] as u64) << 40)
            | ((bytes[1] as u64) << 32)
            | ((bytes[2] as u64) << 24)
            | ((bytes[3] as u64) << 16)
            | ((bytes[4] as u64) << 8)
            | (bytes[5] as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl MacAddressPool {
        /// Build a child pool for tests that exercise allocations within a
        /// range returned by the production range-allocation API.
        fn allocate_range(&mut self) -> Result<Self, Error> {
            self.allocate_range_config().map(Self::new_pool)
        }
    }

    fn mac(v: u64) -> MacAddress {
        MacAddress::from_u64(v)
    }

    #[test]
    fn allocate_pool_overlapping_ranges_still_finds_free_address_after_range() {
        let config = Config {
            // Ranges occupy MACs 0..=1.
            ranges: Some(RangesConfig {
                base: mac(0),
                host_bits: 1,
                range_host_bits: 1,
            }),
            // Pool occupies MACs 0..=3, so MACs 2 and 3 are valid standalone
            // allocation candidates.
            pool: Some(PoolConfig {
                base: mac(0),
                host_bits: 2,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        let addr = pool
            .allocate()
            .expect("pool has free addresses outside the overlapping ranges");

        assert!(
            !pool.config.in_any_range(addr),
            "allocated address {addr} must not be inside ranges"
        );
    }

    #[test]
    fn allocate_range_skips_subrange_reserved_by_address() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x100),
                host_bits: 3,
                range_host_bits: 1,
            }),
            pool: Some(PoolConfig {
                base: mac(0x1000),
                host_bits: 4,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        // Address 0x102 belongs to subrange base 0x102, so that whole
        // subrange must become unavailable for allocate_range().
        pool.reserve(mac(0x102)).unwrap();

        let child = pool
            .allocate_range()
            .expect("there are other free subranges");

        assert_ne!(
            child.config.pool.unwrap().base,
            mac(0x102),
            "allocate_range() must not allocate a subrange reserved by address"
        );
    }

    #[test]
    fn allocate_range_exhausts_after_all_subranges_are_allocated() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x200),
                host_bits: 2,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);

        pool.allocate_range().unwrap();
        pool.allocate_range().unwrap();

        assert!(matches!(pool.allocate_range(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_range_returns_pool_without_subranges() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x300),
                host_bits: 3,
                range_host_bits: 2,
            }),

            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        let child = pool.allocate_range().unwrap();

        assert!(child.config.ranges.is_none());
        assert_eq!(child.config.pool.unwrap().host_bits, 2);
    }

    #[test]
    fn child_pool_returned_by_allocate_range_cannot_allocate_nested_ranges() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x400),
                host_bits: 3,
                range_host_bits: 2,
            }),

            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        let mut child = pool.allocate_range().unwrap();

        assert!(
            matches!(child.allocate_range(), Err(Error::RangesNotConfigured)),
            "child pool has ranges == None, so it must not allocate nested ranges"
        );
    }

    #[test]
    fn child_pool_can_allocate_entire_allocated_range() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x500),
                host_bits: 2,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        let mut child = pool.allocate_range().unwrap();

        let first = child.allocate().unwrap();
        let second = child
            .allocate()
            .expect("range_host_bits == 1 means child range contains 2 addresses");

        assert_ne!(first, second);
        assert!(matches!(child.allocate(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_does_not_skip_free_address_immediately_after_overlapping_ranges() {
        let config = Config {
            // Ranges occupy MACs 0..=1.
            ranges: Some(RangesConfig {
                base: mac(0),
                host_bits: 1,
                range_host_bits: 1,
            }),

            // Pool occupies MACs 0..=3.
            pool: Some(PoolConfig {
                base: mac(0),
                host_bits: 2,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        // MAC 3 is allocated, MAC 2 is still free.
        pool.reserve(mac(3)).unwrap();

        let addr = pool
            .allocate()
            .expect("MAC 2 is free and immediately after the overlapping ranges");

        assert_eq!(addr, mac(2));
    }

    #[test]
    fn allocate_with_zero_host_bits_allocates_only_pool_base_once() {
        let config = Config {
            ranges: None,

            pool: Some(PoolConfig {
                base: mac(0xabc),
                host_bits: 0,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        assert_eq!(pool.allocate().unwrap(), mac(0xabc));
        assert!(matches!(pool.allocate(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_skips_reserved_address_and_wraps_to_pool_base() {
        let config = Config {
            ranges: None,

            pool: Some(PoolConfig {
                base: mac(0x100),
                host_bits: 2,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        // Allocation order starts at offset 1, then 2, 3, 0.
        pool.reserve(mac(0x101)).unwrap();

        assert_eq!(pool.allocate().unwrap(), mac(0x102));
        assert_eq!(pool.allocate().unwrap(), mac(0x103));
        assert_eq!(pool.allocate().unwrap(), mac(0x100));
        assert!(matches!(pool.allocate(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_exhausts_when_all_pool_addresses_are_reserved() {
        let config = Config {
            ranges: None,

            pool: Some(PoolConfig {
                base: mac(0x200),
                host_bits: 2,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        pool.reserve(mac(0x200)).unwrap();
        pool.reserve(mac(0x201)).unwrap();
        pool.reserve(mac(0x202)).unwrap();
        pool.reserve(mac(0x203)).unwrap();

        assert!(matches!(pool.allocate(), Err(Error::Exhausted)));
    }

    #[test]
    fn reserve_allocated_standalone_address_fails() {
        let config = Config {
            ranges: None,

            pool: Some(PoolConfig {
                base: mac(0x300),
                host_bits: 1,
            }),
        };

        let mut pool = MacAddressPool::new(config);

        let addr = pool.allocate().unwrap();

        assert!(matches!(
            pool.reserve(addr),
            Err(Error::AlreadyAllocated(a)) if a == addr
        ));
    }

    #[test]
    fn allocate_range_with_zero_host_bits_allocates_only_ranges_base_once() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x400),
                host_bits: 0,
                range_host_bits: 0,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);

        assert_eq!(
            pool.allocate_range().unwrap().config.pool.unwrap().base,
            mac(0x400)
        );
        assert!(matches!(pool.allocate_range(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_range_when_range_size_equals_ranges_size_allocates_single_range() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x500),
                host_bits: 3,
                range_host_bits: 3,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);

        let child = pool.allocate_range().unwrap();

        assert_eq!(child.config.pool.unwrap().base, mac(0x500));
        assert_eq!(child.config.pool.unwrap().host_bits, 3);
        assert!(matches!(pool.allocate_range(), Err(Error::Exhausted)));
    }

    #[test]
    fn allocate_range_eventually_returns_every_subrange_once() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x600),
                host_bits: 3,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);

        let mut bases = std::collections::HashSet::new();

        for _ in 0..4 {
            let child = pool.allocate_range().unwrap();
            assert!(
                bases.insert(child.config.pool.unwrap().base),
                "subrange base must be unique"
            );
        }

        assert_eq!(
            bases,
            std::collections::HashSet::from([mac(0x600), mac(0x602), mac(0x604), mac(0x606)])
        );

        assert!(matches!(pool.allocate_range(), Err(Error::Exhausted)));
    }

    #[test]
    fn reserving_one_address_in_each_subrange_exhausts_ranges() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x700),
                host_bits: 3,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        pool.reserve(mac(0x700)).unwrap();
        pool.reserve(mac(0x702)).unwrap();
        pool.reserve(mac(0x704)).unwrap();
        pool.reserve(mac(0x706)).unwrap();
        assert!(matches!(pool.allocate_range(), Err(Error::Exhausted)));
    }

    #[test]
    fn reserving_address_inside_allocated_range_should_fail() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x800),
                host_bits: 2,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        let child = pool.allocate_range().unwrap();
        let addr_inside_child_range = child.config.pool.unwrap().base;
        assert!(
            matches!(
                pool.reserve(addr_inside_child_range),
                Err(Error::ReservationWithinAllocatedRange { mac, .. }) if mac == addr_inside_child_range
            ),
            "parent must not reserve an address inside an already allocated range"
        );
    }

    #[test]
    fn reserve_range_config_marks_subrange_allocated_without_allocating_member_addresses() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x900),
                host_bits: 3,
                range_host_bits: 1,
            }),
            pool: None,
        };

        let mut pool = MacAddressPool::new(config);
        pool.reserve_range_config(PoolConfig {
            base: mac(0x902),
            host_bits: 1,
        })
        .unwrap();

        let expected = mac(0x902);
        assert!(matches!(
            pool.reserve(expected),
            Err(Error::ReservationWithinAllocatedRange { mac, range })
                if mac == expected && range == expected
        ));
    }

    #[test]
    fn reserve_silently_accepts_address_outside_pool_and_ranges() {
        let config = Config {
            ranges: Some(RangesConfig {
                base: mac(0x1000),
                host_bits: 4,
                range_host_bits: 2,
            }),
            pool: Some(PoolConfig {
                base: mac(0x2000),
                host_bits: 4,
            }),
        };
        let mut pool = MacAddressPool::new(config);
        let outside = mac(0x3000);
        pool.reserve(outside).unwrap();
        assert!(!pool.allocated.contains(&outside));
        pool.reserve(outside).unwrap();
    }
}
