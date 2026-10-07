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

use std::sync::atomic::{AtomicUsize, Ordering};

use mac_address::MacAddress;

#[derive(Copy, Clone, Debug)]
pub(crate) struct MacAddressPoolConfig {
    /// The first mac address in the pool as a byte array
    pub(crate) start: [u8; 6],
    /// The amount of addresses in the pool
    pub(crate) length: usize,
}

#[derive(Debug)]
pub struct MacAddressPool {
    /// Defines which addresses are available in the pool
    config: MacAddressPoolConfig,
    /// How many addresses have already been allocated
    used: AtomicUsize,
}

impl MacAddressPool {
    pub(crate) fn new(config: MacAddressPoolConfig) -> Self {
        Self {
            config,
            used: AtomicUsize::new(0),
        }
    }

    /// Allocates a unique MAC address from the pool
    ///
    /// Will panic once the pool is depleted
    pub fn allocate(&self) -> MacAddress {
        let offset = self.used.fetch_add(1, Ordering::SeqCst);
        if offset >= self.config.length {
            panic!("Mac address pool with config {:?} is depleted", self.config);
        }

        let mut u64_address = to_u64_be(self.config.start);
        u64_address += offset as u64;

        let mut bytes = [0u8; 6];
        // The MAC address is stored by `to_u64_be` stored in the last 6 bytes
        bytes.copy_from_slice(&u64_address.to_be_bytes()[2..8]);

        MacAddress::new(bytes)
    }

    /// Returns whether an address is part of the pool
    #[cfg(test)]
    pub(crate) fn contains(&self, address: MacAddress) -> bool {
        let a = to_u64_be(address.bytes());
        let min = to_u64_be(self.config.start);

        (min..min + self.config.length as u64).contains(&a)
    }
}

lazy_static::lazy_static! {
    /// Pool of DPU MAC addresses
    pub(crate) static ref DPU_OOB_MAC_ADDRESS_POOL: MacAddressPool =
        MacAddressPool::new(MacAddressPoolConfig {
            start: [0x11, 0x11, 0x11, 0x11, 0x0, 0x0],
            length: 65536,
        });

    /// Pool of DPU BMC MAC addresses
    pub(crate) static ref DPU_BMC_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x11, 0x11, 0x22, 0x22, 0x0, 0x0],
        length: 65536,
    });

    /// Pool of Host MAC addresses
    pub(crate) static ref HOST_MAC_ADDRESS_POOL: MacAddressPool =
        MacAddressPool::new(MacAddressPoolConfig {
            start: [0x22, 0x22, 0x11, 0x11, 0x0, 0x0],
            length: 65536,
        });

    /// Pool of Host BMC MAC addresses
    pub(crate) static ref HOST_BMC_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x22, 0x22, 0x22, 0x22, 0x0, 0x0],
        length: 65536,
    });

    /// Pool of Host non-DPU MAC addresses
    pub(crate) static ref HOST_NON_DPU_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x33, 0x33, 0x11, 0x11, 0x0, 0x0],
        length: 65536,
    });

    /// Pool of Expected Switch BMC MAC addresses
    pub(crate) static ref EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x44, 0x44, 0x11, 0x11, 0x0, 0x0],
        length: 65536,
    });

    /// Pool of Expected Power Shelf BMC MAC addresses
    pub static ref EXPECTED_POWER_SHELF_BMC_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x44, 0x44, 0x22, 0x22, 0x0, 0x0],
        length: 65536,
    });

    /// Pool of Expected Switch NVOS MAC addresses
    pub(crate) static ref EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL: MacAddressPool =
    MacAddressPool::new(MacAddressPoolConfig {
        start: [0x44, 0x44, 0x33, 0x33, 0x0, 0x0],
        length: 65536,
    });
}

fn to_u64_be(bytes: [u8; 6]) -> u64 {
    u64::from_be_bytes([
        0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ])
}
