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

/// SLA for PowerShelf initialization in seconds
pub(super) const INITIALIZING: u64 = 300; // 5 minutes

/// SLA for PowerShelf fetching data in seconds
pub(super) const FETCHING_DATA: u64 = 300; // 5 minutes

/// SLA for PowerShelf configuring in seconds
pub(super) const CONFIGURING: u64 = 300; // 5 minutes

// /// SLA for PowerShelf ready in seconds
// pub const READY: u64 = 0; // 0 minutes

// /// SLA for PowerShelf error in seconds
// pub const ERROR: u64 = 300; // 5 minutes

/// SLA for PowerShelf deleting in seconds
pub(super) const DELETING: u64 = 300; // 5 minutes

/// SLA for PowerShelf maintenance (PowerOn / PowerOff) in seconds
pub(super) const MAINTENANCE: u64 = 300; // 5 minutes

/// SLA for PowerShelf BMC (PMC) credential rotation in seconds. Generous enough
/// to absorb the up-to-5-minute site-explorer pause handshake (its
/// `SITE_EXPLORER_PAUSE_BUDGET`) that precedes the change, a slow BMC, and the
/// rotation engine's short per-device backoff without tripping the SLA on the
/// first retry.
pub(super) const ROTATING_BMC: u64 = 15 * 60; // 15 minutes

/// SLA for PowerShelf rack-level reprovisioning (firmware wait) in seconds
pub(super) const REPROVISIONING: u64 = 3600; // 1 hour

/// SLA for Site Explorer suppression acknowledgement during decommissioning
pub(super) const DECOMMISSIONING_SUPPRESSING_SITE_EXPLORER: u64 = 300; // 5 minutes

/// SLA for recording BMC DHCP suppression during decommissioning
pub(super) const DECOMMISSIONING_SUPPRESSING_BMC_DHCP: u64 = 300; // 5 minutes

/// SLA for BMC factory reset during decommissioning
pub(super) const DECOMMISSIONING_FACTORY_RESET_BMC: u64 = 300; // 5 minutes

/// SLA for waiting for BMC DHCP suppression acknowledgement after reset.
/// Covers BMC reboot, Kea reload, and the subsequent DHCP discover.
pub(super) const DECOMMISSIONING_WAITING_FOR_BMC_DHCP_ACK: u64 = 15 * 60; // 15 minutes

/// SLA for deleting managed per-device credentials during decommissioning
pub(super) const DECOMMISSIONING_DELETING_MANAGED_CREDENTIALS: u64 = 300; // 5 minutes
