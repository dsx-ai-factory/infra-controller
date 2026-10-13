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

//! Static per-host topology for Astra NICs and lookup helpers over it.

/// Astra is currently used in VR machines, and this mapping of physical
/// interface name to pci addresses is static and does not depend on how
/// the actual switch connections are made.
/// Static per-port mapping for an Astra NIC, correlating its
/// DPU and host PCI addresses, physical interface name, and DPU-original
/// interface name. Used as a lookup table in multiple places.
pub struct AstraNicMapping {
    /// DPU-side PCI address.
    pub dpu_pci_address: &'static str,
    /// Host-side PCI address.
    pub host_pci_address: &'static str,
    /// Physical interface name (e.g. `C1-2-L1`), as named in the Cerebro
    /// database.
    pub physical_ifname: &'static str,
    /// DPU original interface name (e.g. `A56p0`).
    pub dpu_original_ifname: &'static str,
}

/// The 32 Astra NIC port mappings for a host.
pub static ASTRA_NIC_MAPPINGS: [AstraNicMapping; 32] = [
    AstraNicMapping {
        dpu_pci_address: "0005:06:00.0",
        host_pci_address: "0001:03:00.0",
        physical_ifname: "C1-2-L1",
        dpu_original_ifname: "A56p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:06:00.1",
        host_pci_address: "0001:03:00.1",
        physical_ifname: "C1-2-L2",
        dpu_original_ifname: "A56p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:06:00.2",
        host_pci_address: "0001:03:00.2",
        physical_ifname: "C1-2-L3",
        dpu_original_ifname: "A56p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:06:00.3",
        host_pci_address: "0001:03:00.3",
        physical_ifname: "C1-2-L4",
        dpu_original_ifname: "A56p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:03:00.0",
        host_pci_address: "0006:01:00.0",
        physical_ifname: "C3-1-L1",
        dpu_original_ifname: "A43p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:03:00.1",
        host_pci_address: "0006:01:00.1",
        physical_ifname: "C3-1-L2",
        dpu_original_ifname: "A43p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:03:00.2",
        host_pci_address: "0006:01:00.2",
        physical_ifname: "C3-1-L3",
        dpu_original_ifname: "A43p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:03:00.3",
        host_pci_address: "0006:01:00.3",
        physical_ifname: "C3-1-L4",
        dpu_original_ifname: "A43p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:03:00.0",
        host_pci_address: "0003:01:00.0",
        physical_ifname: "C1-1-L1",
        dpu_original_ifname: "A53p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:03:00.1",
        host_pci_address: "0003:01:00.1",
        physical_ifname: "C1-1-L2",
        dpu_original_ifname: "A53p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:03:00.2",
        host_pci_address: "0003:01:00.2",
        physical_ifname: "C1-1-L3",
        dpu_original_ifname: "A53p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0005:03:00.3",
        host_pci_address: "0003:01:00.3",
        physical_ifname: "C1-1-L4",
        dpu_original_ifname: "A53p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:06:00.0",
        host_pci_address: "0004:03:00.0",
        physical_ifname: "C3-2-L1",
        dpu_original_ifname: "A46p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:06:00.1",
        host_pci_address: "0004:03:00.1",
        physical_ifname: "C3-2-L2",
        dpu_original_ifname: "A46p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:06:00.2",
        host_pci_address: "0004:03:00.2",
        physical_ifname: "C3-2-L3",
        dpu_original_ifname: "A46p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0004:06:00.3",
        host_pci_address: "0004:03:00.3",
        physical_ifname: "C3-2-L4",
        dpu_original_ifname: "A46p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:06:00.0",
        host_pci_address: "0009:03:00.0",
        physical_ifname: "C7-2-L1",
        dpu_original_ifname: "A6p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:06:00.1",
        host_pci_address: "0009:03:00.1",
        physical_ifname: "C7-2-L2",
        dpu_original_ifname: "A6p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:06:00.2",
        host_pci_address: "0009:03:00.2",
        physical_ifname: "C7-2-L3",
        dpu_original_ifname: "A6p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:06:00.3",
        host_pci_address: "0009:03:00.3",
        physical_ifname: "C7-2-L4",
        dpu_original_ifname: "A6p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:03:00.0",
        host_pci_address: "000e:01:00.0",
        physical_ifname: "C5-1-L1",
        dpu_original_ifname: "A13p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:03:00.1",
        host_pci_address: "000e:01:00.1",
        physical_ifname: "C5-1-L2",
        dpu_original_ifname: "A13p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:03:00.2",
        host_pci_address: "000e:01:00.2",
        physical_ifname: "C5-1-L3",
        dpu_original_ifname: "A13p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:03:00.3",
        host_pci_address: "000e:01:00.3",
        physical_ifname: "C5-1-L4",
        dpu_original_ifname: "A13p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:03:00.0",
        host_pci_address: "000b:01:00.0",
        physical_ifname: "C7-1-L1",
        dpu_original_ifname: "A3p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:03:00.1",
        host_pci_address: "000b:01:00.1",
        physical_ifname: "C7-1-L2",
        dpu_original_ifname: "A3p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:03:00.2",
        host_pci_address: "000b:01:00.2",
        physical_ifname: "C7-1-L3",
        dpu_original_ifname: "A3p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0000:03:00.3",
        host_pci_address: "000b:01:00.3",
        physical_ifname: "C7-1-L4",
        dpu_original_ifname: "A3p3",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:06:00.0",
        host_pci_address: "000c:03:00.0",
        physical_ifname: "C5-2-L1",
        dpu_original_ifname: "A16p0",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:06:00.1",
        host_pci_address: "000c:03:00.1",
        physical_ifname: "C5-2-L2",
        dpu_original_ifname: "A16p1",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:06:00.2",
        host_pci_address: "000c:03:00.2",
        physical_ifname: "C5-2-L3",
        dpu_original_ifname: "A16p2",
    },
    AstraNicMapping {
        dpu_pci_address: "0001:06:00.3",
        host_pci_address: "000c:03:00.3",
        physical_ifname: "C5-2-L4",
        dpu_original_ifname: "A16p3",
    },
];

/// Return the DPU PCI address for the Astra NIC whose physical interface name
/// matches `physical_ifname`, if any.
pub fn astra_dpu_pci_address_for_physical_ifname(physical_ifname: &str) -> Option<&'static str> {
    ASTRA_NIC_MAPPINGS
        .iter()
        .find(|mapping| mapping.physical_ifname == physical_ifname)
        .map(|mapping| mapping.dpu_pci_address)
}

/// Return the cage index for a NIC index in Astra.
///
/// Cage index is `((nic_index / 2) * 2) + 1`.
pub fn astra_get_cage_index(nic_index: u8) -> u8 {
    ((nic_index / 2) * 2) + 1
}

/// Return the MPO index for a NIC index in Astra.
///
/// MPO index is `(nic_index % 2) + 1`.
pub fn astra_get_mpo_index(nic_index: u8) -> u8 {
    (nic_index % 2) + 1
}

/// Return the Cage/MPO-encoded string (e.g. `C1_2`) for a NIC index in Astra.
pub fn astra_get_cage_mpo_encoded_string(nic_index: u8) -> String {
    let cage_index = astra_get_cage_index(nic_index);
    let mpo_index = astra_get_mpo_index(nic_index);
    format!("C{cage_index}-{mpo_index}")
}
