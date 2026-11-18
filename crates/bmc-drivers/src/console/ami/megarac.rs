/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Console, ConsoleSpec, ConsoleStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::console::support::{AttrExpectation, RedfishConsoleExt as _, attr, attr_status};
use crate::resources::RedfishResourcesExt as _;

/// AMI MegaRAC BIOS console attributes; the console transport is not identified.
pub(crate) struct MegaRacConsole;

pub(in crate::console) const ATTRS: &[AttrExpectation] = &[
    attr("TER001", &["Enabled"], &["Disabled"]),
    attr("TER010", &["Enabled"], &["Disabled"]),
    attr("TER06B", &["COM1"], &[]),
    attr("TER0021", &["115200"], &[]),
    attr("TER0020", &["115200"], &[]),
    attr("TER012", &["VT100Plus"], &[]),
    attr("TER011", &["VT-UTF8"], &[]),
    attr("TER05D", &["None"], &[]),
];

#[async_trait]
impl<B: Bmc> Console<B> for MegaRacConsole {
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
        Ok(ConsoleSpec::None {
            reason: "AMI console transport is not identified".to_string(),
        })
    }
}
