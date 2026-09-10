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

//! The seam between the simulator and whatever owns the simulated hardware.
//!
//! The simulator holds no inventory of its own. It answers from whatever its
//! host reports, so RMS can never contradict the Redfish view of the same
//! device. A multi-pod gateway can implement this trait by fanning out across
//! several machine-a-tron pods without the simulator knowing.

/// Hardware state, as seen by the simulator's host.
pub trait RmsInventory: Send + Sync + 'static {
    /// Every device the host currently simulates.
    ///
    /// Returning an owned snapshot rather than a borrow keeps the host free to
    /// hold its devices behind whatever lock it likes without that lock being
    /// held across the `.await` of an RPC.
    fn nodes(&self) -> Vec<SimNode>;
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
/// Addresses are carried as strings normalised by the host rather than as
/// parsed types, so that this crate stays free of address-parsing
/// dependencies and the host decides what "the same MAC" means once.
#[derive(Clone, Debug, Default)]
pub struct SimNode {
    pub kind: Option<SimNodeKind>,
    /// Lower-case hex, no separators.
    pub bmc_mac: Option<String>,
    pub bmc_ip: Option<String>,
    /// The host-side (NVOS, for a switch) MAC, lower-case hex, no separators.
    /// Callers that identify a switch without its BMC, such as a password
    /// rotation, send this one.
    pub host_mac: Option<String>,
    pub host_ip: Option<String>,
    pub rack_id: Option<String>,
    /// Physical slot as the chassis reports it. Compute trays report their
    /// chassis slot, switch trays the rack unit they occupy.
    pub slot_number: Option<u32>,
    /// Index of the tray within its rack: among the compute trays for a
    /// compute tray, among the switch trays for a switch tray.
    pub tray_index: Option<u32>,
}

/// Normalise a MAC for comparison: lower-case, separators removed.
///
/// RMS clients send MACs in whatever form their own inventory holds, so
/// matching on the raw string loses nodes for purely cosmetic reasons.
pub fn normalize_mac(mac: &str) -> String {
    mac.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::normalize_mac;

    #[test]
    fn mac_normalisation_ignores_separators_and_case() {
        assert_eq!(normalize_mac("02:00:AB:cd:12:34"), "0200abcd1234");
        assert_eq!(normalize_mac("02-00-ab-CD-12-34"), "0200abcd1234");
        assert_eq!(normalize_mac("0200abcd1234"), "0200abcd1234");
    }
}
