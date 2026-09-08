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

use std::collections::HashMap;

use carbide_uuid::machine::DpuMachineId;
use config_version::Versioned;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::InstanceNetworkConfig;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::instance::status::spx::InstanceSpxStatus;
use model::instance::status::{InstanceStatus, InstanceStatusObservations, SyncState};
use model::machine::infiniband::MachineInfinibandStatusObservation;
use model::machine::nvlink::MachineNvLinkStatusObservation;
use model::machine::spx::MachineSpxStatusObservation;
use model::machine::{ManagedHostState, ReprovisionRequest};

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::model::instance::status::tenant::instance_status_tenant_state;

pub mod extension_service;
pub mod infiniband;
pub mod network;
pub mod nvlink;
pub mod spx;
pub mod tenant;

impl TryFrom<InstanceStatus> for rpc::InstanceStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceStatus) -> Result<Self, Self::Error> {
        Ok(rpc::InstanceStatus {
            tenant: status.tenant.map(|status| status.try_into()).transpose()?,
            network: Some(status.network.try_into()?),
            infiniband: Some(status.infiniband.try_into()?),
            dpu_extension_services: Some(status.extension_services.try_into()?),
            nvlink: Some(status.nvlink.try_into()?),
            spx_status: Some(status.spx_status.try_into()?),
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
            update: status.reprovision_request.map(|request| request.into()),
        })
    }
}

/// Derives an Instances network status from the users desired config
/// and status that we observed from the networking subsystem.
///
/// This mechanism guarantees that the status we return to the user always
/// matches the latest `Config` set by the user. We can not directly
/// forwarding the last observed status without taking `Config` into account,
/// because the observation might have been related to a different config,
/// and the interfaces therefore won't match.
#[allow(clippy::too_many_arguments)]
pub fn instance_status_from_config_and_observation(
    dpu_id_to_device_map: HashMap<String, Vec<DpuMachineId>>,
    primary_dpu_machine_id: Option<DpuMachineId>,
    instance_config: Versioned<&InstanceConfig>,
    network_config: Versioned<&InstanceNetworkConfig>,
    ib_config: Versioned<&InstanceInfinibandConfig>,
    extension_services_config: Versioned<&InstanceExtensionServicesConfig>,
    nvlink_config: Versioned<&InstanceNvLinkConfig>,
    spx_config: Versioned<&InstanceSpxConfig>,
    observations: &InstanceStatusObservations,
    machine_state: ManagedHostState,
    delete_requested: bool,
    reprovision_request: Option<ReprovisionRequest>,
    ib_status: Option<&MachineInfinibandStatusObservation>,
    nvlink_status: Option<&MachineNvLinkStatusObservation>,
    spx_status: Option<&MachineSpxStatusObservation>,
    is_network_config_request_pending: bool,
    host_health: &model::health::HealthReportSources,
) -> Result<InstanceStatus, RpcDataConversionError> {
    let mut instance_config_synced = SyncState::Synced;

    let operator_managed_networking = network_config.uses_operator_managed_networking();

    for network_obs in observations.network.values() {
        if let Some(version_obs) = network_obs.instance_config_version
            && instance_config.version != version_obs
        {
            instance_config_synced = SyncState::Pending;
            break;
        }
        // TODO(bcavanagh): Switch to SyncState::Pending or
        //                  return Err(RpcDataConversionError::InvalidConfigVersion)
        //                  after all dpu-agents have been updated to support/report the field.
        // If observations.network.instance_config_version was None, then "ignore"
    }

    let used_dpu_ids = network_config
        .value
        .get_used_dpus(&dpu_id_to_device_map, primary_dpu_machine_id);

    let network =
        model::instance::status::network::InstanceNetworkStatus::from_config_and_observations(
            dpu_id_to_device_map,
            network_config,
            &observations.network,
            is_network_config_request_pending,
        );

    let infiniband =
        model::instance::status::infiniband::InstanceInfinibandStatus::from_config_and_observation(
            ib_config, ib_status,
        );

    let extension_services =
        model::instance::status::extension_service::InstanceExtensionServicesStatus::from_config_and_type_observations(
            &used_dpu_ids,
            extension_services_config,
            &observations.extension_services,
            delete_requested,
        );
    let extension_services_ready =
        model::instance::status::extension_service::is_extension_services_ready(
            &extension_services,
        );

    let nvlink = model::instance::status::nvlink::InstanceNvLinkStatus::from_config_and_observation(
        nvlink_config,
        nvlink_status,
    );

    let spx_status = InstanceSpxStatus::from_config_and_observation(spx_config, spx_status);

    let phone_home_last_contact = observations.phone_home_last_contact;

    // If additional configs are added, they need to be incorporated here
    let configs_synced = match (
        network.configs_synced,
        infiniband.configs_synced,
        extension_services.configs_synced,
        nvlink.configs_synced,
        spx_status.configs_synced,
        instance_config_synced,
    ) {
        (
            SyncState::Synced,
            SyncState::Synced,
            SyncState::Synced,
            SyncState::Synced,
            SyncState::Synced,
            SyncState::Synced,
        ) => SyncState::Synced,
        _ => SyncState::Pending,
    };

    let tenant = model::instance::status::tenant::InstanceTenantStatus {
        state: match delete_requested {
            false => instance_status_tenant_state(
                machine_state,
                configs_synced,
                instance_config.os.phone_home_enabled,
                phone_home_last_contact,
                extension_services_ready,
                operator_managed_networking,
                host_health.repair_merge_active(),
            )?,
            true => {
                // If instance deletion was requested, we always confirm the
                // tenant that the instance is actually in progress of shutting down.
                // The instance might however still first need to run through
                // various provisioning steps to become "ready" before starting
                // to terminate
                model::instance::status::tenant::TenantState::Terminating
            }
        },
        state_details: String::new(),
    };

    Ok(InstanceStatus {
        tenant: Some(tenant),
        network,
        infiniband,
        extension_services,
        nvlink,
        spx_status,
        configs_synced,
        reprovision_request,
    })
}

impl TryFrom<SyncState> for rpc::SyncState {
    type Error = RpcDataConversionError;

    fn try_from(state: SyncState) -> Result<Self, Self::Error> {
        Ok(match state {
            SyncState::Synced => rpc::SyncState::Synced,
            SyncState::Pending => rpc::SyncState::Pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use config_version::ConfigVersion;
    use health_report::{HealthReport, REPAIR_REQUEST_MERGE_SOURCE};
    use model::health::HealthReportSources;
    use model::instance::config::InstanceConfig;
    use model::instance::config::extension_services::InstanceExtensionServicesConfig;
    use model::instance::config::infiniband::InstanceInfinibandConfig;
    use model::instance::config::network::InstanceNetworkConfig;
    use model::instance::config::nvlink::InstanceNvLinkConfig;
    use model::instance::config::tenant_config::TenantConfig;
    use model::instance::status::InstanceStatusObservations;
    use model::instance::status::tenant::TenantState;
    use model::machine::{DpuReprovisionStates, InstanceState, ManagedHostState, ReprovisionState};
    use model::os::{OperatingSystem, OperatingSystemVariant};
    use model::tenant::TenantOrganizationId;
    use uuid::Uuid;

    use super::*;

    fn minimal_instance_config() -> InstanceConfig {
        InstanceConfig {
            tenant: TenantConfig {
                tenant_organization_id: TenantOrganizationId::try_from("TenantA".to_string())
                    .unwrap(),
                tenant_keyset_ids: vec![],
                hostname: None,
            },
            os: OperatingSystem {
                user_data: None,
                variant: OperatingSystemVariant::OsImage(Uuid::nil()),
                phone_home_enabled: false,
                run_provisioning_instructions_on_every_boot: false,
            },
            network: InstanceNetworkConfig::default(),
            infiniband: InstanceInfinibandConfig::default(),
            network_security_group_id: None,
            extension_services: InstanceExtensionServicesConfig::default(),
            nvlink: InstanceNvLinkConfig::default(),
            spxconfig: InstanceSpxConfig::default(),
            power_profile: None,
        }
    }

    #[test]
    fn repair_merge_active_yields_repairing_via_status_pipeline() {
        let config = minimal_instance_config();
        let version = ConfigVersion::initial();
        let mut health = HealthReportSources::default();
        health.merges.insert(
            REPAIR_REQUEST_MERGE_SOURCE.to_string(),
            HealthReport {
                source: REPAIR_REQUEST_MERGE_SOURCE.to_string(),
                ..Default::default()
            },
        );

        let status = instance_status_from_config_and_observation(
            HashMap::new(),
            None,
            Versioned::new(&config, version),
            Versioned::new(&config.network, version),
            Versioned::new(&config.infiniband, version),
            Versioned::new(&config.extension_services, version),
            Versioned::new(&config.nvlink, version),
            Versioned::new(&config.spxconfig, version),
            &InstanceStatusObservations {
                network: HashMap::new(),
                extension_services: HashMap::new(),
                phone_home_last_contact: None,
            },
            ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            },
            false,
            None,
            None,
            None,
            None,
            false,
            &health,
        )
        .unwrap();

        assert_eq!(
            status.tenant.as_ref().map(|t| t.state),
            Some(TenantState::Repairing)
        );
    }

    #[test]
    fn test_tenant_state() {
        let machine_id =
            DpuMachineId::from_str("fm100dtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
                .unwrap();

        assert_eq!(
            instance_status_tenant_state(
                ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates {
                        states: HashMap::from([(
                            machine_id,
                            ReprovisionState::WaitingForNetworkConfig,
                        )]),
                    },
                },
                SyncState::Synced,
                false,
                None,
                false,
                false,
                false,
            )
            .unwrap(),
            TenantState::Invalid
        );
    }
}
