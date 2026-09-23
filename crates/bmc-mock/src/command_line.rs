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
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU BMC version in generated firmware inventory"
    )]
    bmc: Option<String>,

    #[clap(
        long = "dpu-uefi-firmware",
        value_name = "VERSION",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU UEFI version in generated firmware inventory"
    )]
    uefi: Option<String>,

    #[clap(
        long = "dpu-bsp-firmware",
        value_name = "VERSION",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Add the DPU BSP version to generated firmware inventory"
    )]
    bsp: Option<String>,

    #[clap(
        long = "dpu-cec-firmware",
        value_name = "VERSION",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Override the DPU CEC version in generated firmware inventory"
    )]
    cec: Option<String>,

    #[clap(
        long = "dpu-nic-firmware",
        value_name = "VERSION",
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
GENERATED MACHINES:

The default hardware profile is wiwynn_gb200_nvl and the default machine role is host.
Use --hardware-profile to select another profile or --machine-role=dpu to expose
a single DPU BMC. The DPU index defaults to 0.

For fixed-count profiles, --dpu-count must match the profile's DPU count.
For variable-count profiles, the default is 0 for hosts, or --dpu-index + 1
for DPU BMCs. An explicit DPU index is valid only with --machine-role=dpu
and must be less than the resolved DPU count.

STATE BACKEND:

Without --state-backend, --libvirt-domain selects libvirt; otherwise the backend
is internal. Explicit --state-backend=libvirt requires --libvirt-domain.
Explicit --state-backend=internal cannot be combined with --libvirt-domain.

DPU FIRMWARE OVERRIDES:

Firmware overrides are optional, opaque version strings for generated DPU endpoints.
The hardware profile determines their Redfish inventory IDs. Omitted BMC, UEFI, CEC,
and NIC values retain the profile inventory; omitting BSP leaves it absent.

An exposed DPU reports its configured versions. A generated host also includes
the primary DPU's explicitly configured versions without replacing colliding
host inventory IDs. The mapping supports generated BlueField-3 and BlueField-4
profiles. Firmware overrides require at least one DPU.

ARCHIVE MODE:

--targz and --ip-router serve archived Redfish data. They cannot be combined
with generated-machine, backend, firmware, Redfish authentication, BMC reset
duration, or IPMI simulation options.
")]
struct Args {
    #[clap(flatten, next_help_heading = "Listener")]
    listener: ListenerArgs,

    #[clap(flatten, next_help_heading = "Machine configuration")]
    machine: MachineArgs,

    #[clap(flatten, next_help_heading = "BMC behavior")]
    bmc_behaviour: BmcBehaviorArgs,

    #[clap(
        long,
        value_enum,
        help_heading = "State backend",
        conflicts_with_all = ["targz", "ip_router"],
        requires_if("libvirt", "libvirt_domain"),
        help = "Use an in-process power-state simulator or a libvirt domain"
    )]
    state_backend: Option<StateBackend>,

    #[clap(flatten, next_help_heading = "Libvirt backend")]
    libvirt: LibvirtArgs,

    #[clap(flatten, next_help_heading = "Archive mode")]
    targz: TarGzArgs,
}

#[derive(Clone, ClapArgs, Debug, Default)]
pub(super) struct BmcBehaviorArgs {
    #[clap(
        long,
        help_heading = "BMC behavior",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Require Redfish authentication on generated routers (disabled by default); use the profile credentials to rotate its factory password through AccountService before ordinary reads"
    )]
    pub(super) redfish_auth: bool,

    #[clap(
        long,
        help_heading = "BMC behavior",
        value_name = "SECONDS",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Keep the generated BMC offline, answering 503, for this many seconds after Manager.Reset or the /ipmi mock action bmc_cold_reset; omitted or 0 makes a reset instantaneous"
    )]
    pub(super) bmc_reset_duration: Option<u64>,

    #[clap(
        long,
        help_heading = "BMC behavior",
        conflicts_with_all = ["targz", "ip_router"],
        help = "Start an IPMI/SOL simulator for the generated BMC mock"
    )]
    pub(super) enable_ipmi_simulation: bool,
}

#[derive(Clone, ClapArgs, Debug, Default)]
pub(super) struct TarGzArgs {
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
}

#[derive(Clone, ClapArgs, Debug, Default)]
pub(super) struct ListenerArgs {
    #[clap(short, long)]
    pub(super) cert_path: Option<String>,

    #[clap(short, long)]
    pub(super) port: Option<u16>,
}

#[derive(Clone, ClapArgs, Debug)]
pub(super) struct MachineArgs {
    #[clap(
        long,
        value_parser = parse_hardware_profile,
        conflicts_with_all = ["targz", "ip_router"],
        default_value = "wiwynn_gb200_nvl",
        help = "Redfish hardware profile for an explicitly configured host or DPU, using its existing snake_case name"
    )]
    pub(super) hardware_profile: HardwareType,

    #[clap(
        long,
        value_enum,
        conflicts_with_all = ["targz", "ip_router"],
        default_value_t = MachineRole::Host,
        help = "Expose a host BMC or one DPU BMC"
    )]
    pub(super) machine_role: MachineRole,

    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "DPU count for a variable-count profile, or an assertion for a fixed-count profile"
    )]
    pub(super) dpu_count: Option<u8>,

    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Zero-based DPU index when --machine-role=dpu"
    )]
    pub(super) dpu_index: Option<u8>,

    #[clap(
        long,
        default_value_t = 0,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Stable instance number used to make generated identities unique"
    )]
    pub(super) instance_index: u8,

    #[clap(flatten, next_help_heading = "DPU firmware overrides")]
    pub(super) dpu_firmware: DpuFirmwareArgs,
}

#[derive(Clone, ClapArgs, Debug)]
pub(super) struct LibvirtArgs {
    #[clap(
        long,
        conflicts_with_all = ["targz", "ip_router"],
        help = "Back the generated BMC with the named libvirt domain"
    )]
    pub(super) libvirt_domain: Option<String>,

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
}

pub(super) enum AppType {
    TarGzRouter {
        targz: TarGzArgs,
        listener: ListenerArgs,
    },
    LibvirtApp {
        listener: ListenerArgs,
        bmc_behaviour: BmcBehaviorArgs,
        machine: MachineArgs,
        libvirt: LibvirtArgs,
    },
    JustMockApp {
        listener: ListenerArgs,
        bmc_behaviour: BmcBehaviorArgs,
        machine: MachineArgs,
    },
}

impl Args {
    fn app_type(self) -> AppType {
        if self.targz.ip_router.is_some() || self.targz.targz.is_some() {
            AppType::TarGzRouter {
                targz: self.targz,
                listener: self.listener,
            }
        } else {
            let domain_defined = self.libvirt.libvirt_domain.is_some();
            match (self.state_backend, domain_defined) {
                (Some(StateBackend::Libvirt), _) | (None, true) => AppType::LibvirtApp {
                    listener: self.listener,
                    machine: self.machine,
                    bmc_behaviour: self.bmc_behaviour,
                    libvirt: self.libvirt,
                },
                (Some(StateBackend::Internal), _) | (None, false) => AppType::JustMockApp {
                    listener: self.listener,
                    machine: self.machine,
                    bmc_behaviour: self.bmc_behaviour,
                },
            }
        }
    }

    pub(super) fn validate(&self) -> eyre::Result<()> {
        self.machine.validate()?;

        if self.state_backend == Some(StateBackend::Internal)
            && self.libvirt.libvirt_domain.is_some()
        {
            eyre::bail!("--libvirt-domain cannot be used with --state-backend=internal");
        }

        Ok(())
    }
}

impl MachineArgs {
    pub(super) fn validate(&self) -> eyre::Result<()> {
        if let Some(fixed_dpu_count) = self.hardware_profile.fixed_number_of_dpu()
            && let Some(dpu_count) = self.dpu_count
            && dpu_count != fixed_dpu_count
        {
            eyre::bail!(
                "invalid DPU count for profile {:?}. specified: {dpu_count}. expect: {fixed_dpu_count}",
                self.hardware_profile,
            );
        }

        if self.dpu_index.is_some() && self.machine_role != MachineRole::Dpu {
            eyre::bail!("DPU index must be specified only for DPU machine role");
        }

        if let Some(dpu_index) = self.dpu_index
            && dpu_index >= self.dpu_count()
        {
            eyre::bail!(
                "invalid DPU index {dpu_index}: expected a value below {}",
                self.dpu_count()
            );
        }

        if !self.dpu_firmware.is_empty() && self.dpu_count() == 0 {
            eyre::bail!("DPU firmware options require at least one DPU");
        }

        if self.machine_role == MachineRole::Dpu && self.dpu_index().is_none() {
            eyre::bail!("DPU index must be less than the configured DPU count");
        }

        Ok(())
    }

    pub(super) fn dpu_count(&self) -> u8 {
        self.hardware_profile
            .fixed_number_of_dpu()
            .or(self.dpu_count)
            .unwrap_or_else(|| match self.machine_role {
                MachineRole::Host => 0,
                MachineRole::Dpu => self
                    .dpu_index
                    .map(|index| index.saturating_add(1))
                    .unwrap_or(1),
            })
    }

    pub(super) fn dpu_index(&self) -> Option<u8> {
        let index = self.dpu_index.unwrap_or(0);
        (index < self.dpu_count()).then_some(index)
    }
}

pub(super) fn parse_args() -> eyre::Result<AppType> {
    let args = Args::parse();
    args.validate()?;
    Ok(args.app_type())
}
