/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Console, ConsoleFallback, ConsoleSpec, ConsoleStatus, DriverOutcome, EscapeSeq, OpCx,
    PlatformError,
};
use nv_redfish::core::Bmc;

use crate::console::support::{
    AttrExpectation, RedfishConsoleExt as _, SSH_PORT, attr, attr_status, spec_error,
};
use crate::resources::RedfishResourcesExt as _;

/// Lenovo XClarity Controller console, reached through the `console 1` shell command.
pub(crate) struct XccConsole;

const ATTRS: &[AttrExpectation] = &[
    attr("DevicesandIOPorts_COMPort1", &["Enabled"], &[]),
    attr(
        "DevicesandIOPorts_ConsoleRedirection",
        &["Enabled"],
        &["Auto"],
    ),
    attr(
        "DevicesandIOPorts_SerialPortSharing",
        &["Enabled"],
        &["Disabled"],
    ),
    attr(
        "DevicesandIOPorts_SerialPortAccessMode",
        &["Shared"],
        &["Disabled"],
    ),
    // Only older systems have these.
    attr(
        "DevicesandIOPorts_SPRedirection",
        &["Enabled"],
        &["Disabled"],
    )
    .optional(),
    attr(
        "DevicesandIOPorts_COMPortActiveAfterBoot",
        &["Enabled"],
        &["Disabled"],
    )
    .optional(),
];

fn xcc_spec() -> Result<ConsoleSpec, PlatformError> {
    // A stale session makes `console 1` fail; the fallback kills and restarts it.
    let fallback = ConsoleFallback::new(
        b"The command line contains extraneous arguments".to_vec(),
        vec![b"console kill".to_vec(), b"console start".to_vec()],
    )
    .map_err(spec_error)?;
    ConsoleSpec::ssh_shell(
        SSH_PORT,
        b"console kill 1\nconsole 1".to_vec(),
        Some(fallback),
        b"\nsystem>".to_vec(),
        EscapeSeq::pair(0x1b, vec![0x28]).map_err(spec_error)?,
    )
    .map_err(spec_error)
}

#[async_trait]
impl<B: Bmc> Console<B> for XccConsole {
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
        xcc_spec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_constructs_the_shell_with_its_fallback() {
        let spec = xcc_spec().expect("Lenovo spec");
        let shell = spec.as_ssh_shell().expect("SSH shell");
        assert_eq!(shell.activate.as_slice(), b"console kill 1\nconsole 1");
        assert_eq!(
            shell
                .fallback
                .as_ref()
                .expect("fallback")
                .commands()
                .collect::<Vec<_>>(),
            vec![b"console kill".as_slice(), b"console start".as_slice()]
        );
    }
}
