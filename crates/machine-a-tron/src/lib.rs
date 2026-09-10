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
mod actor;
pub mod api_client;
pub mod api_throttler;
mod bmc_mock_wrapper;
mod config;
mod control_router;
mod device_handle;
mod device_simulator;
mod dhcp_retry_fsm;
mod dhcp_retry_runtime;
mod dhcp_wrapper;
mod dhcp_wrapper_udp;
mod discovery_info;
mod dpu_machine;
mod host_machine;
pub mod lifecycle_timings;
mod machine_a_tron;
mod machine_fsm;
mod machine_state_machine;
mod machine_utils;
mod mock_ssh_server;
mod power_shelf_fsm;
mod power_shelf_simulator;
mod rack;
mod simulator_registry;
mod status;
mod switch_fsm;
mod switch_simulator;

use std::time::{Duration, Instant};

pub use bmc_mock_wrapper::{BmcMockRegistry, RegisteredBmc};
pub use config::{
    DhcpType, LenovoGb300RackConfig, LogFormat, MachineATronArgs, MachineATronConfig,
    MachineATronContext, MachineConfig, PersistedDevice, PersistedDpuMachine, RackConfig,
    RackModelConfig, WiwynnGb200RackConfig,
};
pub use control_router::{ControlState, append as append_control_routes};
pub use device_handle::DeviceHandle;
pub use device_simulator::{
    DeviceSimulator, MachineSimulator, PowerShelfSimulator, SimulatorLifecycle, SwitchSimulator,
};
pub use dhcp_wrapper::{DhcpClient, UdpDhcpService};
pub use dpu_machine::DpuMachineHandle;
pub use machine_a_tron::MachineATron;
pub use mock_ssh_server::{
    Credentials as MockSshCredentials, MockSshServerHandle, PromptBehavior,
    spawn as spawn_mock_ssh_server,
};
pub use rack::{RackMemberStatus, RackStatus, RacksStatusResponse};
pub use simulator_registry::SimulatorRegistry;
pub use status::{
    DeviceKind, DeviceStatus, DeviceStatusConfig, DevicesStatusResponse, InfinibandPortStatus,
};
pub use ufm_mock::{Guid, InfinibandPortState};

/// Add a Duration to an Instant, defaulting to a time in the far future if there is an overflow.
/// This allows using Duration::MAX and being able to add it to Instant::now(), which overflows by
/// default.
pub fn saturating_add_duration_to_instant(instant: Instant, duration: Duration) -> Instant {
    instant
        .checked_add(duration)
        // Roughly 30 years from now
        .unwrap_or(Instant::now() + Duration::from_secs(30 * 365 * 24 * 3600))
}
