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

use std::sync::Arc;

/// Hardware state, as seen by the simulator's host.
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
/// matching on the raw string loses nodes for purely cosmetic reasons. Only
/// the spellings a MAC actually has are accepted: twelve hex digits bare, or
/// as six octets separated consistently by colons or by dashes. Anything else
/// is `None` rather than being stripped down to something that could match a
/// node by accident.
pub fn normalize_mac(mac: &str) -> Option<String> {
    let octets: Vec<&str> = match mac.len() {
        12 if mac.is_ascii() => (0..12).step_by(2).map(|i| &mac[i..i + 2]).collect(),
        17 if mac.is_ascii() && matches!(&mac[2..3], ":" | "-") => mac.split(&mac[2..3]).collect(),
        _ => return None,
    };
    let well_formed = octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|b| b.is_ascii_hexdigit()));
    well_formed.then(|| octets.concat().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::normalize_mac;

    #[test]
    fn mac_normalisation_ignores_separators_and_case() {
        for spelling in ["02:00:AB:cd:12:34", "02-00-ab-CD-12-34", "0200abcd1234"] {
            assert_eq!(
                normalize_mac(spelling).as_deref(),
                Some("0200abcd1234"),
                "{spelling}"
            );
        }
    }

    #[test]
    fn anything_that_is_not_a_mac_is_rejected() {
        let near_misses = [
            ("02:00:AB:CD:12:34!", "trailing punctuation"),
            ("0200abcd1234!", "trailing punctuation, bare"),
            ("02:00:AB:CD:12:34:", "trailing separator"),
            (":02:00:AB:CD:12:34", "leading separator"),
            (" 0200abcd1234", "leading whitespace"),
            ("02:00:AB:CD:12", "too short"),
            ("02:00:AB:CD:12:3", "one digit short"),
            ("0200abcd123", "one digit short, bare"),
            ("02:00:AB:CD:12:34:56", "too long"),
            ("0200abcd123456", "too long, bare"),
            ("02:00-AB:CD:12:34", "mixed separators"),
            ("02-00:AB-CD-12-34", "mixed separators, dash first"),
            ("02.00.AB.CD.12.34", "unsupported separator"),
            ("0200.abcd.1234", "dotted"),
            ("02:00:AB:CD:12:3G", "non-hex digit"),
            ("0200abcd12g4", "non-hex digit, bare"),
            ("02:00:AB:CD:12:\u{e9}", "non-ascii, separated"),
            ("0200abcd1\u{e9}4", "non-ascii straddling an octet, bare"),
            ("020:0AB:CD:12:34", "octets of the wrong width"),
            ("", "empty"),
        ];
        for (input, why) in near_misses {
            assert_eq!(normalize_mac(input), None, "{why}: {input:?}");
        }
    }
}
