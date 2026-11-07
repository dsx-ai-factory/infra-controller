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
//! In-process coordination for endpoint exploration.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// Tracks endpoints currently being explored so periodic site exploration and ad-hoc
/// `RefreshEndpointReport` calls do not probe the same BMC at the same time.
///
/// Coordination is per-process only. `nico-api` runs a single replica, so the only window in which
/// two processes could probe the same endpoint is the brief overlap of a rolling deploy, where a
/// duplicate probe is harmless and writes are still guarded by optimistic concurrency. If `nico-api`
/// is ever scaled to multiple active replicas, this would no longer dedupe across them.
#[derive(Clone, Default)]
pub(super) struct EndpointExplorationLocks {
    in_flight: Arc<Mutex<HashSet<IpAddr>>>,
}

/// A claim on exploring a single endpoint. The endpoint is released when this is dropped, including
/// on panic or task cancellation.
pub(super) struct EndpointExplorationGuard {
    in_flight: Arc<Mutex<HashSet<IpAddr>>>,
    bmc_ip: IpAddr,
}

impl Drop for EndpointExplorationGuard {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .expect("EndpointExplorationLocks mutex poisoned")
            .remove(&self.bmc_ip);
    }
}

impl EndpointExplorationLocks {
    /// Try to claim exclusive exploration of `bmc_ip` within this process. Returns `None` if another
    /// task is already exploring it.
    pub(super) fn try_claim(&self, bmc_ip: IpAddr) -> Option<EndpointExplorationGuard> {
        let claimed = self
            .in_flight
            .lock()
            .expect("EndpointExplorationLocks mutex poisoned")
            .insert(bmc_ip);

        claimed.then(|| EndpointExplorationGuard {
            in_flight: self.in_flight.clone(),
            bmc_ip,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, last))
    }

    #[test]
    fn second_claim_for_same_endpoint_is_rejected_until_released() {
        let locks = EndpointExplorationLocks::default();

        let guard = locks.try_claim(ip(1)).expect("first claim should succeed");
        assert!(
            locks.try_claim(ip(1)).is_none(),
            "second claim for the same endpoint should be rejected while held"
        );

        drop(guard);
        assert!(
            locks.try_claim(ip(1)).is_some(),
            "claim should succeed again once the previous guard is dropped"
        );
    }

    #[test]
    fn claims_are_per_endpoint() {
        let locks = EndpointExplorationLocks::default();

        let _guard_a = locks.try_claim(ip(1)).expect("claim for a should succeed");
        assert!(
            locks.try_claim(ip(2)).is_some(),
            "a claim on one endpoint must not block a different endpoint"
        );
    }
}
