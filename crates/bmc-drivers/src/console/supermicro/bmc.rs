/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Console, ConsoleSpec, ConsoleState, ConsoleStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::serial_interface::{
    FlowControl, Parity, PinOut, SerialInterface, SignalType,
};

use crate::console::support::ipmi_sol_spec;

/// Supermicro console driver.
///
/// The BMC exposes its serial console and serial interface read-only, so
/// setup changes nothing; Serial over LAN works when the console is enabled
/// and the serial interface holds the Supermicro defaults.
pub(crate) struct SupermicroBmcConsole;

/// Whether `interface` holds the Supermicro defaults Serial over LAN needs.
fn holds_supermicro_defaults(interface: &SerialInterface) -> bool {
    interface.interface_enabled == Some(Some(true))
        && interface.signal_type == Some(SignalType::Rs232)
        && interface.bit_rate.as_deref() == Some("115200")
        && interface.parity == Some(Parity::None)
        && interface.data_bits.as_deref() == Some("8")
        && interface.stop_bits.as_deref() == Some("1")
        && interface.flow_control == Some(FlowControl::None)
        && interface.connector_type.as_deref() == Some("RJ45")
        && interface.pin_out == Some(Some(PinOut::Cyclades))
}

#[async_trait]
impl<B: Bmc> Console<B> for SupermicroBmcConsole {
    async fn setup(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
    }

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        let interfaces = cx
            .manager()
            .await?
            .serial_interfaces()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        let interface_defaults = interfaces
            .last()
            .map(|interface| holds_supermicro_defaults(&interface.raw()))
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: "the manager lists no serial interfaces".to_string(),
            })?;

        let system = cx.system().await?;
        let raw = system.raw();
        let serial = raw
            .serial_console
            .as_ref()
            .ok_or(PlatformError::Unsupported)?;
        let ssh_enabled = serial
            .ssh
            .as_ref()
            .is_some_and(|ssh| ssh.service_enabled == Some(true));
        let enabled =
            ssh_enabled && serial.max_concurrent_sessions != Some(0) && interface_defaults;
        Ok(ConsoleStatus {
            state: if enabled {
                ConsoleState::Enabled
            } else {
                ConsoleState::Disabled
            },
            message: format!(
                "ssh_service_enabled={ssh_enabled}, max_concurrent_sessions={}, serial_interface_defaults={interface_defaults}",
                serial
                    .max_concurrent_sessions
                    .map_or_else(|| "unreported".to_string(), |sessions| sessions.to_string())
            ),
        })
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        ipmi_sol_spec()
    }
}
