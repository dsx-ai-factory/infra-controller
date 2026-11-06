/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{Console, ConsoleSpec, ConsoleStatus, DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::Bmc;

use crate::console::support::ipmi_sol_spec;

/// NVIDIA OpenBMC tray console: an IPMI SOL session. These BMCs expose no
/// console settings to set up or check.
pub(crate) struct OpenBmcConsole;

#[async_trait]
impl<B: Bmc> Console<B> for OpenBmcConsole {
    async fn setup(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    async fn status(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        ipmi_sol_spec()
    }
}
