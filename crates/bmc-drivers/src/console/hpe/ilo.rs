/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Console, ConsoleSpec, ConsoleStatus, DriverOutcome, EscapeSeq, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;

use crate::console::support::{
    AttrExpectation, RedfishConsoleExt as _, SSH_PORT, attr, attr_status, spec_error,
};
use crate::resources::RedfishResourcesExt as _;

/// HPE iLO console; the virtual serial port is reached with `vsp`.
pub(crate) struct IloConsole;

/// `UefiSerialDebugLevel` set to `Disabled` silences UEFI debug output; it
/// does not turn the console off.
const ATTRS: &[AttrExpectation] = &[
    attr("EmbeddedSerialPort", &["Com2Irq3"], &["Disabled"]),
    attr("EmsConsole", &["Virtual"], &["Disabled"]),
    attr("SerialConsoleBaudRate", &["BaudRate115200"], &[]),
    attr("SerialConsoleEmulation", &["Vt100Plus"], &[]),
    attr("SerialConsolePort", &["Virtual"], &["Disabled"]),
    attr("UefiSerialDebugLevel", &["ErrorsOnly"], &[]),
    attr("VirtualSerialPort", &["Com1Irq4"], &["Disabled"]),
];

fn hpe_spec() -> Result<ConsoleSpec, PlatformError> {
    ConsoleSpec::ssh_shell(
        SSH_PORT,
        b"vsp".to_vec(),
        None,
        b"\n</>hpiLO->".to_vec(),
        EscapeSeq::pair(0x1b, vec![0x28]).map_err(spec_error)?,
    )
    .map_err(spec_error)
}

#[async_trait]
impl<B: Bmc> Console<B> for IloConsole {
    async fn setup(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.setup_console_bios_attributes(ATTRS).await
    }

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        Ok(attr_status(
            &cx.current_bios_settings().await?.attributes,
            ATTRS,
        ))
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        hpe_spec()
    }
}

#[cfg(test)]
mod tests {
    use bmc_platform::ConsoleState;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::Fixture;

    const SYSTEM: &str = "/redfish/v1/Systems/1";
    const BIOS: &str = "/redfish/v1/Systems/1/Bios";

    /// The console attributes of a DL380a Gen11 with the console set up.
    fn configured() -> Value {
        json!({
            "EmbeddedSerialPort": "Com2Irq3",
            "EmsConsole": "Virtual",
            "SerialConsoleBaudRate": "BaudRate115200",
            "SerialConsoleEmulation": "Vt100Plus",
            "SerialConsolePort": "Virtual",
            "UefiSerialDebugLevel": "ErrorsOnly",
            "VirtualSerialPort": "Com1Irq4",
        })
    }

    #[tokio::test]
    async fn status_reads_back_the_bios_settings_setup_writes() {
        let mut ems_off = configured();
        ems_off["EmsConsole"] = json!("Disabled");
        for (attributes, expected) in [
            (configured(), ConsoleState::Enabled),
            (ems_off, ConsoleState::Partial),
        ] {
            let bmc = Fixture::new("HPE", "ProLiant", "1", "1")
                .document(
                    SYSTEM,
                    json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System", "Bios": {"@odata.id": BIOS}}),
                )
                .document(
                    BIOS,
                    json!({"@odata.id": BIOS, "Id": "Bios", "Name": "BIOS", "Attributes": attributes}),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            assert_eq!(
                IloConsole.status(&cx).await.map(|status| status.state),
                Ok(expected)
            );
        }
    }

    #[test]
    fn spec_constructs_the_vsp_shell() {
        assert_eq!(
            hpe_spec()
                .expect("HPE spec")
                .as_ssh_shell()
                .expect("SSH shell")
                .activate
                .as_slice(),
            b"vsp"
        );
    }
}
