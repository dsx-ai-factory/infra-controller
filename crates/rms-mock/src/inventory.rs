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
//! The mock holds no inventory of its own. It answers from whatever its host
//! reports, so RMS can never contradict the Redfish view of the same device.
//! A multi-pod gateway can implement this trait by fanning out across several
//! machine-a-tron pods without the mock knowing.

use std::net::IpAddr;
use std::sync::Arc;

use mac_address::MacAddress;

/// Hardware state, as seen by the mock's host.
pub trait RmsInventory: Send + Sync + 'static {
    /// Every device the host currently simulates.
    ///
    /// The snapshot is shared rather than owned so that a host can hand the
    /// same one to every request until its hardware changes: a client that
    /// enriches a fleet one request per device would otherwise have the fleet
    /// rebuilt once per device. A snapshot rather than a borrow also keeps the
    /// host free to hold its devices behind whatever lock it likes without
    /// that lock being held across the `.await` of an RPC.
    fn nodes(&self) -> Arc<[SimNode]>;
}

/// What a device is. RMS treats the three kinds differently, and some
/// operations apply only to switches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimNodeKind {
    Compute,
    Switch,
    PowerShelf,
}

/// One simulated device.
///
/// Addresses are typed, so the host hands over the values it already holds
/// and the mock parses the strings a request carries once, at the boundary
/// (see `resolve`). Neither side has to agree on a spelling.
#[derive(Clone, Debug, Default)]
pub struct SimNode {
    pub kind: Option<SimNodeKind>,
    pub bmc_mac: Option<MacAddress>,
    pub bmc_ip: Option<IpAddr>,
    /// The host-side (NVOS, for a switch) MAC. Callers that identify a switch
    /// without its BMC, such as a password rotation, send this one.
    pub host_mac: Option<MacAddress>,
    pub host_ip: Option<IpAddr>,
    pub rack_id: Option<String>,
    /// Physical slot as the chassis reports it. Compute trays report their
    /// chassis slot, switch trays the rack unit they occupy.
    pub slot_number: Option<u32>,
    /// Index of the tray within its rack: among the compute trays for a
    /// compute tray, among the switch trays for a switch tray.
    pub tray_index: Option<u32>,
}
