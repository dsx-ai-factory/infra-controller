/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The platform settings a host needs before it boots for provisioning:
//! secure boot off, the BMC's manager settings, the serial console, and the
//! BIOS settings. Each platform's drivers decide what those mean on it.

use bmc_platform::{
    BiosSettings, BootInterfaceSelector, ConsoleState, DriverOutcome, ManagerSettings,
    OperationReference, PlatformError, SecureBootStatus, VendorJobId,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::nv_redfish::RedfishBmc;
use nv_redfish::schema::secure_boot::SecureBootUpdate;

use crate::config::MachineStateHandlerSiteConfig;

mod legacy;

pub(crate) use legacy::PlatformBmc;

/// The iDRAC configuration job known by `job_id`.
pub(crate) fn idrac_job(job_id: VendorJobId) -> OperationReference {
    OperationReference::VendorJob {
        uri: format!("/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{job_id}").into(),
        job_id,
        retry_after_seconds: None,
    }
}

/// The platform settings for one host.
struct PlatformConfig {
    bios_profile: BiosSettings,
    manager_profile: ManagerSettings,
    boot_interface: Option<BootInterfaceSelector>,
}

impl PlatformConfig {
    /// The settings for the host behind `bmc`, which boots from
    /// `boot_interface` when it has one.
    fn new(
        site_config: &MachineStateHandlerSiteConfig,
        bmc: &ConnectedBmc<RedfishBmc>,
        boot_interface: Option<&BootInterfaceTarget>,
    ) -> Self {
        let (bios_profile, manager_profile) =
            legacy::profiles(site_config, bmc.endpoint().identity());
        Self {
            bios_profile,
            manager_profile,
            boot_interface: boot_interface.map(BootInterfaceSelector::from),
        }
    }
}

/// Writes every platform setting the platform supports, in order, and
/// returns the BMC work they started. Settings take effect on the next host
/// restart.
pub(crate) async fn apply(
    bmc: &PlatformBmc<'_>,
    site_config: &MachineStateHandlerSiteConfig,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<DriverOutcome, PlatformError> {
    let bmc = match bmc {
        PlatformBmc::Libredfish {
            client,
            expected_dpu_count,
        } => return legacy::apply(*client, site_config, boot_interface, *expected_dpu_count).await,
        PlatformBmc::Drivers(bmc) => bmc,
    };
    let config = PlatformConfig::new(site_config, bmc, boot_interface);
    let disable_secure_boot = async {
        if bmc.secure_boot()?.status().await? != SecureBootStatus::Enabled {
            return Ok(DriverOutcome::complete());
        }
        bmc.secure_boot()?
            .set(
                &SecureBootUpdate::builder()
                    .with_secure_boot_enable(false)
                    .build(),
            )
            .await
    };
    let apply_settings = async {
        bmc.bmc_control()?
            .apply_settings(&config.manager_profile)
            .await
    };
    let setup_console = async { bmc.console()?.setup().await };
    let apply_bios = async {
        bmc.bios()?
            .apply(&config.bios_profile, config.boot_interface.as_ref())
            .await
    };
    Ok(
        unless_unsupported(disable_secure_boot.await, DriverOutcome::complete())?
            .merge(unless_unsupported(
                apply_settings.await,
                DriverOutcome::complete(),
            )?)
            .merge(unless_unsupported(
                setup_console.await,
                DriverOutcome::complete(),
            )?)
            .merge(unless_unsupported(
                apply_bios.await,
                DriverOutcome::complete(),
            )?),
    )
}

/// Whether every platform setting the platform reports is in effect.
pub(crate) async fn in_effect(
    bmc: &PlatformBmc<'_>,
    site_config: &MachineStateHandlerSiteConfig,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<bool, PlatformError> {
    let bmc = match bmc {
        PlatformBmc::Libredfish { client, .. } => {
            return legacy::in_effect(*client, boot_interface).await;
        }
        PlatformBmc::Drivers(bmc) => bmc,
    };
    let config = PlatformConfig::new(site_config, bmc, boot_interface);
    let secure_boot_disabled =
        async { Ok(bmc.secure_boot()?.status().await? == SecureBootStatus::Disabled) };
    let settings_applied = async { Ok(bmc.bmc_control()?.settings_status().await?.is_applied) };
    let console_enabled =
        async { Ok(bmc.console()?.status().await?.state == ConsoleState::Enabled) };
    let bios_applied = async {
        Ok(bmc
            .bios()?
            .status(&config.bios_profile, config.boot_interface.as_ref())
            .await?
            .is_applied)
    };
    Ok(unless_unsupported(secure_boot_disabled.await, true)?
        && unless_unsupported(settings_applied.await, true)?
        && unless_unsupported(console_enabled.await, true)?
        && unless_unsupported(bios_applied.await, true)?)
}

/// A setting the platform does not support has nothing to write and nothing
/// to check, so it yields `fallback`.
fn unless_unsupported<T>(
    result: Result<T, PlatformError>,
    fallback: T,
) -> Result<T, PlatformError> {
    match result {
        Err(PlatformError::Unsupported) => Ok(fallback),
        result => result,
    }
}
