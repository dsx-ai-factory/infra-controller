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

//! SLAs for Machine State Machine Controller

use std::time::Duration;

pub const DPUDISCOVERING: Duration = Duration::from_secs(30 * 60);

// DPUInit any substate other than INIT
// WaitingForPlatformPowercycle WaitingForPlatformConfiguration WaitingForNetworkConfig WaitingForNetworkInstall
pub const DPUINIT_NOTINIT: Duration = Duration::from_secs(30 * 60);

// HostInit state, any substate other than Init and  WaitingForDiscovery
// EnableIpmiOverLan WaitingForPlatformConfiguration PollingBiosSetup UefiSetup Discovered Lockdown PollingLockdownStatus MachineValidating
pub const HOST_INIT: Duration = Duration::from_secs(30 * 60);

// Ready-state boot-interface convergence may include vendor BIOS jobs,
// controlled reboots, and final observation after lockdown is restored.
pub const BOOT_CONFIGURING: Duration = Duration::from_secs(90 * 60);

pub const CONFIGURE_ASTRA: Duration = Duration::from_secs(30 * 60);

pub const WAITING_FOR_CLEANUP: Duration = Duration::from_secs(30 * 60);

pub const CREATED: Duration = Duration::from_secs(30 * 60);

pub const FORCE_DELETION: Duration = Duration::from_secs(30 * 60);

pub const DPU_REPROVISION: Duration = Duration::from_secs(30 * 60);

pub const HOST_REPROVISION: Duration = Duration::from_secs(40 * 60);

pub const MEASUREMENT_WAIT_FOR_MEASUREMENT: Duration = Duration::from_secs(30 * 60);

pub const SPDM_ATTESTATION_TRIGGER: Duration = Duration::from_secs(30);

pub const SPDM_ATTESTATION_RESULT_POLL: Duration = Duration::from_secs(10 * 60);

pub const START_ASSIGNMENT_CYCLE: Duration = Duration::from_secs(60);

pub const BOM_VALIDATION: Duration = Duration::from_secs(5 * 60);

// ASSIGNED state, any substate other than Ready and BootingWithDiscoveryImage
// Init WaitingForNetworkConfig WaitingForStorageConfig WaitingForRebootToReady SwitchToAdminNetwork WaitingForNetworkReconfig DPUReprovision Failed
pub const ASSIGNED: Duration = Duration::from_secs(30 * 60);

// ASSIGNED state, HostPlatformConfiguration substate
pub const ASSIGNED_HOST_PLATFORM_CONFIGURATION: Duration = Duration::from_secs(90 * 60);
pub const VALIDATION: Duration = Duration::from_secs(30 * 60);

pub const MAINTENANCE: Duration = Duration::from_secs(5 * 60);

// BMC credential rotation. A single synchronous Redfish password change
// per device (host + each DPU); generous enough to absorb the up-to-5-minute
// site-explorer pause handshake (its `SITE_EXPLORER_PAUSE_BUDGET`) that precedes
// the change, a slow BMC, and the engine's short per-device backoff without
// tripping the SLA on the first retry.
pub const ROTATING_BMC: Duration = Duration::from_secs(15 * 60);

// Host UEFI credential rotation. Applying a new UEFI password
// requires a BIOS config job plus a full host power-cycle and job-completion
// poll, so the budget is more generous than the synchronous BMC rotation: enough
// to absorb a slow reboot plus the engine's short per-device backoff without
// tripping the SLA on the first retry.
pub const ROTATING_HOST_UEFI: Duration = Duration::from_secs(40 * 60);

// DPU UEFI credential rotation. Applying a new DPU UEFI password
// stages a Bios/Settings change and commits it with a DPU restart (scoped to the
// DPU, not a full host power-cycle). One DPU is converged per cycle, so the
// budget mirrors the host UEFI rotation: enough to absorb a slow DPU restart plus
// the engine's short per-device backoff without tripping the SLA on the first
// retry.
pub const ROTATING_DPU_UEFI: Duration = Duration::from_secs(40 * 60);

/// SLA for NIC lockdown key rotation
pub const ROTATING_NIC_LOCKDOWN: Duration = Duration::from_secs(40 * 60);

/// SLA for Site Explorer suppression acknowledgement during decommissioning.
pub const DECOMMISSIONING_SUPPRESSING_SITE_EXPLORER: Duration = Duration::from_secs(5 * 60);

/// SLA for host deconfiguration during decommissioning (lockdown clear, UEFI
/// password/job work, and UEFI settings reset).
pub const DECOMMISSIONING_DECONFIGURING_HOST: Duration = Duration::from_secs(40 * 60);

/// SLA for DPU deconfiguration during decommissioning (DPF delete and/or vanilla
/// BFB install plus boot).
pub const DECOMMISSIONING_DECONFIGURING_DPUS: Duration = Duration::from_secs(40 * 60);

/// SLA for recording OOB DHCP suppression during decommissioning.
pub const DECOMMISSIONING_SUPPRESSING_OOB_DHCP: Duration = Duration::from_secs(5 * 60);

/// SLA for the host power cycle that forces OOB rediscovery during decommissioning.
pub const DECOMMISSIONING_POWER_CYCLING_HOST: Duration = Duration::from_secs(5 * 60);

/// SLA for powering the host back on after the decommissioning power cycle.
pub const DECOMMISSIONING_POWERING_ON_HOST: Duration = Duration::from_secs(5 * 60);

/// SLA for waiting for OOB DHCP suppression acknowledgement after the host power
/// cycle.
pub const DECOMMISSIONING_WAITING_FOR_OOB_DHCP_ACKNOWLEDGEMENT: Duration =
    Duration::from_secs(15 * 60);

/// SLA for recording BMC DHCP suppression during decommissioning.
pub const DECOMMISSIONING_SUPPRESSING_BMC_DHCP: Duration = Duration::from_secs(5 * 60);

/// SLA for BMC factory reset during decommissioning.
pub const DECOMMISSIONING_FACTORY_RESETTING_BMCS: Duration = Duration::from_secs(5 * 60);

/// SLA for waiting for BMC DHCP suppression acknowledgement after factory reset.
pub const DECOMMISSIONING_WAITING_FOR_BMC_DHCP_ACKNOWLEDGEMENT: Duration =
    Duration::from_secs(15 * 60);

/// SLA for deleting managed per-device BMC and DPU credentials during
/// decommissioning.
pub const DECOMMISSIONING_DELETING_MANAGED_CREDENTIALS: Duration = Duration::from_secs(5 * 60);

/// Configuration for machine state SLA durations.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineSlaConfig {
    /// SLA for the Assigned/BootingWithDiscoveryImage state.
    pub assigned_booting_with_discovery_image: Duration,
}

impl Default for MachineSlaConfig {
    fn default() -> Self {
        // Default failure_retry_time is 90 minutes.
        Self::new(chrono::Duration::minutes(90))
    }
}

impl MachineSlaConfig {
    pub fn new(failure_retry_time: chrono::Duration) -> Self {
        let failure_retry_time = failure_retry_time
            .to_std()
            .unwrap_or(Duration::from_secs(90 * 60));
        Self {
            // Set to 1.1 * failure_retry_time so the SLA fires
            // shortly after the retry would have triggered.
            assigned_booting_with_discovery_image: failure_retry_time * 11 / 10,
        }
    }
}
