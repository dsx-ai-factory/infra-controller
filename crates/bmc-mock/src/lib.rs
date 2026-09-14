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

//! Mock BMCs with hardware-specific Redfish inventories and stateful management operations.
//!
//! # Architecture and extension rules
//!
//! Select the hardware representation and its capabilities when constructing the BMC:
//!
//! ```text
//! HardwareType / MachineInfo
//!     -> hw::<platform> profile
//!     -> redfish resource/service configs
//!     -> resource/service state
//!     -> Redfish request handlers
//! ```
//!
//! `machine_info` selects profiles and supplies machine identity and caller-provided settings.
//! `hw` owns the platform's inventory, resource IDs, initial values, and choice of supported
//! protocol behavior. `redfish` owns resource schemas, builders, HTTP handling, and the state
//! transitions of the configured features. `mock_machine_router` assembles configs, state,
//! callbacks, and routes before serving requests. See `hw/mod.rs` and `redfish/mod.rs` for
//! the corresponding extension templates.
//!
//! Hardware code uses Redfish configs and builders. Keep new Redfish code independent of
//! hardware profile types: handlers must not inspect `HardwareType`, recognize platforms
//! from model strings or resource IDs, or construct hardware profiles during a request.
//! A new platform should reuse existing protocol implementations through configuration.
//!
//! State may contain platform-specific or OEM features. Select and fully initialize those
//! features before serving requests, using an explicit config, capability, or OEM state
//! variant. During requests, dispatch on that configured behavior. Runtime operations may
//! change values and perform configured transitions; they must not infer missing platform
//! capabilities or lazily install them. Simulator integration may supply runtime values
//! through explicit APIs with documented support and lifecycle semantics.
//!
//! Distinguish feature support from its mutable value. Use an optional feature config/state
//! for optional support, and make advertised links and endpoint availability follow the same
//! source of truth. A disabled feature value is different from an unsupported feature.
//! Constructors must return ready-to-use state without a separate platform-specific fix-up.
//!
//! Keep external machine effects behind callbacks and explicit events. Protocol handlers
//! should not acquire dependencies on machine-a-tron or a particular simulation backend.
//!
//! These rules govern additions and changes even where existing code has exceptions.
//! Do not copy an exception as an extension template; keep unrelated architectural cleanup
//! in a separate refactor.

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;
mod ipmi;
pub mod ipmi_sim;
pub mod libvirt;
pub mod simulated;

mod auth_router;
pub mod availability;
mod bmc_state;
mod combined_server;
mod event_controls;
mod http;
mod hw;
pub mod infiniband;
mod json;
pub mod mac_address_pool;
mod machine_info;
mod middleware_router;
mod mock_machine_router;
mod rack_info;
mod redfish;
mod sse;
mod tar_router;
pub mod test_support;
pub mod tls;

pub use bmc_state::{BmcEvent, BmcState};
pub use carbide_axum_utils::authority_router::authority_router as combined_router;
pub use carbide_axum_utils::injection;
pub use combined_server::{CombinedServer, ListenerOrAddress};
pub use http::redfish_error_envelope;
pub use hw::rack::{RackElevation, RackPlacement, RackUnit};
pub use machine_info::{
    DpuFirmwareVersions, DpuMachineInfo, DpuSettings, HostFirmwareVersions, HostMachineInfo,
    MachineInfo,
};
pub use mock_machine_router::{
    BmcCommand, EventServiceOverride, MachineRouterOptions, SetSystemPowerError,
    SetSystemPowerResult, machine_router, machine_router_with_injection_store,
};
pub use rack_info::RackInfo;
pub use redfish::event_service::{
    EventServiceConfig, EventServiceError, EventServiceLimits, EventServiceState, EventServiceStats,
};
pub use redfish::virtual_media::DeviceConfig as VirtualMediaDeviceConfig;
pub use sse::StreamStep;

pub const DUMMY_FACTORY_USERNAME: &str = "root";
pub const DUMMY_FACTORY_PASSWORD: &str = "factory_password";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum RackType {
    #[serde(rename = "wiwynn_gb200_nvl72")]
    WiwynnGb200Nvl72,
    #[serde(rename = "lenovo_gb300_nvl72")]
    LenovoGb300Nvl72,
}

impl fmt::Display for RackType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WiwynnGb200Nvl72 => formatter.write_str("WIWYNN GB200 NVL72"),
            Self::LenovoGb300Nvl72 => formatter.write_str("Lenovo GB300 NVL72"),
        }
    }
}

#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum HardwareType {
    #[serde(rename = "dell_poweredge_r750")]
    #[default]
    DellPowerEdgeR750,
    #[serde(rename = "dell_poweredge_r760_bf4")]
    DellPowerEdgeR760Bf4,
    #[serde(rename = "wiwynn_gb200_nvl")]
    WiwynnGB200Nvl,
    #[serde(rename = "lenovo_gb300_nvl")]
    LenovoGB300Nvl,
    #[serde(rename = "nvidia_dgx_gb300")]
    NvidiaDgxGb300,
    #[serde(rename = "supermicro_gb300_nvl")]
    SupermicroGb300Nvl,
    #[serde(rename = "nvidia_dgx_vr")]
    NvidiaDgxVr,
    #[serde(rename = "liteon_power_shelf")]
    LiteOnPowerShelf,
    #[serde(rename = "delta_power_shelf")]
    DeltaPowerShelf,
    #[serde(rename = "nvidia_switch_nd5200_ld")]
    NvidiaSwitchNd5200Ld,
    #[serde(rename = "nvidia_switch_n5700_ld")]
    NvidiaSwitchN5700Ld,
    #[serde(rename = "nvidia_dgx_h100")]
    NvidiaDgxH100,
    #[serde(rename = "generic_ami")]
    GenericAmi,
    #[serde(rename = "hpe_proliant_dl380a_gen11")]
    HpeProliantDl380aGen11,
    /// A non-GB300 Supermicro-vendor server (no NVIDIA GB300 GPU chassis). Reuses the
    /// generic-server representation but reports a Supermicro vendor; used to assert that
    /// the `is_gb300()` gate keeps such a box classified as generic `Supermicro`.
    #[serde(rename = "generic_supermicro")]
    GenericSupermicro,
}

impl fmt::Display for HardwareType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::DellPowerEdgeR750 => "Dell PowerEdge R750".fmt(f),
            Self::DellPowerEdgeR760Bf4 => "Dell PowerEdge R760 Bluefield-4".fmt(f),
            Self::WiwynnGB200Nvl => "WIWYNN GB200 NVL".fmt(f),
            Self::LenovoGB300Nvl => "Lenovo GB300 NVL".fmt(f),
            Self::NvidiaDgxGb300 => "NVIDIA DGX GB300 NVL".fmt(f),
            Self::SupermicroGb300Nvl => "Supermicro GB300 NVL".fmt(f),
            Self::NvidiaDgxVr => "NVIDIA DGX VR NVL".fmt(f),
            Self::LiteOnPowerShelf => "Lite-On Power Shelf".fmt(f),
            Self::DeltaPowerShelf => "Delta Power Shelf".fmt(f),
            Self::NvidiaSwitchNd5200Ld => "NVIDIA Switch ND5200_LD".fmt(f),
            Self::NvidiaSwitchN5700Ld => "NVIDIA Switch N5700_LD".fmt(f),
            Self::NvidiaDgxH100 => "NVIDIA DGX H100".fmt(f),
            Self::GenericAmi => "Generic AMI Server".fmt(f),
            Self::HpeProliantDl380aGen11 => "HPE ProLiant DL380a Gen11".fmt(f),
            Self::GenericSupermicro => "Generic Supermicro Server".fmt(f),
        }
    }
}

impl HardwareType {
    /// Key in `DesiredFirmwareVersionEntry.component_versions` for the host BMC
    /// version.  Most platforms use `"bmc"`; DGX H100 uses `"combinedbmcuefi"`
    /// because the API models its BMC as a `CombinedBmcUefi` component type.
    pub fn host_bmc_version_key(&self) -> &'static str {
        match self {
            Self::NvidiaDgxH100 => "combinedbmcuefi",
            _ => "bmc",
        }
    }

    // This function returns how many DPUs must be attached to the
    // platform. If None than platform can support variable number of
    // DPUs.
    pub fn fixed_number_of_dpu(&self) -> Option<u8> {
        match self {
            Self::DellPowerEdgeR750 => None,
            Self::DellPowerEdgeR760Bf4 => Some(1),
            Self::WiwynnGB200Nvl => Some(2),
            Self::LenovoGB300Nvl => Some(1),
            Self::NvidiaDgxGb300 => Some(1),
            Self::SupermicroGb300Nvl => Some(1),
            Self::NvidiaDgxVr => Some(1),
            Self::LiteOnPowerShelf => Some(0),
            Self::DeltaPowerShelf => Some(0),
            Self::NvidiaSwitchNd5200Ld => Some(0),
            Self::NvidiaSwitchN5700Ld => Some(0),
            Self::NvidiaDgxH100 => Some(1),
            Self::GenericAmi => None,
            Self::HpeProliantDl380aGen11 => None,
            Self::GenericSupermicro => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Default)]
pub enum MockPowerState {
    #[default]
    On,
    Off,
    PowerCycling {
        since: Instant,
    },
}

impl fmt::Display for MockPowerState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::On => "On".fmt(f),
            Self::Off => "Off".fmt(f),
            Self::PowerCycling { since } => write!(f, "PowerCycling {:?}", since.elapsed()),
        }
    }
}

// Simulate a 5-second power cycle
pub const POWER_CYCLE_DELAY: Duration = Duration::from_secs(5);

pub trait Callbacks: std::fmt::Debug + Send + Sync {
    fn get_power_state(&self) -> MockPowerState;
    fn send_power_command(&self, reset_type: SystemPowerControl)
    -> Result<(), SetSystemPowerError>;
    fn set_power_state(&self, reset_type: SystemPowerControl) -> Result<(), SetSystemPowerError> {
        type C = SystemPowerControl;
        match (reset_type, self.get_power_state()) {
            (
                C::GracefulShutdown | C::ForceOff | C::GracefulRestart | C::ForceRestart,
                MockPowerState::Off,
            ) => Err(SetSystemPowerError::BadRequest(
                "bmc-mock: cannot power off machine, it is already off".to_string(),
            )),
            (C::On | C::ForceOn, MockPowerState::On) => Err(SetSystemPowerError::BadRequest(
                "bmc-mock: cannot power on machine, it is already on".to_string(),
            )),
            (_, MockPowerState::PowerCycling { since }) if since.elapsed() < POWER_CYCLE_DELAY => {
                Err(SetSystemPowerError::BadRequest(format!(
                    "bmc-mock: cannot reset machine, it is in the middle of power cycling since {:?} ago",
                    since.elapsed()
                )))
            }
            _ => Ok(()),
        }?;
        self.send_power_command(reset_type)
    }

    fn state_refresh_indication(&self);
}

pub trait HostnameQuerying: std::fmt::Debug + Send + Sync {
    fn get_hostname(&'_ self) -> Cow<'_, str>;
}

// https://www.dmtf.org/sites/default/files/standards/documents/DSP2046_2023.3.html
// 6.5.5.1 ResetType
#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub enum SystemPowerControl {
    /// Power on a machine
    On,
    /// Graceful host shutdown
    GracefulShutdown,
    /// Forcefully powers a machine off
    ForceOff,
    /// Graceful restart. Asks the OS to restart via ACPI
    /// - Might restart DPUs if no OS is running
    /// - Will not apply pending BIOS/UEFI setting changes
    GracefulRestart,
    /// Force restart. This is equivalent to pressing the reset button on the front panel.
    /// - Will not restart DPUs
    /// - Will apply pending BIOS/UEFI setting changes
    ForceRestart,

    //
    // libredfish doesn't support these yet, and not all vendors provide them
    //

    // Cut then restore the power
    PowerCycle,

    // Forcefully power a machine on (?)
    ForceOn,

    // Like it says, pretend the button got pressed
    PushPowerButton,

    // Non-maskable interrupt then power off
    Nmi,

    // Write state to disk and power off
    Suspend,

    // VM / Hypervisor
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootOptionKind {
    Disk,
    Network,
}

#[cfg(test)]
mod hardware_type_tests {
    use super::HardwareType;

    #[test]
    fn nvidia_switch_n5700_ld_serde_and_dpu_count() {
        let hardware_type = HardwareType::NvidiaSwitchN5700Ld;
        let serialized = serde_json::to_string(&hardware_type).expect("hardware type serializes");

        assert_eq!(serialized, r#""nvidia_switch_n5700_ld""#);
        assert_eq!(
            serde_json::from_str::<HardwareType>(&serialized).expect("hardware type deserializes"),
            hardware_type
        );
        assert_eq!(hardware_type.fixed_number_of_dpu(), Some(0));
    }
}
