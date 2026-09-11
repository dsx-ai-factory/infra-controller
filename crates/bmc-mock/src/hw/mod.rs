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

//! Hardware profiles describing how specific devices are represented through Redfish.
//!
//! # Responsibilities
//!
//! A profile receives machine identity and settings and returns fully populated Redfish
//! configs. It owns the platform's resource IDs, manufacturers, models, part numbers,
//! firmware inventory, device counts, topology, and links between devices. It also chooses
//! supported collections, actions, protocol modes, OEM extensions, and their initial values.
//! Keep platform-specific URI choices and payload values here; use Redfish resource helpers
//! and builders to express their wire representation.
//!
//! `machine_info` selects a profile and supplies its inputs. Keep the platform representation
//! in this module rather than growing protocol or platform-payload logic in that dispatcher.
//! Shared hardware components and families belong here too: reuse helpers such as `nic`,
//! `openbmc`, and `nvidia_gbx00` when their modeled behavior matches the platform.
//!
//! Profiles configure protocol behavior before the BMC starts serving requests. They do not
//! implement HTTP handlers or select behavior in response to a request. If a feature needs
//! mutable state, provide its config or select its OEM state variant during construction;
//! subsequent mutations belong to the owning Redfish/OEM state implementation.
//!
//! # Adding or extending a platform
//!
//! 1. Describe the modeled hardware in `hw/<platform>.rs`, with fields for the identity,
//!    component descriptions, and settings needed to construct its representation.
//! 2. Implement the relevant `manager_config`, `system_config`, `chassis_config`, and
//!    `update_service_config` methods, following the config-building shape in `generic_ami`.
//!    Return complete configs using Redfish builders. Configure capabilities explicitly;
//!    use behavior modes for protocol differences instead of requiring a vendor check in
//!    a generic handler.
//! 3. Register the profile here and wire its selection and input construction through
//!    `HardwareType` and `machine_info`. A platform using existing protocol features should
//!    not require platform branches in `redfish`.
//! 4. For an optional collection, preserve the distinction between unsupported (`None`),
//!    supported but empty (`Some(vec![])`), and populated. Use the same capability to drive
//!    parent links and endpoint availability; do not introduce redundant availability flags.
//! 5. If the protocol lacks the required feature, add a configurable implementation in
//!    `redfish` or `redfish/oem/<vendor>`, then enable it explicitly in the profile.
//! 6. Check the observable contract at the narrowest useful layer: inventory identity and
//!    links, supported versus unsupported resources, and any distinct state transition.
//!
//! Preserve quirks of the modeled hardware, including omissions and unusual spelling.
//! Document the scrape or compatibility constraint that justifies them, and identify
//! synthetic fixture values as such. Do not generalize one platform's quirk to every device
//! from that vendor or alter shared defaults merely to accommodate one profile.
//!
//! Existing departures from these boundaries are not templates for new code. Keep their
//! cleanup separate from a platform addition. See `redfish/mod.rs` for protocol-side rules.

/// Description of NIC card.
pub(super) mod nic;

/// Common OpenBMC bmcweb hardware profiles.
pub(super) mod openbmc;

/// Support of NVIDIA Bluefield3 DPU.
pub(super) mod bluefield3;

/// Support of NVIDIA Bluefield4 DPU.
pub(super) mod bluefield4;

/// Generic AMI server.
pub(super) mod generic_ami;

/// Support of HPE ProLiant DL380a Gen11 servers (iLO 6).
pub(super) mod hpe_proliant_dl380a_gen11;

/// Support of Dell PowerEdge R750 servers.
pub(super) mod dell_poweredge_r750;

/// Support of Dell PowerEdge R760 server with Bluefield4 installed.
pub(super) mod dell_poweredge_r760_bf4;

/// Support of Wiwynn GB200 NVL servers.
pub(super) mod wiwynn_gb200_nvl;

/// Rack hardware layouts.
pub(super) mod rack;

/// WIWYNN GB200 NVL72 rack.
pub(super) mod wiwynn_gb200_nvl72_rack;

/// Lenovo GB300 NVL72 rack.
pub(super) mod lenovo_gb300_nvl72_rack;

/// Support of Lenovo GB300 NVL servers.
pub(super) mod lenovo_gb300_nvl;

/// Support of DGX GB300 NVL servers (NVIDIA "GB BMC" host).
pub(super) mod dgx_gb300_nvl;

/// Support of Supermicro (SMC) GB300 NVL servers (Supermicro OpenBMC host).
pub(super) mod supermicro_gb300_nvl;

/// Support of DGX VR NVL servers.
pub(super) mod dgx_vr_nvl;

/// Support of LiteOn Power Shelf.
pub(super) mod liteon_power_shelf;

/// Support of Delta Energy Systems Power Shelf.
pub(super) mod delta_power_shelf;

/// Support of NVIDIA Switch ND5200_LD.
pub(super) mod nvidia_switch_nd5200_ld;

/// Support of NVIDIA Switch N5700_LD.
pub(super) mod nvidia_switch_n5700_ld;

/// Support of NVIDIA DGX H100.
pub(super) mod nvidia_dgx_h100;

/// Common support of GB200 and GB300
pub(super) mod nvidia_gbx00;

/// GB200 CPU/GPU
pub(super) mod nvidia_gb200;

/// GB300 CPU/GPU
pub(super) mod nvidia_gb300;

/// Intel E810 NIC.
pub(super) mod nic_intel_e810;

/// Intel X550 NIC.
pub(super) mod nic_intel_x550;

/// Intel I210 NIC.
pub(super) mod nic_intel_i210;

/// NVIDIA ConnectX-7.
pub(super) mod nic_nvidia_cx7;
