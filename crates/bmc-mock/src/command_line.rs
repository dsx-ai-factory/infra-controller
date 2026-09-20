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
use std::str::FromStr;

use bmc_mock::{DpuFirmwareVersions, HardwareType};
use clap::{Args as ClapArgs, Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum MachineRole {
    Host,
    Dpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum StateBackend {
    Internal,
    Libvirt,
}

fn parse_hardware_profile(value: &str) -> Result<HardwareType, String> {
    let hardware_type = serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| format!("unknown hardware profile: {value}"))?;
    match hardware_type {
        HardwareType::LiteOnPowerShelf
        | HardwareType::DeltaPowerShelf
        | HardwareType::NvidiaSwitchNd5200Ld
        | HardwareType::NvidiaSwitchN5700Ld => {
            Err(format!("hardware profile is not a host or DPU: {value}"))
        }
        hardware_type => Ok(hardware_type),
    }
}

#[derive(Clone, Parser, Debug)]
pub(super) struct IpRouterPair {
    pub(super) ip_address: String,
    pub(super) targz: std::path::PathBuf,
}

impl From<String> for IpRouterPair {
    fn from(value: String) -> Self {
        let mut parts = value.split(',');
        let ip_address = parts.next().unwrap();
        let targz = parts.next().unwrap();
        let targz = PathBuf::from_str(targz).unwrap();

        IpRouterPair {
            ip_address: ip_address.to_owned(),
            targz,
        }
    }
}

#[derive(Clone, ClapArgs, Debug, Default)]
pub(super) struct DpuFirmwareArgs {
    #[clap(
        long = "dpu-bmc-firmware",
        value_name = "VERSION",
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU BMC version in generated firmware inventory"
    )]
    bmc: Option<String>,

    #[clap(
        long = "dpu-uefi-firmware",
        value_name = "VERSION",
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU UEFI version in generated firmware inventory"
    )]
    uefi: Option<String>,

    #[clap(
        long = "dpu-bsp-firmware",
        value_name = "VERSION",
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Add the DPU BSP version to generated firmware inventory"
    )]
    bsp: Option<String>,

    #[clap(
        long = "dpu-cec-firmware",
        value_name = "VERSION",
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU CEC version in generated firmware inventory"
    )]
    cec: Option<String>,

    #[clap(
        long = "dpu-nic-firmware",
        value_name = "VERSION",
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU NIC version in generated firmware inventory"
    )]
    nic: Option<String>,
}

impl DpuFirmwareArgs {
    pub(super) fn is_empty(&self) -> bool {
        self.bmc.is_none()
            && self.uefi.is_none()
            && self.bsp.is_none()
            && self.cec.is_none()
            && self.nic.is_none()
    }
}

impl From<DpuFirmwareArgs> for DpuFirmwareVersions {
    fn from(value: DpuFirmwareArgs) -> Self {
        Self {
            bmc: value.bmc,
            uefi: value.uefi,
            bsp: value.bsp,
            cec: value.cec,
            nic: value.nic,
        }
    }
}

#[derive(Clone, Parser, Debug)]
#[command(after_long_help = "\
DPU FIRMWARE OVERRIDES:

Firmware overrides are optional, opaque version strings for generated DPU endpoints.
The hardware profile determines their Redfish inventory IDs. Omitted BMC, UEFI, CEC,
and NIC values retain the profile inventory; omitting BSP leaves it absent. An exposed
DPU reports the configured versions. A generated host also adds the primary DPU's
explicitly configured versions without replacing colliding host inventory IDs.

The inventory mapping supports generated BlueField-3 and BlueField-4 profiles using
generation-specific Redfish IDs. The profile must resolve to at least one DPU; set
--dpu-count to a positive value for variable-count profiles. Explicit internal mode
also requires --machine-role and --state-backend=internal. The libvirt shorthand uses
--hardware-profile with --libvirt-domain and implies the host role and libvirt state
backend. Firmware overrides cannot be combined with --targz or --ip-router.
")]
pub(super) struct Args {
    #[clap(short, long)]
    pub(super) cert_path: Option<String>,

    #[clap(short, long)]
    pub(super) port: Option<u16>,

    #[clap(
        long,
        help = "Path to .tar.gz file of redfish data to output. Create it from libredfish tests/mockups/<vendor>"
    )]
    pub(super) targz: Option<std::path::PathBuf>,

    #[clap(
        long,
        help = "An ip_address and .tar.gz file pair (comma separated).\nThe file is an archive of redfish data when the request is forwarded to a specific IP address.\nRepeat for different machines"
    )]
    pub(super) ip_router: Option<Vec<IpRouterPair>>,

    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Require Redfish authentication on generated routers (disabled by default); use the profile credentials to rotate its factory password through AccountService before ordinary reads"
    )]
    pub(super) redfish_auth: bool,

    #[clap(
        long,
        value_name = "SECONDS",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Keep the generated BMC offline, answering 503, for this many seconds after Manager.Reset or the /ipmi mock action bmc_cold_reset; omitted or 0 makes a reset instantaneous"
    )]
    pub(super) bmc_reset_duration: Option<u64>,

    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Start an IPMI/SOL simulator for the generated BMC mock"
    )]
    pub(super) enable_ipmi_simulation: bool,

    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Back the generated BMC with the named libvirt domain"
    )]
    pub(super) libvirt_domain: Option<String>,

    #[clap(
        long,
        value_parser = parse_hardware_profile,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Redfish hardware profile for an explicitly configured host or DPU, using its existing snake_case name"
    )]
    pub(super) hardware_profile: Option<HardwareType>,

    #[clap(
        long,
        value_enum,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Expose a host BMC or one DPU BMC"
    )]
    pub(super) machine_role: Option<MachineRole>,

    #[clap(
        long,
        value_enum,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Use an in-process power-state simulator or a libvirt domain"
    )]
    pub(super) state_backend: Option<StateBackend>,

    #[clap(
        long,
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "DPU count for a variable-count profile, or an assertion for a fixed-count profile"
    )]
    pub(super) dpu_count: Option<u8>,

    #[clap(
        long,
        requires = "hardware_profile",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Zero-based DPU index when --machine-role=dpu"
    )]
    pub(super) dpu_index: Option<usize>,

    #[clap(
        long,
        default_value_t = 0,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Stable instance number used to make generated identities unique"
    )]
    pub(super) instance_index: u8,

    #[clap(
        long,
        default_value = "qemu:///system",
        requires = "libvirt_domain",
        conflicts_with_all = ["targz", "ip_router"],
    )]
    pub(super) libvirt_uri: String,

    #[clap(
        long,
        default_value = "virsh",
        requires = "libvirt_domain",
        conflicts_with_all = ["targz", "ip_router"],
    )]
    pub(super) virsh_path: PathBuf,

    #[clap(flatten, next_help_heading = "DPU firmware overrides")]
    pub(super) dpu_firmware: DpuFirmwareArgs,
}

pub(super) fn parse_args() -> Args {
    Args::parse()
}
