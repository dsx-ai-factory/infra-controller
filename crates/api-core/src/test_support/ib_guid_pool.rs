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

#[derive(Copy, Clone, Debug)]
struct IbGuidPoolConfig {
    /// The first GUID in the pool as a byte array
    start: [u8; 8],
    /// The amount of GUIDs in the pool
    length: usize,
}

#[derive(Debug)]
pub(crate) struct IbGuidPool {
    /// Defines which GUIDs are available in the pool
    config: IbGuidPoolConfig,
    /// How many GUIDs have already been allocated
    used: AtomicUsize,
}

impl IbGuidPool {
    fn new(config: IbGuidPoolConfig) -> Self {
        Self {
            config,
            used: AtomicUsize::new(0),
        }
    }

    /// Allocates a unique GUID from the pool
    ///
    /// Will panic once the pool is depleted
    pub(crate) fn allocate(&self) -> String {
        let offset = self.used.fetch_add(1, Ordering::SeqCst);
        if offset >= self.config.length {
            panic!("GUID pool with config {:?} is depleted", self.config);
        }

        let mut u64_guid = u64::from_be_bytes(self.config.start);
        u64_guid += offset as u64;
        format!("{u64_guid:016x}")
    }
}

lazy_static::lazy_static! {
    /// Pool of IB GUIDs
    pub(crate) static ref IB_GUID_POOL: IbGuidPool =
        IbGuidPool::new(IbGuidPoolConfig {
            start: [0xa, 0xb, 0xc, 0x0, 0x0, 0x0, 0x0, 0x0],
            length: 65536,
        });
}

#[test]
fn test_guid_pool() {
    let pool1 = IbGuidPool::new(IbGuidPoolConfig {
        start: [0x94, 0x6d, 0xae, 0x03, 0x00, 0x2a, 0xc7, 0x52],
        length: 2,
    });
    assert_eq!(pool1.allocate(), "946dae03002ac752");
    assert_eq!(pool1.allocate(), "946dae03002ac753");

    let pool2 = IbGuidPool::new(IbGuidPoolConfig {
        start: [0, 0, 0, 0, 0, 0, 0x1, 0xa],
        length: 2,
    });
    assert_eq!(pool2.allocate(), "000000000000010a");
    assert_eq!(pool2.allocate(), "000000000000010b");

    let pool3 = IbGuidPool::new(IbGuidPoolConfig {
        start: [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe],
        length: 2,
    });
    assert_eq!(pool3.allocate(), "fffffffffffffffe");
    assert_eq!(pool3.allocate(), "ffffffffffffffff");
}
