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

//! The platform settings a host needs before it boots for provisioning: the
//! BMC's manager settings, the serial console, the BIOS settings, secure
//! boot off, and its boot interface first in the boot order, and the ones its
//! DPUs need. Each platform's drivers decide what those mean on it.

use bmc_platform::{
    BiosSettings, BiosStatus, BootInterfaceSelector, ConsoleState, DriverOutcome, ManagerSettings,
    OperationReference, PlatformError, RshimState, SecureBootStatus, VendorJobId,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::nv_redfish::RedfishBmc;
use nv_redfish::schema::computer_system::{BootSource, BootSourceOverrideEnabled, BootUpdate};
use nv_redfish::schema::secure_boot::SecureBootUpdate;

use crate::config::MachineStateHandlerSiteConfig;

mod legacy;

pub use legacy::PlatformBmc;

/// The iDRAC configuration job known by `job_id`.
pub(crate) fn idrac_job(job_id: VendorJobId) -> OperationReference {
    OperationReference::VendorJob {
        uri: format!("/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{job_id}").into(),
        job_id,
        retry_after_seconds: None,
    }
}

/// The vendor job `outcome` started, which the host must wait for before
/// restarting; other work is not waited on.
pub(crate) fn vendor_job_id(outcome: &DriverOutcome) -> Option<VendorJobId> {
    outcome.references().find_map(|reference| match reference {
        OperationReference::VendorJob { job_id, .. } => Some(job_id.clone()),
        OperationReference::RedfishTask { .. } => None,
    })
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

/// Writes every platform setting the platform supports, in order. Settings
/// take effect on the next host restart, which must wait for the returned
/// BIOS work. Work the other writes start is not waited on.
///
/// Secure boot is written after the BIOS because a BIOS write may clear the
/// configuration work the BMC has queued. A BMC may refuse the secure boot
/// write while that BIOS work is pending; the BIOS work is still returned, and
/// secure boot is written once a later apply finds the BIOS settings in effect.
pub async fn apply(
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
    unless_unsupported(apply_settings.await, DriverOutcome::complete())?;
    unless_unsupported(setup_console.await, DriverOutcome::complete())?;
    let bios = unless_unsupported(apply_bios.await, DriverOutcome::complete())?;
    after_pending_bios(
        bios,
        unless_unsupported(disable_secure_boot.await, DriverOutcome::complete()),
    )
}

/// The `bios` work `apply` returns once the secure boot write that followed
/// it answered `secure_boot`. A failed write fails `apply` only when no BIOS
/// work is pending, since failing would lose the work the host must wait for.
fn after_pending_bios(
    bios: DriverOutcome,
    secure_boot: Result<DriverOutcome, PlatformError>,
) -> Result<DriverOutcome, PlatformError> {
    match secure_boot {
        Err(error) if matches!(bios, DriverOutcome::Accepted { .. }) => {
            tracing::warn!(
                %error,
                "disabling secure boot failed while BIOS work is pending; a later apply retries it"
            );
            Ok(bios)
        }
        secure_boot => secure_boot.map(|_| bios),
    }
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

/// Puts `boot_interface`'s HTTP boot option first in the host's persistent
/// boot order. The order takes effect on the next host restart, which must
/// wait for the returned work.
pub async fn configure_boot_order(
    bmc: &PlatformBmc<'_>,
    boot_interface: &BootInterfaceTarget,
) -> Result<DriverOutcome, PlatformError> {
    match bmc {
        PlatformBmc::Libredfish {
            client,
            expected_dpu_count,
        } => legacy::configure_boot_order(*client, boot_interface, *expected_dpu_count).await,
        PlatformBmc::Drivers(bmc) => {
            bmc.boot_order()?
                .configure(&BootInterfaceSelector::from(boot_interface))
                .await
        }
    }
}

/// Whether `boot_interface`'s HTTP boot option is first in the host's boot
/// order. Unlike the other settings, a platform without boot order support is
/// an error: the host could not boot for provisioning.
pub(crate) async fn boot_order_in_effect(
    bmc: &PlatformBmc<'_>,
    boot_interface: &BootInterfaceTarget,
) -> Result<bool, PlatformError> {
    match bmc {
        PlatformBmc::Libredfish { client, .. } => {
            legacy::boot_order_in_effect(*client, boot_interface).await
        }
        PlatformBmc::Drivers(bmc) => Ok(bmc
            .boot_order()?
            .status(&BootInterfaceSelector::from(boot_interface))
            .await?
            .is_configured()),
    }
}

/// What a DPU's BIOS reports of the DPU's platform settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DpuSettings {
    InEffect,
    Pending,
    /// The BIOS reports none of them, as before the DPU's UEFI finishes POST.
    Unreported,
}

impl From<&BiosStatus> for DpuSettings {
    fn from(status: &BiosStatus) -> Self {
        if status.is_applied {
            Self::InEffect
        } else if !status.differences.is_empty()
            && status.differences.iter().all(|diff| diff.actual.is_none())
        {
            Self::Unreported
        } else {
            Self::Pending
        }
    }
}

/// Writes a DPU's platform settings: its BIOS settings, the host's rshim
/// access off, and one UEFI HTTP boot. The BIOS settings take effect on the
/// next DPU restart.
pub(crate) async fn apply_dpu(
    bmc: &PlatformBmc<'_>,
    site_config: &MachineStateHandlerSiteConfig,
) -> Result<(), PlatformError> {
    let bmc = match bmc {
        PlatformBmc::Libredfish {
            client,
            expected_dpu_count,
        } => {
            return legacy::apply(*client, site_config, None, *expected_dpu_count)
                .await
                .map(drop);
        }
        PlatformBmc::Drivers(bmc) => bmc,
    };
    let apply_bios = async { bmc.bios()?.apply(&BiosSettings::default(), None).await };
    let disable_host_rshim = async { bmc.dpu()?.set_host_rshim(RshimState::Disabled).await };
    unless_unsupported(apply_bios.await, DriverOutcome::complete())?;
    unless_unsupported(disable_host_rshim.await, DriverOutcome::complete())?;
    unless_unsupported(set_http_boot_once(bmc).await, DriverOutcome::complete())?;
    Ok(())
}

/// Boots the DPU behind `bmc` from UEFI HTTP once, on its next restart.
pub(crate) async fn boot_http_once(bmc: &PlatformBmc<'_>) -> Result<(), PlatformError> {
    match bmc {
        PlatformBmc::Libredfish { client, .. } => legacy::boot_http_once(*client).await,
        PlatformBmc::Drivers(bmc) => set_http_boot_once(bmc).await.map(drop),
    }
}

async fn set_http_boot_once(
    bmc: &ConnectedBmc<RedfishBmc>,
) -> Result<DriverOutcome, PlatformError> {
    bmc.boot_order()?
        .set_override(
            &BootUpdate::builder()
                .with_boot_source_override_target(BootSource::UefiHttp)
                .with_boot_source_override_enabled(BootSourceOverrideEnabled::Once)
                .build(),
        )
        .await
}

/// What the DPU behind `bmc` reports of its platform settings.
pub(crate) async fn dpu_settings(bmc: &PlatformBmc<'_>) -> Result<DpuSettings, PlatformError> {
    let bmc = match bmc {
        PlatformBmc::Libredfish { client, .. } => return legacy::dpu_settings(*client).await,
        PlatformBmc::Drivers(bmc) => bmc,
    };
    let status = async {
        bmc.bios()?
            .status(&BiosSettings::default(), None)
            .await
            .map(|status| DpuSettings::from(&status))
    };
    unless_unsupported(status.await, DpuSettings::InEffect)
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

#[cfg(test)]
mod tests {
    use bmc_platform::BiosDiff;
    use carbide_test_support::value_scenarios;
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_failed_secure_boot_write_keeps_pending_bios_work() {
        let job = || {
            DriverOutcome::accepted(idrac_job(
                VendorJobId::new("JID_42".to_string()).expect("nonempty"),
            ))
        };
        let refused = || PlatformError::NotReady {
            message: "SYS011".to_string(),
        };
        value_scenarios!(run = |(bios, secure_boot)| after_pending_bios(bios, secure_boot);
            "the secure boot write succeeds" {
                (job(), Ok(DriverOutcome::complete())) => Ok(job()),
                (DriverOutcome::complete(), Ok(DriverOutcome::complete()))
                    => Ok(DriverOutcome::complete()),
            }
            "the secure boot write fails" {
                (job(), Err(refused())) => Ok(job()),
                (DriverOutcome::complete(), Err(refused())) => Err(refused()),
            }
        );
    }

    #[test]
    fn only_vendor_jobs_are_waited_on() {
        let job = || VendorJobId::new("JID_42".to_string()).expect("nonempty");
        let task = || OperationReference::RedfishTask {
            uri: "/redfish/v1/TaskService/Tasks/1".to_string().into(),
            retry_after_seconds: None,
            task: None,
        };
        value_scenarios!(run = |outcome: DriverOutcome| vendor_job_id(&outcome);
            "no vendor job" {
                DriverOutcome::complete() => None,
                DriverOutcome::accepted(task()) => None,
            }
            "a vendor job among other work" {
                DriverOutcome::accepted(task()).merge(DriverOutcome::accepted(idrac_job(job())))
                    => Some(job()),
            }
        );
    }

    /// A DPU's settings from a BIOS whose differing attributes report `actual`.
    fn reporting(actual: Vec<Option<Value>>) -> DpuSettings {
        let differences: Vec<_> = actual
            .into_iter()
            .map(|actual| BiosDiff {
                key: "HostPrivilegeLevel".to_string(),
                expected: json!("Restricted"),
                actual,
            })
            .collect();
        DpuSettings::from(&BiosStatus {
            is_applied: differences.is_empty(),
            differences,
        })
    }

    #[test]
    fn dpu_settings_follow_what_the_bios_reports() {
        value_scenarios!(reporting:
            "every setting in effect" {
                vec![] => DpuSettings::InEffect,
            }
            "a reported setting differs" {
                vec![Some(json!("Privileged"))] => DpuSettings::Pending,
                vec![Some(json!("Privileged")), None] => DpuSettings::Pending,
            }
            "no setting reported" {
                vec![None, None] => DpuSettings::Unreported,
            }
        );
    }
}
