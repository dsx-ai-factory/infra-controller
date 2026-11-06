/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Console, ConsoleSpec, ConsoleStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::console::support::{
    AttrExpectation, RedfishConsoleExt as _, attr, attr_status, ipmi_sol_spec, write_only,
};
use crate::resources::RedfishResourcesExt as _;

/// NVIDIA Viking console; the console is an IPMI SOL session. Setup writes
/// only the attributes the BIOS reports.
pub(crate) struct VikingConsole;

const ATTRS: &[AttrExpectation] = &[
    attr("AcpiSpcrConsoleRedirectionEnable", &["true"], &["false"]).optional(),
    attr("ConsoleRedirectionEnable0", &["true"], &["false"]).optional(),
    attr("AcpiSpcrPort", &["COM0"], &[]).optional(),
    attr("AcpiSpcrFlowControl", &["None"], &[]).optional(),
    attr("AcpiSpcrBaudRate", &["115200"], &[]).optional(),
    attr("BaudRate0", &["115200"], &[]).optional(),
    write_only("AcpiSpcrTerminalType", &["VT-UTF8"]).optional(),
    write_only("TerminalType0", &["ANSI"]).optional(),
];

#[async_trait]
impl<B: Bmc> Console<B> for VikingConsole {
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
        ipmi_sol_spec()
    }
}
