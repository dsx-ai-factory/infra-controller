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

//! The platform settings through libredfish's `machine_setup` and
//! `set_boot_order_dpu_first`, and the choice between it and the drivers by
//! the site's `bmc_backend`.

use std::collections::BTreeMap;

use bmc_backend::{idrac_outcome, libredfish_operation_status, platform_error};
use bmc_platform::{
    BiosSettings, DriverOutcome, ManagerSettings, OperationReference, OperationStatus,
    PlatformError, PlatformIdentity,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::dpu_bios::is_dpu_bios_attributes_not_ready;
use carbide_redfish::nv_redfish::RedfishBmc;
use carbide_uuid::machine::MachineIdSubtypeTrait;
use libredfish::{BiosProfileVendor, Redfish, RedfishError};
use model::machine::{Machine, ManagedHostStateSnapshot};
use serde_json::Value;
use state_controller::state_handler::StateHandlerError;

use super::DpuSettings;
use crate::config::MachineStateHandlerSiteConfig;
use crate::context::MachineStateHandlerServices;

/// A host's or DPU's BMC for its platform settings: the libredfish client the
/// caller already holds, or the drivers.
pub enum PlatformBmc<'a> {
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
        let host = &mh_snapshot.host_snapshot;
        let expected_dpu_count = host.associated_dpu_machine_ids().len();
        Self::connect_machine(services, host, expected_dpu_count, redfish_client).await
    }

    /// The BMC of `dpu` through the site's `bmc_backend`, reusing
    /// `redfish_client` under libredfish. The DPU's host has at least this
    /// DPU.
    pub(crate) async fn connect_dpu(
        services: &MachineStateHandlerServices,
        dpu: &Machine<impl MachineIdSubtypeTrait>,
        redfish_client: &'a dyn Redfish,
    ) -> Result<Self, StateHandlerError> {
        Self::connect_machine(services, dpu, 1, redfish_client).await
    }

    async fn connect_machine(
        services: &MachineStateHandlerServices,
        machine: &Machine<impl MachineIdSubtypeTrait>,
        expected_dpu_count: usize,
        redfish_client: &'a dyn Redfish,
    ) -> Result<Self, StateHandlerError> {
        Ok(match services.connect_drivers(machine).await? {
            Some(bmc) => Self::Drivers(bmc),
            None => Self::Libredfish {
                client: redfish_client,
                expected_dpu_count,
            },
        })
    }

    /// See [`ConnectedBmc::operation_status`] and
    /// [`libredfish_operation_status`].
    pub(crate) async fn operation_status(
        &self,
        reference: &OperationReference,
    ) -> Result<OperationStatus, PlatformError> {
        match self {
            // Boxed so the state handler's future stays within the stack limit.
            Self::Drivers(bmc) => Box::pin(bmc.operation_status(reference)).await,
            Self::Libredfish { client, .. } => {
                libredfish_operation_status(*client, reference).await
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
    call_machine_setup_and_handle_no_dpu_error(
        client,
        boot_interface,
        expected_dpu_count,
        site_config,
    )
    .await
    .map(idrac_outcome)
    .map_err(platform_error)
}

/// See [`super::configure_boot_order`].
pub(super) async fn configure_boot_order(
    client: &dyn Redfish,
    boot_interface: &BootInterfaceTarget,
    expected_dpu_count: usize,
) -> Result<DriverOutcome, PlatformError> {
    set_boot_order_dpu_first_and_handle_no_dpu_error(client, boot_interface, expected_dpu_count)
        .await
        .map(idrac_outcome)
        .map_err(platform_error)
}

/// See [`super::boot_order_in_effect`].
pub(super) async fn boot_order_in_effect(
    client: &dyn Redfish,
    boot_interface: &BootInterfaceTarget,
) -> Result<bool, PlatformError> {
    boot_interface
        .run(|bi| client.is_boot_order_setup(bi))
        .await
        .map_err(platform_error)
}

/// Call [`Redfish::machine_setup`], but ignore any [`RedfishError::NoDpu`] if we expect there to
/// be no DPUs on the host. Returns `Ok(Some(job_id))` when the vendor (e.g. Dell) creates a BIOS
/// config job that must complete before configuring boot order; `Ok(None)` when no job to wait for.
async fn call_machine_setup_and_handle_no_dpu_error(
    redfish_client: &dyn Redfish,
    boot_interface: Option<&BootInterfaceTarget>,
    expected_dpu_count: usize,
    site_config: &MachineStateHandlerSiteConfig,
) -> Result<Option<String>, RedfishError> {
    let setup_result = match boot_interface {
        Some(target) => {
            target
                .run(|bi| {
                    redfish_client.machine_setup(
                        Some(bi),
                        &site_config.bios_profiles,
                        site_config.selected_profile,
                        &site_config.oem_manager_profiles,
                    )
                })
                .await
        }
        None => {
            redfish_client
                .machine_setup(
                    None,
                    &site_config.bios_profiles,
                    site_config.selected_profile,
                    &site_config.oem_manager_profiles,
                )
                .await
        }
    };
    handle_no_dpu_error(setup_result, expected_dpu_count, "machine_setup")
}

async fn set_boot_order_dpu_first_and_handle_no_dpu_error(
    redfish_client: &dyn Redfish,
    boot_interface: &BootInterfaceTarget,
    expected_dpu_count: usize,
) -> Result<Option<String>, RedfishError> {
    let setup_result = boot_interface
        .run(|bi| redfish_client.set_boot_order_dpu_first(bi))
        .await;
    handle_no_dpu_error(setup_result, expected_dpu_count, "set_boot_order_dpu_first")
}

/// Treat `Err(RedfishError::NoDpu)` as `Ok(None)` *only* when the host is
/// declared zero-DPU (`expected_dpu_count == 0`). Other error variants and
/// successful results pass through untouched. The host DPU policy gate in
/// site-explorer is what guarantees `expected_dpu_count == 0` actually
/// means the host carries no managed DPU -- either `Ignore` or `Nic`.
/// Neither has a DPU to answer Redfish, so a `NoDpu` error is expected, not a
/// fault.
fn handle_no_dpu_error(
    result: Result<Option<String>, RedfishError>,
    expected_dpu_count: usize,
    operation: &'static str,
) -> Result<Option<String>, RedfishError> {
    match (result, expected_dpu_count) {
        (Err(RedfishError::NoDpu), 0) => {
            tracing::info!(
                operation,
                "redfish operation failed with NoDpu on a zero-DPU host; treating as Ok"
            );
            Ok(None)
        }
        (other, _) => other,
    }
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

/// See [`super::boot_http_once`].
pub(super) async fn boot_http_once(client: &dyn Redfish) -> Result<(), PlatformError> {
    client
        .boot_once(libredfish::Boot::UefiHttp)
        .await
        .map_err(platform_error)
}

/// See [`super::dpu_settings`].
pub(super) async fn dpu_settings(client: &dyn Redfish) -> Result<DpuSettings, PlatformError> {
    match client.is_bios_setup(None).await {
        Ok(true) => Ok(DpuSettings::InEffect),
        Ok(false) => Ok(DpuSettings::Pending),
        Err(error) if is_dpu_bios_attributes_not_ready(&error) => Ok(DpuSettings::Unreported),
        Err(error) => Err(platform_error(error)),
    }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bmc_platform::{ServiceRootIdentity, SystemIdentity};
    use carbide_redfish::libredfish::test_support::{RedfishSim, RedfishSimAction};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
    use carbide_secrets::credentials::{CredentialKey, CredentialType};
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
    fn handle_no_dpu_error_treats_as_ok_on_zero_dpu_host() {
        let result = handle_no_dpu_error(Err(RedfishError::NoDpu), 0, "machine_setup");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn handle_no_dpu_error_surfaces_when_dpus_were_expected() {
        let result = handle_no_dpu_error(Err(RedfishError::NoDpu), 2, "machine_setup");
        assert!(matches!(result, Err(RedfishError::NoDpu)));
    }

    #[test]
    fn handle_no_dpu_error_passes_through_success() {
        let job_id = "bios-job-1".to_string();
        let result = handle_no_dpu_error(Ok(Some(job_id.clone())), 0, "machine_setup");
        assert!(matches!(result, Ok(Some(ref s)) if s == &job_id));

        let result = handle_no_dpu_error(Ok(None), 2, "machine_setup");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn handle_no_dpu_error_does_not_touch_other_errors() {
        // Other error variants must surface, even on zero-DPU hosts -- we
        // only ignore the *specific* NoDpu signal.
        let result =
            handle_no_dpu_error(Err(RedfishError::Lockdown), 0, "set_boot_order_dpu_first");
        assert!(matches!(result, Err(RedfishError::Lockdown)));
    }

    /// Verify that `oem_manager_profiles` from the site config is forwarded to `machine_setup`.
    ///
    /// This test catches regressions where the argument gets dropped or replaced with an empty map.
    #[tokio::test]
    async fn test_oem_manager_profiles_passed_to_machine_setup() {
        let mut config = MachineStateHandlerSiteConfig::test_default();

        // Build an oem_manager_profiles map with a Dell R760 PSU Hot Spare setting.
        // This mirrors the fix for the Dell R760 PSU fan issue (nvbugs-5834644).
        config.oem_manager_profiles = HashMap::from([(
            RedfishVendor::Dell,
            HashMap::from([(
                "r760".to_string(),
                HashMap::from([(
                    BiosProfileType::Performance,
                    HashMap::from([(
                        "ServerPwr.1.PSRapidOn".to_string(),
                        serde_json::Value::String("Disabled".to_string()),
                    )]),
                )]),
            )]),
        )]);

        let sim = RedfishSim::default();
        let timepoint = sim.timepoint();
        let client = sim
            .create_client(
                "test-host",
                None,
                RedfishAuth::Key(CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                }),
                None,
            )
            .await
            .unwrap();

        let result =
            call_machine_setup_and_handle_no_dpu_error(client.as_ref(), None, 1, &config).await;

        assert!(result.is_ok());

        let actions = sim.actions_since(&timepoint).all_hosts();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            RedfishSimAction::MachineSetup {
                oem_manager_profiles: config.oem_manager_profiles,
                boot_interface_mac: None,
            }
        );
    }
}
