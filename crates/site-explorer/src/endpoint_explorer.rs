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

use std::net::SocketAddr;

use carbide_redfish::boot_interface::BootInterfaceTarget;
use libredfish::RoleId;
use libredfish::model::service_root::RedfishVendor;
use model::expected_entity::ExpectedEntity;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationError, EndpointExplorationReport, LockdownStatus,
};

use super::metrics::SiteExplorationMetrics;

/// This trait defines how the `SiteExplorer` will query information about endpoints
#[async_trait::async_trait]
pub trait EndpointExplorer: Send + Sync + 'static {
    /// Query an endpoint for information
    ///
    /// The query carries the information `MachineInterface` information that is derived
    /// from DHCP requests as well as the information that might have been fetched in
    /// a previous exploration.
    async fn explore_endpoint(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        expected: Option<&ExpectedEntity>,
        last_exploration_error: Option<&EndpointExplorationError>,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError>;

    async fn check_preconditions(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> Result<(), EndpointExplorationError>;
}

/// Authenticated BMC operations, resolving stored credentials from the
/// endpoint's interface. Kept separate from [`EndpointExplorer`] so BMC ops
/// don't route through the explorer.
#[async_trait::async_trait]
pub trait AuthenticatedBmc: Send + Sync + 'static {
    // redfish_reset_bmc issues a BMC reset through redfish.
    async fn redfish_reset_bmc(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        reset_type: Option<libredfish::ManagerResetType>,
    ) -> Result<(), EndpointExplorationError>;

    // ipmitool_reset_bmc issues a cold BMC reset through ipmitool.
    async fn ipmitool_reset_bmc(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError>;

    async fn redfish_get_power_state(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<libredfish::PowerState, EndpointExplorationError>;

    async fn redfish_power_control(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError>;

    /// Reset a Redfish Chassis (e.g. a GPU baseboard) out-of-band via the BMC at
    /// `address`, resolving credentials from `interface`. `chassis_id` is the
    /// Redfish Chassis id (e.g. "HGX_Chassis_0"). Errors if credentials cannot be
    /// loaded or the Redfish request fails.
    async fn redfish_chassis_reset(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        chassis_id: &str,
        action: libredfish::SystemPowerControl,
    ) -> Result<(), EndpointExplorationError>;

    async fn have_credentials(&self, interface: &MachineInterfaceSnapshot) -> bool;

    async fn disable_secure_boot(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError>;

    async fn lockdown(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        action: libredfish::EnabledDisabled,
    ) -> Result<(), EndpointExplorationError>;

    async fn lockdown_status(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<LockdownStatus, EndpointExplorationError>;

    async fn enable_infinite_boot(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError>;

    async fn is_infinite_boot_enabled(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<Option<bool>, EndpointExplorationError>;

    async fn machine_setup(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError>;

    async fn set_boot_order_dpu_first(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError>;

    async fn set_nic_mode(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        mode: BlueFieldOperatingMode,
    ) -> Result<(), EndpointExplorationError>;

    async fn is_viking(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<bool, EndpointExplorationError>;

    async fn clear_nvram(
        &self,
        bmc_ip_address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError>;

    async fn create_bmc_user(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        username: &str,
        password: &str,
        role_id: RoleId,
    ) -> Result<(), EndpointExplorationError>;

    async fn delete_bmc_user(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        username: &str,
    ) -> Result<(), EndpointExplorationError>;

    // set_bmc_root_password authenticates with the endpoint's currently stored
    // BMC root credentials, probes the vendor, sets `new_password` on the
    // device, and records it as the device's per-BMC credential. This is an
    // out-of-band set that does not update rotation convergence; the rotation
    // engine will reassert the site-wide password on its next pass.
    async fn set_bmc_root_password(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        new_password: &str,
    ) -> Result<(), EndpointExplorationError>;

    // probe_bmc_vendor resolves the endpoint's Redfish vendor using its stored
    // BMC root credentials.
    async fn probe_bmc_vendor(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<RedfishVendor, EndpointExplorationError>;
}
