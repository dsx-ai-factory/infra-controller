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

use model::instance::status::SyncState;
use model::instance::status::tenant::{InstanceTenantStatus, TenantState};
use model::machine::nic_firmware::{NicFirmwarePhase, NicFirmwarePurpose};
use model::machine::{InstanceState, ManagedHostState};

use crate as rpc;
use crate::errors::RpcDataConversionError;

/// Converts machine state into the tenant-visible [`TenantState`].
///
/// When `repair_active` is true, [`TenantState::Repairing`] is returned only if the
/// instance would otherwise be tenant-ready (`InstanceState::Ready` with synced configs
/// and extension services ready). It does not override Failed, Updating, Configuring,
/// Provisioning, or Terminating.
///
/// When `operator_managed_networking` is true, NICo has no data-plane readiness
/// signal to wait for. Allocation and network-wait states are therefore projected
/// as tenant-ready while terminal and update states retain precedence.
/// Pre-assignment attestation and assignment-cycle setup remain provisioning.
pub fn instance_status_tenant_state(
    machine_state: ManagedHostState,
    configs_synced: SyncState,
    phone_home_enrolled: bool,
    phone_home_last_contact: Option<chrono::DateTime<chrono::Utc>>,
    extension_services_ready: bool,
    operator_managed_networking: bool,
    repair_active: bool,
) -> Result<TenantState, RpcDataConversionError> {
    // At this point, we are sure that instance is created.
    // If machine state is still ready, means state machine has not processed this instance
    // yet.

    let tenant_ready_state = || {
        if repair_active {
            TenantState::Repairing
        } else {
            TenantState::Ready
        }
    };

    let tenant_state = match machine_state {
        ManagedHostState::Ready => {
            if operator_managed_networking {
                tenant_ready_state()
            } else {
                TenantState::Provisioning
            }
        }
        // These states occur after allocation and before Assigned, while the
        // host is still completing the assignment's attestation checks.
        ManagedHostState::PreAssignedMeasuring { .. } | ManagedHostState::StartAssignmentCycle => {
            TenantState::Provisioning
        }
        ManagedHostState::Assigned { instance_state } => match instance_state {
            InstanceState::Init
            | InstanceState::WaitingForNetworkSegmentToBeReady
            | InstanceState::WaitingForNetworkConfig
            | InstanceState::WaitingForStorageConfig
            | InstanceState::WaitingForExtensionServicesConfig
            | InstanceState::WaitingForRebootToReady => {
                if operator_managed_networking {
                    tenant_ready_state()
                } else {
                    TenantState::Provisioning
                }
            }
            InstanceState::NetworkConfigUpdate { .. } => {
                if operator_managed_networking {
                    tenant_ready_state()
                } else {
                    TenantState::Configuring
                }
            }

            InstanceState::Ready if operator_managed_networking => tenant_ready_state(),
            InstanceState::Ready => {
                let phone_home_pending = phone_home_enrolled && phone_home_last_contact.is_none();

                // TODO phone_home_last_contact window? e.g. must have been received in last 10 minutes
                match (phone_home_pending, configs_synced, extension_services_ready) {
                    // If there is no pending phone-home, but configs are
                    // not synced, configs must have changed after provisioning finished
                    // since we entered Ready state.
                    (false, SyncState::Pending, _) => TenantState::Configuring,

                    // If there is no pending phone-home, but extension services are not ready,
                    // then extension services must have changed after provisioning finished
                    // since we entered Ready state.
                    (false, _, false) => TenantState::Configuring,

                    // If there is no pending phone-home and extension services are ready,
                    // the instance is tenant-ready; surface online repair only in this case.
                    (false, SyncState::Synced, true) if repair_active => TenantState::Repairing,
                    (false, SyncState::Synced, true) => TenantState::Ready,

                    // If there is a pending phone-home, we're still
                    // provisioning.
                    (true, _, _) => TenantState::Provisioning,
                }
            }
            // If termination had been requested (i.e., if the `deleted` column
            // of the instance record in the DB is non-null), then things would
            // have short-circuited to Terminating before ever even getting to
            // this tenant_state function.
            InstanceState::SwitchToAdminNetwork | InstanceState::WaitingForNetworkReconfig => {
                TenantState::Terminating
            }
            // When tenants request a custom pxe reboot, the managed hosts
            // will go through HostPlatformConfiguration and WaitingForDpusToUp
            // before going back to Ready
            InstanceState::WaitingForDpusToUp | InstanceState::HostPlatformConfiguration { .. } => {
                TenantState::Configuring
            }
            InstanceState::BootingWithDiscoveryImage { .. }
            | InstanceState::DPUReprovision { .. }
            | InstanceState::HostReprovision { .. } => TenantState::Updating,
            InstanceState::DpaProvisioning => TenantState::Updating,
            InstanceState::WaitingForDpaToBeReady => TenantState::Updating,
            InstanceState::Failed { .. } => TenantState::Failed,
        },
        ManagedHostState::NicFirmwareUpdating { operation }
            if matches!(operation.purpose, NicFirmwarePurpose::Allocation { .. }) =>
        {
            match operation.phase {
                NicFirmwarePhase::Failed { .. } => TenantState::Failed,
                _ => TenantState::Updating,
            }
        }
        ManagedHostState::NicFirmwareUpdating { operation }
            if matches!(
                operation.purpose,
                NicFirmwarePurpose::RestoreBaseline { .. }
            ) =>
        {
            TenantState::Terminating
        }
        ManagedHostState::ForceDeletion | ManagedHostState::Reset { .. } => {
            TenantState::Terminating
        }
        _ => {
            tracing::error!(%machine_state, "Invalid state during state handling");
            TenantState::Invalid
        }
    };

    Ok(tenant_state)
}

impl TryFrom<InstanceTenantStatus> for rpc::InstanceTenantStatus {
    type Error = RpcDataConversionError;

    fn try_from(state: InstanceTenantStatus) -> Result<Self, Self::Error> {
        Ok(rpc::InstanceTenantStatus {
            state: rpc::TenantState::try_from(state.state)? as i32,
            state_details: state.state_details,
        })
    }
}

impl TryFrom<TenantState> for rpc::TenantState {
    type Error = RpcDataConversionError;

    fn try_from(state: TenantState) -> Result<Self, Self::Error> {
        Ok(match state {
            TenantState::Provisioning => rpc::TenantState::Provisioning,
            TenantState::DpuReprovisioning => rpc::TenantState::DpuReprovisioning,
            TenantState::Ready => rpc::TenantState::Ready,
            TenantState::Configuring => rpc::TenantState::Configuring,
            TenantState::Terminating => rpc::TenantState::Terminating,
            TenantState::Terminated => rpc::TenantState::Terminated,
            TenantState::Failed => rpc::TenantState::Failed,
            TenantState::HostReprovisioning => rpc::TenantState::HostReprovisioning,
            TenantState::Updating => rpc::TenantState::Updating,
            TenantState::Invalid => rpc::TenantState::Invalid,
            TenantState::Repairing => rpc::TenantState::Repairing,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use carbide_uuid::machine::MachineId;
    use chrono::Utc;
    use health_report::{HealthReport, REPAIR_REQUEST_MERGE_SOURCE};
    use model::health::HealthReportSources;
    use model::instance::status::SyncState;
    use model::machine::{
        DpuReprovisionStates, FailureCause, FailureDetails, FailureSource, InstanceState,
        ManagedHostState,
    };

    use super::*;

    /// Firmware work cannot expose an allocation as ready, and restoration
    /// remains part of releasing its previous tenant.
    #[test]
    fn nic_firmware_tenant_state() {
        let owner = uuid::Uuid::nil().into();
        scenarios!(
            run = |(purpose, phase)| {
                let mut operation = model::test_support::nic_firmware::operation();
                operation.purpose = purpose;
                operation.phase = phase;
                instance_status_tenant_state(
                    ManagedHostState::NicFirmwareUpdating { operation },
                    SyncState::Synced, false, None, true, true, true,
                ).map_err(drop)
            };
            "active work cannot become tenant ready" {
                (NicFirmwarePurpose::Allocation { instance_id: owner }, NicFirmwarePhase::ObservingSupervisor { target_index: 0 })
                    => Yields(TenantState::Updating),
            }
            "terminal allocation failure remains visible" {
                (NicFirmwarePurpose::Allocation { instance_id: owner }, NicFirmwarePhase::Failed { reason: "required verification failed".into() })
                    => Yields(TenantState::Failed),
            }
            "baseline restoration keeps the old tenant terminating" {
                (NicFirmwarePurpose::RestoreBaseline { instance_id: owner, cleanup_context: model::machine::CleanupContext::Deprovision }, NicFirmwarePhase::ObservingSupervisor { target_index: 0 })
                    => Yields(TenantState::Terminating),
            }
        );
    }

    #[test]
    fn repair_merge_active_detects_merge_sources() {
        let mut health = HealthReportSources::default();
        assert!(!health.repair_merge_active());
        health.merges.insert(
            REPAIR_REQUEST_MERGE_SOURCE.to_string(),
            HealthReport {
                source: REPAIR_REQUEST_MERGE_SOURCE.to_string(),
                ..Default::default()
            },
        );
        assert!(health.repair_merge_active());
    }

    #[test]
    fn repair_merge_tenant_state_precedence() {
        let machine_id =
            MachineId::from_str("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
                .unwrap();
        let failed = InstanceState::Failed {
            details: FailureDetails {
                cause: FailureCause::NoError,
                failed_at: Utc::now(),
                source: FailureSource::StateMachine,
            },
            machine_id,
        };

        // Each row: a (machine_state, configs_synced) pair under repair_active=true,
        // exercising which states repair-merge does or does not override.
        scenarios!(
            run = |(machine_state, configs_synced)| {
                instance_status_tenant_state(
                    machine_state,
                    configs_synced,
                    false,
                    None,
                    true,
                    false,
                    true,
                )
                .map_err(drop)
            };
            "tenant-ready with repair merge" {
                (
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::Ready,
                    },
                    SyncState::Synced,
                ) => Yields(TenantState::Repairing),
            }

            "terminating with repair merge" {
                (
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::SwitchToAdminNetwork,
                    },
                    SyncState::Synced,
                ) => Yields(TenantState::Terminating),
            }

            "reprovision with repair merge" {
                (
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::DPUReprovision {
                            dpu_states: DpuReprovisionStates {
                                states: HashMap::new(),
                            },
                        },
                    },
                    SyncState::Synced,
                ) => Yields(TenantState::Updating),
            }

            "configuring with repair merge" {
                (
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::Ready,
                    },
                    SyncState::Pending,
                ) => Yields(TenantState::Configuring),
            }

            "failed with repair merge" {
                (
                    ManagedHostState::Assigned {
                        instance_state: failed,
                    },
                    SyncState::Synced,
                ) => Yields(TenantState::Failed),
            }
        );
    }

    #[test]
    fn assignment_checks_remain_provisioning() {
        scenarios!(
            run = |(machine_state, operator_managed_networking, repair_active)| {
                instance_status_tenant_state(
                    machine_state,
                    SyncState::Synced,
                    false,
                    None,
                    true,
                    operator_managed_networking,
                    repair_active,
                )
                .map_err(drop)
            };
            "pre-assignment attestation" {
                (
                    ManagedHostState::PreAssignedMeasuring {
                        spdm_measuring_state: model::machine::SpdmMeasuringState::TriggerMeasurements,
                    },
                    false,
                    false,
                ) => Yields(TenantState::Provisioning),
            }

            "start assignment cycle" {
                (ManagedHostState::StartAssignmentCycle, false, false) => Yields(TenantState::Provisioning),
            }

            "operator-managed networking still requires assignment checks" {
                (
                    ManagedHostState::PreAssignedMeasuring {
                        spdm_measuring_state: model::machine::SpdmMeasuringState::TriggerMeasurements,
                    },
                    true,
                    true,
                ) => Yields(TenantState::Provisioning),
                (ManagedHostState::StartAssignmentCycle, true, true) => Yields(TenantState::Provisioning),
            }
        );
    }

    #[test]
    fn existing_managed_allocation_waits_for_configuration() {
        scenarios!(
            run = |machine_state| {
                instance_status_tenant_state(
                    machine_state, SyncState::Synced, false, None, true, false, false,
                ).map_err(drop)
            };
            "allocation awaits controller pickup" {
                ManagedHostState::Ready => Yields(TenantState::Provisioning),
            }
            "assigned allocation still awaits provisioning" {
                ManagedHostState::Assigned { instance_state: InstanceState::Init }
                    => Yields(TenantState::Provisioning),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForNetworkSegmentToBeReady }
                    => Yields(TenantState::Provisioning),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForNetworkConfig }
                    => Yields(TenantState::Provisioning),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForStorageConfig }
                    => Yields(TenantState::Provisioning),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForExtensionServicesConfig }
                    => Yields(TenantState::Provisioning),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForRebootToReady }
                    => Yields(TenantState::Provisioning),
            }
            "subsequent network changes are configuring, not initial provisioning" {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::NetworkConfigUpdate {
                        network_config_update_state:
                            model::machine::NetworkConfigUpdateState::WaitingForNetworkSegmentToBeReady,
                    },
                } => Yields(TenantState::Configuring),
            }
        );
    }

    #[test]
    fn existing_ready_prerequisites_and_precedence() {
        let contacted = Some(Utc::now());
        // Keep independent readiness prerequisites and their precedence explicit.
        // Repair cannot hide a pending phone-home or extension-service update.
        scenarios!(
            run = |(synced, enrolled, last_contact, extensions_ready, repair)| {
                instance_status_tenant_state(
                    ManagedHostState::Assigned { instance_state: InstanceState::Ready },
                    synced, enrolled, last_contact, extensions_ready, false, repair,
                ).map_err(drop)
            };
            "phone-home is optional when not enrolled" {
                (SyncState::Synced, false, None, true, false) => Yields(TenantState::Ready),
            }
            "enrolled instance becomes ready after contacting home" {
                (SyncState::Synced, true, contacted, true, false) => Yields(TenantState::Ready),
            }
            "enrolled instance must contact home first" {
                (SyncState::Synced, true, None, true, false) => Yields(TenantState::Provisioning),
            }
            "initial phone-home takes precedence over updates and repair" {
                (SyncState::Pending, true, None, false, true) => Yields(TenantState::Provisioning),
            }
            "configuration changes after phone-home are configuring" {
                (SyncState::Pending, true, contacted, true, false) => Yields(TenantState::Configuring),
            }
            "extension readiness is independent of phone-home enrollment" {
                (SyncState::Synced, false, None, false, true) => Yields(TenantState::Configuring),
                (SyncState::Synced, true, contacted, false, false) => Yields(TenantState::Configuring),
            }
        );
    }

    #[test]
    fn existing_lifecycle_states_override_operator_readiness_and_repair() {
        let failed = InstanceState::Failed {
            details: FailureDetails {
                cause: FailureCause::NoError,
                failed_at: Utc::now(),
                source: FailureSource::StateMachine,
            },
            machine_id: MachineId::from_str(
                "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
            )
            .unwrap(),
        };
        scenarios!(
            run = |machine_state| {
                instance_status_tenant_state(
                    machine_state, SyncState::Synced, false, None, true, true, true,
                ).map_err(drop)
            };
            "release retains terminating status" {
                ManagedHostState::Assigned { instance_state: InstanceState::SwitchToAdminNetwork }
                    => Yields(TenantState::Terminating),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForNetworkReconfig }
                    => Yields(TenantState::Terminating),
                ManagedHostState::ForceDeletion => Yields(TenantState::Terminating),
                ManagedHostState::Reset { reset_state: model::machine::ResetState::DeletingInstance }
                    => Yields(TenantState::Terminating),
            }
            "platform reconfiguration remains visible" {
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForDpusToUp }
                    => Yields(TenantState::Configuring),
                ManagedHostState::Assigned {
                    instance_state: InstanceState::HostPlatformConfiguration {
                        platform_config_state: model::machine::HostPlatformConfigurationState::CheckHostConfig,
                    },
                } => Yields(TenantState::Configuring),
            }
            "reprovision and DPA work remain updating" {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::BootingWithDiscoveryImage { retry: Default::default() },
                } => Yields(TenantState::Updating),
                ManagedHostState::Assigned {
                    instance_state: InstanceState::DPUReprovision {
                        dpu_states: DpuReprovisionStates { states: HashMap::new() },
                    },
                } => Yields(TenantState::Updating),
                ManagedHostState::Assigned {
                    instance_state: InstanceState::HostReprovision {
                        reprovision_state: model::machine::HostReprovisionState::CheckingFirmwareV2 {
                            firmware_type: None, firmware_number: None,
                        },
                    },
                } => Yields(TenantState::Updating),
                ManagedHostState::Assigned { instance_state: InstanceState::DpaProvisioning }
                    => Yields(TenantState::Updating),
                ManagedHostState::Assigned { instance_state: InstanceState::WaitingForDpaToBeReady }
                    => Yields(TenantState::Updating),
            }
            "failure is not hidden by repair or operator networking" {
                ManagedHostState::Assigned { instance_state: failed } => Yields(TenantState::Failed),
            }
            "unrelated pre-allocation states stay invalid for a tenant" {
                ManagedHostState::Created => Yields(TenantState::Invalid),
            }
        );
    }

    #[test]
    fn operator_managed_allocations_project_as_tenant_ready() {
        // Allocated/network-wait states where Flat has no NICo readiness signal:
        // operator-managed networking should not wait on network observations.
        scenarios!(
            run = |machine_state| {
                instance_status_tenant_state(
                    machine_state,
                    SyncState::Pending,
                    true,
                    None,
                    false,
                    true,
                    false,
                )
                .map_err(drop)
            };
            "allocated before state controller pickup" {
                ManagedHostState::Ready => Yields(TenantState::Ready),
            }

            "assigned init" {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::Init,
                } => Yields(TenantState::Ready),
            }

            "waiting for network config" {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::WaitingForNetworkConfig,
                } => Yields(TenantState::Ready),
            }

            "network config update" {
                ManagedHostState::Assigned {
                    instance_state: InstanceState::NetworkConfigUpdate {
                        network_config_update_state:
                            model::machine::NetworkConfigUpdateState::WaitingForNetworkSegmentToBeReady,
                    },
                } => Yields(TenantState::Ready),
            }

            "assigned ready ignores NICo readiness signals for operator networking" {
                ManagedHostState::Assigned { instance_state: InstanceState::Ready }
                    => Yields(TenantState::Ready),
            }
        );
    }
}
