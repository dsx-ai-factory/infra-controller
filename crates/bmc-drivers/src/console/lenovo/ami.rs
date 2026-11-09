/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Console, ConsoleSpec, ConsoleStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::console::ami::megarac::ATTRS as MEGARAC_ATTRS;
use crate::console::support::{RedfishConsoleExt as _, SSH_PORT, attr_status};
use crate::resources::RedfishResourcesExt as _;

/// Lenovo AMI: the AMI BIOS serial redirection attributes, with SSH login
/// landing on the serial console directly.
pub(crate) struct LenovoAmiConsole;

#[async_trait]
impl<B: Bmc> Console<B> for LenovoAmiConsole {
    async fn setup(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        cx.setup_console_bios_attributes(MEGARAC_ATTRS).await
    }

    async fn status(&self, cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        Ok(attr_status(
            &cx.current_bios_settings().await?.attributes,
            MEGARAC_ATTRS,
        ))
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        Ok(ConsoleSpec::SshDirect { port: SSH_PORT })
    }
}
