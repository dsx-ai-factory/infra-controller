/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Console, ConsoleSpec, ConsoleState, ConsoleStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;

use crate::console::support::DPU_SSH_PORT;

/// NVIDIA BlueField: the DPU console is an SSH service on its own port, so
/// there is nothing to set up.
pub(crate) struct BlueFieldConsole;

#[async_trait]
impl<B: Bmc> Console<B> for BlueFieldConsole {
    async fn setup(&self, _cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        Ok(DriverOutcome::complete())
    }

    async fn status(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleStatus, PlatformError> {
        Ok(ConsoleStatus {
            state: ConsoleState::Enabled,
            message: "DPU console is directly available over SSH".to_string(),
        })
    }

    async fn spec(&self, _cx: &OpCx<'_, B>) -> Result<ConsoleSpec, PlatformError> {
        Ok(ConsoleSpec::SshDirect { port: DPU_SSH_PORT })
    }
}
