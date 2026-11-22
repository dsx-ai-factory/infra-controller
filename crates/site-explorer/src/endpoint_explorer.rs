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

use bmc_platform::{LockdownDesiredState, PowerState, ResetType};
use carbide_redfish::boot_interface::BootInterfaceTarget;
use libredfish::RoleId;
use libredfish::model::service_root::RedfishVendor;
use model::expected_entity::ExpectedEntity;
use model::machine::MachineInterfaceSnapshot;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationError, EndpointExplorationReport, LockdownStatus,
};

use super::{SiteWideBmcPassword, SiteWideBmcPasswordLoader};

/// This trait defines how the `SiteExplorer` will query information about endpoints
#[async_trait::async_trait]
pub trait EndpointExplorer: Send + Sync + 'static {
    /// Loads the site's current BMC root password and its rotation version once per pass.
    async fn load_sitewide_bmc_password(
        &self,
    ) -> Result<SiteWideBmcPassword, EndpointExplorationError>;

    /// Query an endpoint for information
    ///
    /// The query carries the information `MachineInterface` information that is derived
    /// from DHCP requests as well as the information that might have been fetched in
    /// a previous exploration.
    ///
    /// The loader is called only when ingestion needs the site-wide password;
    /// stored or retained BMC credentials do not require it for manual exploration.
    async fn explore_endpoint(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        expected: Option<&ExpectedEntity>,
        last_exploration_error: Option<&EndpointExplorationError>,
        boot_interface: Option<&BootInterfaceTarget>,
        load_sitewide_bmc_password: SiteWideBmcPasswordLoader<'_>,
    ) -> Result<EndpointExplorationReport, EndpointExplorationError>;
}

/// Core has prepared its client and any IPMI restart credential. When Redfish uses
/// nico-bmc-proxy, the proxy authenticates upstream during forwarding. The selected
/// backend may read live state or issue multiple requests when invoked.
pub struct PreparedPowerControl {
    /// The port to recheck if the selected driver can use IPMI for this action.
    pub ipmi_port: Option<u16>,
    /// Await to invoke the prepared backend. Dropping this future before polling suppresses the send.
    pub send: futures_util::future::BoxFuture<'static, Result<(), EndpointExplorationError>>,
}

impl PreparedPowerControl {
    /// An IPMI restart requires the effective port of the latest identifying report.
    /// Unrelated exploration details cannot invalidate this socket.
    pub fn matches_ipmi_report(&self, report: Option<&EndpointExplorationReport>) -> bool {
        self.ipmi_port.is_none_or(|port| {
            report
                .filter(|report| report.service_root.is_some())
                .is_some_and(|report| bmc_runtime::ipmi_port(report) == port)
        })
    }
}

/// Authenticated BMC operations, resolving stored credentials from the
/// endpoint's interface. Kept separate from [`EndpointExplorer`] so BMC ops
/// don't route through the explorer.
#[async_trait::async_trait]
pub trait AuthenticatedBmc: Send + Sync + 'static {
    /// Issues a BMC reset through Redfish. `report` selects the drivers as for
    /// [`Self::redfish_get_power_state`]. A `reset_type` is sent as given; without
    /// one the platform's default reset is used, which the drivers backend knows
    /// only for a BMC `report` identifies and otherwise leaves to the BMC.
    async fn redfish_reset_bmc(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        reset_type: Option<ResetType>,
    ) -> Result<(), EndpointExplorationError>;

    // ipmitool_reset_bmc issues a cold BMC reset through ipmitool.
    async fn ipmitool_reset_bmc(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
    ) -> Result<(), EndpointExplorationError>;

    /// `report` is the BMC's explored endpoint, which selects its drivers.
    /// Without one that recorded the service root, as for BMCs exploration
    /// could not identify, the drivers backend acts on the host system over
    /// plain Redfish.
    async fn redfish_get_power_state(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
    ) -> Result<Option<PowerState>, EndpointExplorationError>;

    /// Select the configured backend and synchronously invoke the requested action.
    async fn redfish_power_control(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        action: ResetType,
    ) -> Result<(), EndpointExplorationError> {
        self.prepare_redfish_power_control(address, interface, report, action)
            .await?
            .send
            .await
    }

    /// Resolve credentials and select the actual mutation client before the caller
    /// checks whether this endpoint is still authorized to receive the command.
    async fn prepare_redfish_power_control(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        action: ResetType,
    ) -> Result<PreparedPowerControl, EndpointExplorationError>;

    /// Reset a Redfish Chassis (e.g. an HGX baseboard) out-of-band via the BMC at
    /// `address`, resolving credentials from `interface`. `chassis_id` is the
    /// Redfish Chassis id (e.g. "HGX_Chassis_0"). Errors if credentials cannot be
    /// loaded or the Redfish request fails.
    async fn redfish_chassis_reset(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        chassis_id: &str,
        action: ResetType,
    ) -> Result<(), EndpointExplorationError>;

    async fn have_credentials(&self, interface: &MachineInterfaceSnapshot) -> bool;

    /// Turns secure boot off for the next boot. `report` selects the drivers;
    /// without one that identifies the BMC the drivers backend fails.
    async fn disable_secure_boot(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
    ) -> Result<(), EndpointExplorationError>;

    /// Moves the platform's full lockdown to `desired`. `report` selects the
    /// drivers; without one that identifies the BMC the drivers backend
    /// fails, as lockdown has no plain Redfish form.
    async fn lockdown(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        desired: LockdownDesiredState,
    ) -> Result<(), EndpointExplorationError>;

    /// Reads the platform's lockdown. `report` selects the drivers as for
    /// [`Self::lockdown`].
    async fn lockdown_status(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
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

    /// Runs libredfish's `machine_setup`, or under the drivers stages only the
    /// BIOS settings.
    async fn machine_setup(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Result<(), EndpointExplorationError>;

    async fn set_boot_order_dpu_first(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        boot_interface: &BootInterfaceTarget,
    ) -> Result<(), EndpointExplorationError>;

    /// Moves the DPU to `mode`. `report` selects the drivers; without one that
    /// identifies the BMC the drivers backend fails.
    async fn set_nic_mode(
        &self,
        address: SocketAddr,
        interface: &MachineInterfaceSnapshot,
        report: Option<&EndpointExplorationReport>,
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
