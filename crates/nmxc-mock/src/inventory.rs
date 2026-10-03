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

//! The seam between the mock and whatever owns the simulated hardware.
//!
//! The mock holds no hardware inventory of its own. It answers from whatever
//! its host reports, so the GPUs NMX-C lists for a rack are necessarily the
//! ones the rack's machines report through discovery, matched by the same
//! fabric GUID.

use std::net::IpAddr;
use std::sync::Arc;

use uuid::Uuid;

/// Simulated racks, as seen by the mock's host.
pub trait NmxcInventory: Send + Sync + 'static {
    /// Every NVLink domain the host currently simulates.
    ///
    /// The snapshot is shared rather than owned so that a host can hand the
    /// same one to every request until its hardware changes, and so that the
    /// host's own locks are never held across the `.await` of an RPC.
    fn domains(&self) -> Arc<[SimDomain]>;
}

/// One NVLink domain: a rack and the controller that would serve it.
#[derive(Clone, Debug, Default)]
pub struct SimDomain {
    /// Stable identity of the domain across snapshots; the mock keeps the
    /// domain's partition table under it. The rack id, for a host that has
    /// one.
    pub key: String,
    /// Reported as `domain_uuid` on every response.
    pub domain_uuid: Uuid,
    /// The NVOS management addresses of the domain's switches. A request
    /// addressed to any of them is answered by this domain.
    pub nvos_ips: Vec<IpAddr>,
    pub switches: Vec<SimSwitch>,
    pub compute_nodes: Vec<SimComputeNode>,
}

impl SimDomain {
    /// Every GPU uid in the domain, in compute-node order.
    pub fn gpu_uids(&self) -> impl Iterator<Item = u64> + '_ {
        self.compute_nodes
            .iter()
            .flat_map(|node| node.gpus.iter().map(|gpu| gpu.uid))
    }
}

/// An NVLink switch tray.
#[derive(Clone, Debug, Default)]
pub struct SimSwitch {
    pub chassis_serial: String,
    /// The rack unit the tray occupies.
    pub slot_number: u32,
    /// Index of the tray among the rack's switch trays.
    pub tray_index: u32,
    /// NVLink switch chips on the tray.
    pub num_switches: u32,
}

/// A compute tray, located the way its GPUs' `platform_info` locates it.
#[derive(Clone, Debug, Default)]
pub struct SimComputeNode {
    pub chassis_serial: String,
    pub slot_number: u32,
    pub tray_index: u32,
    pub host_id: u32,
    pub gpus: Vec<SimGpu>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SimGpu {
    /// The GPU's fabric GUID. NICo matches partition membership to machines
    /// by this value, so it must be the one the machine reports in discovery.
    pub uid: u64,
    pub module_id: u32,
}

/// An [`NmxcInventory`] whose domains are fixed at construction.
///
/// Tests and hosts with nothing to report use this in place of a live
/// inventory.
pub struct StaticInventory(Arc<[SimDomain]>);

impl StaticInventory {
    pub fn new(domains: Arc<[SimDomain]>) -> Self {
        Self(domains)
    }
}

impl NmxcInventory for StaticInventory {
    fn domains(&self) -> Arc<[SimDomain]> {
        Arc::clone(&self.0)
    }
}
