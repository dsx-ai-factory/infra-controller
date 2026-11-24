/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Console, ConsoleSpec, ConsoleStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::console::support::{
    AttrExpectation, RedfishConsoleExt as _, SSH_PORT, attr, attr_status,
};
use crate::resources::RedfishResourcesExt as _;

/// Lenovo GB300 AMI console; this BIOS prefixes enum values with the attribute
/// name, and SSH login lands on the serial console directly.
pub(crate) struct Gb300Console;

const ATTRS: &[AttrExpectation] = &[
    attr("TER001", &["Enabled"], &["Disabled"]),
    attr("TER010", &["Enabled"], &["Disabled"]),
    attr("TER06B", &["TER06BCOM0"], &[]),
    attr("TER0021", &["TER0021115200"], &[]),
    attr("TER0020", &["TER0020115200"], &[]),
    attr("TER012", &["TER012VT100Plus"], &[]),
    attr("TER011", &["TER011VTUTF8"], &[]),
    attr("TER05D", &["TER05DNone"], &[]),
];

#[async_trait]
impl<B: Bmc> Console<B> for Gb300Console {
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
        Ok(ConsoleSpec::SshDirect { port: SSH_PORT })
    }
}
