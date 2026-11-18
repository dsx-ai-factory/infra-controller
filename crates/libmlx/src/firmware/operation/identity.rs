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

//! PCI-slot keys for supervisor discovery, not physical-card or reset-domain identity.

use std::io;

use crate::device::discovery::normalize_pci_address;

/// Keep the tool's exact PCI function separate from the shared slot ownership key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeviceIdentity {
    /// Canonical full address passed to firmware tools.
    pub(super) device: String,
    /// Domain-qualified slot shared by every function's supervisor discovery.
    pub(super) key: String,
}

impl DeviceIdentity {
    /// Group every function of one PCI slot while preserving the execution BDF.
    /// Discovery's abbreviated spelling denotes domain zero; MST paths and
    /// other aliases cannot establish the supervisor's PCI identity.
    pub(super) fn parse(device: &str) -> io::Result<Self> {
        let device = normalize_pci_address(device).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "device must be a PCI address [dddd:]bb:ss.f with slot 00..1f and function 0..7",
            )
        })?;
        let key = device[..10].to_string();
        Ok(Self { device, key })
    }
}

/// Bind supervisor status to this boot so stale runtime state cannot match by address alone.
pub(super) fn current_boot_id() -> io::Result<String> {
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    uuid::Uuid::parse_str(value.trim())
        .map(|value| value.to_string())
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::DeviceIdentity;

    /// Slot ownership must group functions without collapsing domains or accepting aliases.
    #[test]
    fn pci_slot_keys_preserve_domains_and_group_functions() {
        scenarios!(run = |device| DeviceIdentity::parse(device)
            .map(|identity| (identity.device, identity.key)).map_err(drop);
            "canonical slot ownership" {
                "0000:AB:1F.0" => Yields(("0000:ab:1f.0".into(), "0000:ab:1f".into())),
                "ab:1f.0" => Yields(("0000:ab:1f.0".into(), "0000:ab:1f".into())),
                "0000:ab:1f.7" => Yields(("0000:ab:1f.7".into(), "0000:ab:1f".into())),
                "0001:ab:1f.0" => Yields(("0001:ab:1f.0".into(), "0001:ab:1f".into())),
            }
            "unsupported aliases and out-of-range components" {
                "/dev/mst/mt4129_pciconf0" => Fails,
                "0000:ab:20.0" => Fails,
                "0000:ab:1f.8" => Fails,
                "0000:ab:1g.0" => Fails,
                "0000:ab:1f.0/../other" => Fails,
            }
        );
    }
}
