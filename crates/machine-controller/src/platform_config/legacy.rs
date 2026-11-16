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

//! The platform settings through libredfish's `machine_setup`, and the choice
//! between it and the drivers by the site's `bmc_backend`.

use std::collections::BTreeMap;

use bmc_backend::platform_error;
use bmc_platform::{
    BiosSettings, DriverOutcome, ManagerSettings, OperationReference, OperationStatus,
    PlatformError, PlatformIdentity, VendorJobId,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::{BiosProfileVendor, JobState, Redfish};
use model::machine::ManagedHostStateSnapshot;
use serde_json::Value;
use state_controller::state_handler::StateHandlerError;

use super::idrac_job;
use crate::config::MachineStateHandlerSiteConfig;
use crate::context::MachineStateHandlerServices;
use crate::handler::call_machine_setup_and_handle_no_dpu_error;

/// A host's BMC for its platform settings: the libredfish client the caller
/// already holds, or the drivers.
pub(crate) enum PlatformBmc<'a> {
    Libredfish {
        client: &'a dyn Redfish,
        /// libredfish reports a host without a boot interface as an error,
        /// which stands only when the host has DPUs to boot from.
        expected_dpu_count: usize,
    },
    Drivers(Box<ConnectedBmc<RedfishBmc>>),
}

impl<'a> PlatformBmc<'a> {
    /// The host's BMC through the site's `bmc_backend`, reusing
    /// `redfish_client` under libredfish.
    pub(crate) async fn connect(
        services: &MachineStateHandlerServices,
        mh_snapshot: &ManagedHostStateSnapshot,
        redfish_client: &'a dyn Redfish,
    ) -> Result<Self, StateHandlerError> {
        Ok(
            match services.connect_drivers(&mh_snapshot.host_snapshot).await? {
                Some(bmc) => Self::Drivers(bmc),
                None => Self::Libredfish {
                    client: redfish_client,
                    expected_dpu_count: mh_snapshot
                        .host_snapshot
                        .associated_dpu_machine_ids()
                        .len(),
                },
            },
        )
    }

    /// See [`ConnectedBmc::operation_status`]. libredfish reads only the
    /// iDRAC jobs its `machine_setup` starts.
    pub(crate) async fn operation_status(
        &self,
        reference: &OperationReference,
    ) -> Result<OperationStatus, PlatformError> {
        match (self, reference) {
            (Self::Drivers(bmc), reference) => bmc.operation_status(reference).await,
            (Self::Libredfish { client, .. }, OperationReference::VendorJob { job_id, .. }) => {
                client
                    .get_job_state(job_id.as_str())
                    .await
                    .map(job_status)
                    .map_err(platform_error)
            }
            (Self::Libredfish { .. }, OperationReference::RedfishTask { .. }) => {
                Err(PlatformError::Unsupported)
            }
        }
    }
}

/// See [`super::apply`].
pub(super) async fn apply(
    client: &dyn Redfish,
    site_config: &MachineStateHandlerSiteConfig,
    boot_interface: Option<&BootInterfaceTarget>,
    expected_dpu_count: usize,
) -> Result<DriverOutcome, PlatformError> {
    let job_id = call_machine_setup_and_handle_no_dpu_error(
        client,
        boot_interface,
        expected_dpu_count,
        site_config,
    )
    .await
    .map_err(platform_error)?;
    Ok(job_id
        .and_then(|job_id| VendorJobId::new(job_id).ok())
        .map_or_else(DriverOutcome::complete, |job_id| {
            DriverOutcome::accepted(idrac_job(job_id))
        }))
}

/// See [`super::in_effect`].
pub(super) async fn in_effect(
    client: &dyn Redfish,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<bool, PlatformError> {
    match boot_interface {
        Some(target) => target.run(|bi| client.is_bios_setup(Some(bi))).await,
        None => client.is_bios_setup(None).await,
    }
    .map_err(platform_error)
}

/// The BIOS and BMC manager profiles the site selected for the BMC with
/// `identity`: the ones configured for its service root vendor and system
/// model.
pub(super) fn profiles(
    site_config: &MachineStateHandlerSiteConfig,
    identity: &PlatformIdentity,
) -> (BiosSettings, ManagerSettings) {
    let selected = |profiles: &BiosProfileVendor| -> BTreeMap<String, Value> {
        let Some(vendor) = identity.service_root.vendor.as_deref() else {
            return BTreeMap::new();
        };
        let model = libredfish::model_coerce(
            identity
                .system
                .as_ref()
                .and_then(|system| system.model.as_deref())
                .unwrap_or_default(),
        );
        profiles
            .iter()
            .filter(|(key, _)| key.to_string().eq_ignore_ascii_case(vendor))
            .find_map(|(_, models)| models.get(&model))
            .and_then(|profiles| profiles.get(&site_config.selected_profile))
            .map(|attributes| attributes.clone().into_iter().collect())
            .unwrap_or_default()
    };
    (
        BiosSettings {
            attributes: selected(&site_config.bios_profiles),
        },
        ManagerSettings {
            attributes: selected(&site_config.oem_manager_profiles),
        },
    )
}

/// An iDRAC job state libredfish does not recognize is still running.
fn job_status(state: JobState) -> OperationStatus {
    match state {
        JobState::Scheduled => OperationStatus::AwaitingReset,
        JobState::Running | JobState::Unknown => OperationStatus::Running,
        JobState::Completed => OperationStatus::Completed,
        JobState::ScheduledWithErrors | JobState::CompletedWithErrors | JobState::Failed => {
            OperationStatus::Failed {
                state: format!("{state:?}"),
                message: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bmc_platform::{ServiceRootIdentity, SystemIdentity};
    use carbide_test_support::value_scenarios;
    use libredfish::BiosProfileType;
    use libredfish::model::service_root::RedfishVendor;
    use serde_json::json;

    use super::*;

    /// A profile applies to the BMCs whose service root vendor names its
    /// vendor key and whose system model, with spaces made underscores, names
    /// its model key.
    #[test]
    fn profiles_follow_the_configured_vendor_and_model() {
        let configured: BiosProfileVendor = HashMap::from([(
            RedfishVendor::Hpe,
            HashMap::from([(
                "Model_X".to_string(),
                HashMap::from([(
                    BiosProfileType::Performance,
                    HashMap::from([("Attribute".to_string(), json!("Value"))]),
                )]),
            )]),
        )]);
        let site_config = MachineStateHandlerSiteConfig {
            bios_profiles: configured.clone(),
            oem_manager_profiles: configured,
            ..MachineStateHandlerSiteConfig::test_default()
        };
        let identity = |vendor: &str, model: &str| PlatformIdentity {
            service_root: ServiceRootIdentity {
                vendor: Some(vendor.to_string()),
                ..Default::default()
            },
            system: Some(SystemIdentity {
                model: Some(model.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = BTreeMap::from([("Attribute".to_string(), json!("Value"))]);

        for (scenario, identity, expected) in [
            (
                "the configured vendor and model",
                identity("HPE", "Model X"),
                profile,
            ),
            ("another model", identity("HPE", "Model Y"), BTreeMap::new()),
            (
                "another vendor",
                identity("Dell", "Model X"),
                BTreeMap::new(),
            ),
        ] {
            let (bios, manager) = profiles(&site_config, &identity);
            assert_eq!(bios.attributes, expected, "{scenario}");
            assert_eq!(manager.attributes, expected, "{scenario}");
        }
    }

    #[test]
    fn idrac_job_states_map_to_operation_statuses() {
        let failed = |state: &str| OperationStatus::Failed {
            state: state.to_string(),
            message: None,
        };
        value_scenarios!(job_status:
            "awaiting the host restart" {
                JobState::Scheduled => OperationStatus::AwaitingReset,
            }
            "running, including states libredfish does not recognize" {
                JobState::Running => OperationStatus::Running,
                JobState::Unknown => OperationStatus::Running,
            }
            "completed" {
                JobState::Completed => OperationStatus::Completed,
            }
            "ended without completing" {
                JobState::ScheduledWithErrors => failed("ScheduledWithErrors"),
                JobState::CompletedWithErrors => failed("CompletedWithErrors"),
                JobState::Failed => failed("Failed"),
            }
        );
    }
}
