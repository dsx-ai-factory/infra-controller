/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{BootProgress, BootProgressTypes, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};

use crate::power::standard::StandardPower;

/// BlueField DPUs report a running Arm OS through an OEM boot state.
pub(crate) struct BlueFieldPower;

/// The OEM boot state of a BlueField whose Arm OS is running.
const OEM_OS_IS_RUNNING: &str = "OsIsRunning";

#[async_trait]
impl<B: Bmc> Power<B> for BlueFieldPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn boot_progress(&self, cx: &OpCx<'_, B>) -> Result<Option<BootProgress>, PlatformError> {
        boot_progress(&StandardPower, cx).await
    }
}

/// The boot progress `standard` reads, with the OEM running state reported
/// as `OsRunning`.
pub(super) async fn boot_progress<B: Bmc>(
    standard: &dyn Power<B>,
    cx: &OpCx<'_, B>,
) -> Result<Option<BootProgress>, PlatformError> {
    Ok(standard.boot_progress(cx).await?.map(os_running))
}

fn os_running(mut progress: BootProgress) -> BootProgress {
    if progress.oem_last_state.as_deref() == Some(OEM_OS_IS_RUNNING) {
        progress.last_state = Some(BootProgressTypes::OsRunning);
    }
    progress
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn the_oem_running_state_reports_as_os_running() {
        let progress = |last_state, oem_last_state: Option<&str>| BootProgress {
            last_state,
            oem_last_state: oem_last_state.map(str::to_string),
        };
        value_scenarios!(run = |boot_progress: BootProgress| os_running(boot_progress).last_state;
            "the Arm OS runs" {
                progress(Some(BootProgressTypes::Oem), Some("OsIsRunning"))
                    => Some(BootProgressTypes::OsRunning),
                progress(Some(BootProgressTypes::OsRunning), None)
                    => Some(BootProgressTypes::OsRunning),
            }
            "still booting" {
                progress(Some(BootProgressTypes::Oem), Some("UEFI")) => Some(BootProgressTypes::Oem),
                progress(None, None) => None,
            }
        );
    }
}
