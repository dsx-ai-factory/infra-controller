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

use std::path::PathBuf;

use clap::{ArgGroup, Parser};
use libredfish::model::update_service::ComponentType;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Every redfish command targets a BMC: --address is required, --username
and --password are optional, and all are given before the subcommand:

Read the current power state:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-power-state

Power a machine on:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword on

Force a machine off (alias: off):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword force-off

Set PXE first in the boot order:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword boot-pxe

Update host firmware from a local package:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    update-firmware-multipart --filename ./host-fw.bin

")]
pub(crate) struct RedfishAction {
    #[clap(subcommand)]
    pub(super) command: Cmd,

    // A non-`Option` field is required by clap automatically, which renders
    // `--address <ADDRESS>` (unbracketed) in the usage line and rejects a
    // missing value as a clap error (exit 2) — no runtime validation needed.
    #[clap(
        long,
        help = "IP:port of machine BMC. Port is optional and defaults to 443"
    )]
    pub(super) address: String,

    #[clap(long, help = "Username for machine BMC")]
    pub(super) username: Option<String>,

    #[clap(long, help = "Password for machine BMC")]
    pub(super) password: Option<String>,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[clap(rename_all = "kebab_case")]
pub(super) enum Cmd {
    /// List BIOS attributes
    BiosAttrs,
    /// Set hard drive first in boot order
    BootHdd,
    /// Set PXE first in boot order
    BootPxe,
    /// Set Boot order to UEFI Http First
    BootUefiHttp,
    /// On next boot only, boot from hard drive
    BootOnceHdd,
    /// On next boot only, boot from PXE
    BootOncePxe,
    /// Boot rom UEFI HTTP Once
    BootOnceUefiHttp,
    /// Delete all pending jobs
    ClearPending,
    /// Create new BMC user
    CreateBmcUser(BmcUser),
    /// Create new BMC user
    DeleteBmcUser(DeleteBmcUser),
    /// Setup host for use
    MachineSetup(MachineSetupArgs),
    /// Is everything MachineSetup does already done? What's missing?
    MachineSetupStatus(MachineSetupStatusArgs),
    /// Set our password policy
    SetForgePasswordPolicy,
    /// List one or all BIOS boot options
    GetBootOption(BootOptionSelector),
    /// Is this thing on?
    GetPowerState,
    /// Disable BMC/BIOS lockdown
    LockdownDisable,
    /// Enable BMC/BIOS lockdown
    LockdownEnable,
    /// Display status of BMC/BIOS lockdown
    LockdownStatus,
    /// Force turn machine off
    #[clap(alias = "off", verbatim_doc_comment)]
    #[command(after_long_help = "\
EXAMPLES:

Force a machine off (also accepted as the alias `off`):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword force-off

")]
    ForceOff,
    /// Force restart. This is equivalent to pressing the reset button on the front panel.
    /// - Will not restart DPUs
    /// - Will apply pending BIOS/UEFI setting changes
    #[clap(alias = "reset", verbatim_doc_comment)]
    #[command(after_long_help = "\
EXAMPLES:

Force-restart a machine (also accepted as the alias `reset`):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword force-restart

")]
    ForceRestart,
    /// Graceful restart. Asks the OS to restart via ACPI
    /// - Might restart DPUs if no OS is running
    /// - Will not apply pending BIOS/UEFI setting changes
    #[clap(alias = "restart", verbatim_doc_comment)]
    #[command(after_long_help = "\
EXAMPLES:

Gracefully restart a machine (also accepted as the alias `restart`):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword graceful-restart

")]
    GracefulRestart,
    /// Graceful host shutdown
    #[clap(alias = "shutdown", verbatim_doc_comment)]
    #[command(after_long_help = "\
EXAMPLES:

Gracefully shut a machine down (also accepted as the alias `shutdown`):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword graceful-shutdown

")]
    GracefulShutdown,
    /// AC power cycle
    ACPowerCycle,
    /// Power on a machine
    On,
    /// List PCIe devices
    PcieDevices,
    /// List Direct Attached drives
    LocalStorage,
    /// List pending operations
    Pending,
    /// Display power metrics (voltages, power supplies, etc)
    PowerMetrics,
    /// Enable serial console
    SerialEnable,
    /// Serial console status
    SerialStatus,
    /// Display thermal metrics (fans and temperatures)
    ThermalMetrics,
    /// Clear Trusted Platform Module (TPM)
    TpmReset,
    /// Reset BMC to factory defaults
    BmcResetToDefaults,
    /// Reboot the BMC itself
    BmcReset {
        /// Redfish Manager.Reset type. Omit for the vendor default.
        #[clap(long = "reset-type", value_enum)]
        reset_type: Option<crate::bmc_machine::common::ResetTypeArg>,
    },
    /// Get Secure boot status
    GetSecureBoot,
    /// Disable Secure Boot
    DisableSecureBoot,
    /// List Chassis
    GetChassisAll,
    /// List Chassis Subsystem
    GetChassis(Chassis),
    /// Show BMC's Ethernet interface information
    GetBmcEthernetInterfaces,
    /// Show System Ethernet interface information
    GetSystemEthernetInterfaces,
    /// List of existing BMC accounts
    GetBmcAccounts,
    /// Rename an account
    ChangeBmcUsername(BmcUsername),
    /// Change password for a BMC user
    ChangeBmcPassword(BmcPassword),
    /// Change UEFI password
    #[command(after_long_help = "\
EXAMPLES:

Change the UEFI password (supply the current and new values):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    change-uefi-password --current-password mycurrentpassword --new-password mynewpassword

")]
    ChangeUefiPassword(UefiPassword),
    #[clap(about = "DPU specific operations", subcommand)]
    Dpu(DpuOperations),
    /// Get information about the managers
    GetManager,
    /// Update host firmware
    UpdateFirmwareMultipart(Multipart),
    /// Get detailed info on a Redfish task
    GetTask(Task),
    /// Get a list of Redfish tasks
    GetTasks,
    /// Clear UEFI password
    #[command(after_long_help = "\
EXAMPLES:

Clear the UEFI password (supply the current one):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    clear-uefi-password --current-password mycurrentpassword --new-password ''

")]
    ClearUefiPassword(UefiPassword),
    /// Is IPMI enabled over LAN
    IsIpmiOverLanEnabled,
    /// Enable IPMI over LAN
    EnableIpmiOverLan,
    /// Disable IPMI over LAN
    DisableIpmiOverLan,
    /// Get Base Mac Address (DPU only)
    GetBaseMacAddress,
    /// Clear Nvram (Viking only)
    ClearNvram,
    /// Set BIOS options
    SetBios(SetBios),
    /// Reset BIOS settings to factory defaults. Returns once the BMC accepts
    /// the reset request. A system restart is required for the settings to
    /// take effect.
    #[command(after_long_help = "\
EXAMPLES:

Reset BIOS settings to factory defaults:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword reset-bios

Reset BIOS settings and restart the system to apply the change:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    reset-bios --reboot

")]
    ResetBios(ResetBiosArgs),
    /// Get DPU mode
    GetNicMode,
    /// Is infinite boot enable
    IsInfiniteBootEnabled,
    /// Enable infinite boot
    EnableInfiniteBoot,
    /// Set NIC mode (host networking via the NIC)
    SetNicMode,
    /// Set DPU mode (host networking via the DPU)
    SetDpuMode,
    /// Power cycle a machine
    ChassisResetCard1Powercycle,
    /// Set the DPU as the first boot target
    /// Set the boot order so the DPU boots first
    #[command(after_long_help = "\
EXAMPLES:

Set the boot order so the DPU boots first:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    set-boot-order-dpu-first --boot-interface-mac 00:11:22:33:44:55

")]
    SetBootOrderDpuFirst(SetBootOrderDpuFirstArgs),
    /// Get status of the rshim process on DPU
    GetHostRshim,
    /// Enable rshim on DPU
    EnableHostRshim,
    /// Disable rshim on dpu
    DisableHostRshim,
    /// Get the Boss Controller
    GetBossController,
    /// Decommission a storage controller
    DecommissionController(DecomissionControllerArgs),
    /// Create a storage volume
    CreateVolume(CreateVolumeArgs),
    /// Check if boot order is set correctly
    /// Check whether the DPU-first boot order is already configured
    #[command(after_long_help = "\
EXAMPLES:

Check whether the DPU-first boot order is already configured:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    is-boot-order-setup --boot-interface-mac 00:11:22:33:44:55

")]
    IsBootOrderSetup(SetBootOrderDpuFirstArgs),
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[clap(group(ArgGroup::new("selector").required(true).args(&["all", "id"])))]
#[command(after_long_help = "\
EXAMPLES:

List all BIOS boot options:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    get-boot-option --all

Show one boot option by ID:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    get-boot-option --id Boot0001

")]
pub(super) struct BootOptionSelector {
    #[clap(long)]
    all: bool,
    #[clap(long)]
    pub(super) id: Option<String>,
}

#[derive(clap::Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show DPU firmware versions:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware show --all

Show DPU port information:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu ports

")]
pub(super) enum DpuOperations {
    /// BMC's FW Commands
    #[clap(visible_alias = "fw", about = "BMC's FW Commands", subcommand)]
    Firmware(FwCommand),
    /// Show ports information
    Ports(ShowPort),
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Print the DPU firmware update status:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware status

Update the DPU BMC firmware from a package:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware update --package ./bmc-fw.fwpkg

Show all discovered firmware versions:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware show --all

")]
pub(super) enum FwCommand {
    /// Print FW update status
    Status,
    /// Update BMC's FW to the given FW package
    Update(FwPackage),
    /// Show FW versions of different components
    Show(ShowFw),
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Install a DPU BMC firmware package:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware update --package ./bmc-fw.fwpkg

")]
pub(super) struct FwPackage {
    #[clap(short, long, help = "FW package to install")]
    pub(super) package: PathBuf,
}

#[derive(Parser, Debug, PartialEq, Clone)]
pub(super) struct UefiPassword {
    #[clap(long, help = "Current UEFI password")]
    pub(super) current_password: String,
    #[clap(long, help = "New UEFI password")]
    pub(super) new_password: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Rename a BMC account:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    change-bmc-username --old-user svc-ops --new-user svc-platform

")]
pub(super) struct BmcUsername {
    #[clap(long, help = "Old username")]
    pub(super) old_user: String,
    #[clap(long, help = "New username")]
    pub(super) new_user: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Change a BMC user's password:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    change-bmc-password --user svc-ops --new-password 'mynewpassword'

")]
pub(super) struct BmcPassword {
    #[clap(long, help = "New BMC password")]
    pub(super) new_password: String,
    #[clap(long, help = "BMC user")]
    pub(super) user: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Update host firmware from a local file:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    update-firmware-multipart --filename ./host-fw.bin

Update firmware and specify the component type:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    update-firmware-multipart --filename ./uefi.bin --component-type uefi

")]
pub(super) struct Multipart {
    #[clap(long, help = "Local filename for the firmware to be installed")]
    pub(super) filename: String,
    #[clap(
        long,
        help = "Firmware type, ignored by some platforms and optional on others"
    )]
    pub(super) component_type: Option<ComponentType>,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Get details for a Redfish task:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    get-task --taskid JID_123456789012

")]
pub(super) struct Task {
    #[clap(long, help = "Task ID")]
    pub(super) taskid: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show one chassis subsystem by ID:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    get-chassis --chassis-id Chassis-1

")]
pub(super) struct Chassis {
    #[clap(long, help = "Chassis ID")]
    pub(super) chassis_id: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create a BMC user (defaults to the administrator role):
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    create-bmc-user --user svc-ops --new-password 'mynewpassword'

Create a read-only BMC user:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    create-bmc-user --user auditor --new-password 'mynewpassword' --role-id readonly

")]
pub(super) struct BmcUser {
    #[clap(long, help = "BMC password")]
    pub(super) new_password: String,
    #[clap(long, help = "BMC user")]
    pub(super) user: String,
    #[clap(
        long,
        value_enum,
        help = "BMC role for the new account (default: administrator)"
    )]
    pub(super) role_id: Option<crate::bmc_role::BmcRole>,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Delete a BMC user by name:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    delete-bmc-user --user svc-ops

")]
pub(super) struct DeleteBmcUser {
    #[clap(long, help = "BMC user")]
    pub(super) user: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Run the standard machine setup:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    machine-setup --boot-interface-mac 00:11:22:33:44:55

")]
pub(super) struct MachineSetupArgs {
    #[clap(long, help = "boot_interface_mac")]
    pub(super) boot_interface_mac: Option<String>,
    #[clap(long, help = "BIOS profile config in JSON format")]
    pub(super) bios_profiles: Option<String>,
    #[clap(long, help = "BIOS profile to use")]
    pub(super) selected_profile: Option<libredfish::BiosProfileType>,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Check what machine-setup steps remain:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    machine-setup-status --boot-interface-mac 00:11:22:33:44:55

")]
pub(super) struct MachineSetupStatusArgs {
    #[clap(long, help = "boot_interface_mac")]
    pub(super) boot_interface_mac: Option<String>,
}

#[derive(Parser, Debug, PartialEq, Clone)]
pub(super) struct ResetBiosArgs {
    #[clap(
        short,
        long,
        help = "Perform a forced restart after the BMC accepts the BIOS reset request"
    )]
    pub(super) reboot: bool,
}

#[derive(Parser, Debug, PartialEq, Clone)]
pub(super) struct SetBootOrderDpuFirstArgs {
    #[clap(long, help = "boot_interface_mac")]
    pub(super) boot_interface_mac: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Decommission a storage controller:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    decommission-controller --controller-id RAID.Slot.1-1

")]
pub(super) struct DecomissionControllerArgs {
    #[clap(long, help = "controller_id")]
    pub(super) controller_id: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create a volume on a storage controller:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    create-volume --controller-id RAID.Slot.1-1 --volume-name data0

")]
pub(super) struct CreateVolumeArgs {
    #[clap(long, help = "controller_id")]
    pub(super) controller_id: String,
    #[clap(long, help = "volume_name")]
    pub(super) volume_name: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[clap(group(
        ArgGroup::new("show_fw")
        .required(true)
        .args(&["all", "bmc", "dpu_os", "uefi", "fw"])))]
#[command(after_long_help = "\
EXAMPLES:

Show all discovered firmware key/values:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware show --all

Show just the DPU OS version:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware show --dpu-os

Show a specific firmware type by name:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu firmware show DPU_NIC

")]
pub(super) struct ShowFw {
    #[clap(
        short,
        long,
        action,
        conflicts_with = "fw",
        help = "Show all discovered firmware key/values"
    )]
    pub(super) all: bool,

    #[clap(long, action, conflicts_with = "fw", help = "Show BMC FW Version")]
    pub(super) bmc: bool,

    #[clap(
        long,
        action,
        conflicts_with = "fw",
        help = "Show DPU OS version (shortcut for `show DPU_OS`)"
    )]
    pub(super) dpu_os: bool,

    #[clap(
        long,
        action,
        conflicts_with = "fw",
        help = "Show UEFI version (shortcut for `show DPU_UEFI`)"
    )]
    pub(super) uefi: bool,

    #[clap(
        default_value(""),
        help = "The firmware type to query (e.g. DPU_OS, DPU_UEFI, DPU_NIC), leave empty for all (default)"
    )]
    pub(super) fw: String,
}

#[derive(Parser, Debug, PartialEq, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show all DPU ports:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu ports

Show a single port by name:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    dpu ports eth0

")]
pub(super) struct ShowPort {
    #[clap(
        short,
        long,
        action,
        conflicts_with = "port",
        help = "Show all ports (DEPRECATED)"
    )]
    pub(super) all: bool,

    #[clap(
        default_value(""),
        help = "The port to query (e.g. eth0, eth1), leave empty for all (default)"
    )]
    pub(super) port: String,
}

#[derive(Parser, Debug, Clone, PartialEq)]
#[command(after_long_help = "\
EXAMPLES:

Set one or more BIOS attributes from JSON:
    $ nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword \
    set-bios --attributes '{\"OperatingModes_ChooseOperatingMode\": \"MaximumPerformance\"}'

")]
pub(super) struct SetBios {
    #[clap(
        long,
        help = "BIOS attributes to set in JSON, ex: {\"OperatingModes_ChooseOperatingMode\": \"MaximumPerformance\"}"
    )]
    pub(super) attributes: String,
}
