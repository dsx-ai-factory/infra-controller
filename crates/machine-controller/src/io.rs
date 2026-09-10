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

//! State Controller IO implementation for Machines

use carbide_uuid::machine::HostMachineId;
use config_version::{ConfigVersion, Versioned};
use db::{self, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpa_interface::DpaSearchConfig;
use model::machine::slas::MachineSlaConfig;
use model::machine::{
    self, AttestationMode, ConfigureAstraState, DecommissioningState, DpuDiscoveringState,
    DpuInitState, HostHealthConfig, MachineMaintenanceOperation, MachineValidatingState,
    ManagedHostState, ManagedHostStateSnapshot, MeasuringState, ReadyBootConfigState,
    SpdmMeasuringState, ValidationState,
};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate::context::MachineStateHandlerContextObjects;
use crate::metrics::MachineMetricsEmitter;

/// State Controller IO implementation for Machines
#[derive(Default, Debug)]
pub struct MachineStateControllerIO {
    pub host_health: HostHealthConfig,
    pub sla_config: MachineSlaConfig,
}

#[async_trait::async_trait]
impl StateControllerIO for MachineStateControllerIO {
    type ObjectId = HostMachineId;
    type State = ManagedHostStateSnapshot;
    type ControllerState = ManagedHostState;
    type MetricsEmitter = MachineMetricsEmitter;
    type ContextObjects = MachineStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "machine_state_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "machine_state_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "machine_state_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db::managed_host::load_host_ids(txn).await
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        machine_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let mut retstate = db::managed_host::load_snapshot(
            txn,
            machine_id,
            model::machine::LoadSnapshotOptions {
                include_history: false,
                include_instance_data: true,
                host_health_config: self.host_health,
            },
        )
        .await?;

        if let Some(retstate) = retstate.as_mut() {
            let dpa_search_config = DpaSearchConfig {
                only_svpc: false,
                only_astra: false,
            };
            let dpa_snapshots =
                db::dpa_interface::find_by_machine_id(&mut *txn, *machine_id, dpa_search_config)
                    .await?;
            retstate.dpa_interface_snapshots = dpa_snapshots;
        };

        return Ok(retstate);
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        let current = state.host_snapshot.state.clone();

        Ok(Versioned::new(current.value, current.version))
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        _old_version: ConfigVersion,
        _new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<bool, DatabaseError> {
        db::machine::update_state(txn, object_id, new_state).await?;
        Ok(true)
    }

    /// State history for machines (including DPUs) is persisted internally by
    /// `db::machine::advance()` inside `update_state`, so this is actually a
    /// no-op for now.
    // TODO(chet): Pull this in as well.
    async fn persist_state_history(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _new_version: ConfigVersion,
        _new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::machine::update_controller_state_outcome(txn, object_id, outcome).await
    }

    fn metric_state_names(state: &ManagedHostState) -> (&'static str, &'static str) {
        use model::machine::{CleanupState, InstanceState, MachineState};

        fn dpuinit_state_name(dpu_state: &DpuInitState) -> &'static str {
            match dpu_state {
                DpuInitState::InstallDpuOs { .. } => "installdpuos",
                DpuInitState::Init => "init",
                DpuInitState::WaitingForNetworkInstall => "waitingfornetworkinstall",
                DpuInitState::WaitingForNetworkConfig => "waitingfornetworkconfig",
                DpuInitState::WaitingForPlatformConfiguration => "waitingforplatformconfiguration",
                DpuInitState::PollingBiosSetup => "pollingbiossetup",
                // The observed-Off wait remains part of the existing
                // operator-facing power-cycle metric.
                DpuInitState::WaitingForPlatformPowercycle { .. }
                | DpuInitState::WaitingForPlatformPowerOff => "waitingforplatformpowercycle",
                DpuInitState::DpfStates { .. } => "dpfstates",
            }
        }

        fn machine_state_name(machine_state: &MachineState) -> &'static str {
            match machine_state {
                MachineState::Init => "init",
                MachineState::WaitingForPlatformConfiguration { .. } => {
                    "waitingforplatformconfiguration"
                }
                MachineState::WaitingForBiosJob { .. } => "waitingforbiosjob",
                MachineState::PollingBiosSetup { .. } => "pollingbiossetup",
                MachineState::SetBootOrder { .. } => "setbootorder",
                MachineState::UefiSetup { .. } => "uefisetup",
                MachineState::WaitingForDiscovery => "waitingfordiscovery",
                MachineState::Discovered { .. } => "discovered",
                MachineState::WaitingForLockdown { .. } => "waitingforlockdown",
                MachineState::EnableIpmiOverLan => "enableipmioverlan",
                MachineState::Measuring { .. } => "machinestatemeasuring",
                MachineState::SpdmMeasuring { .. } => "machinestatespdmmeasuring",
            }
        }

        fn discovering_state_name(discovering_state: &DpuDiscoveringState) -> &'static str {
            match discovering_state {
                DpuDiscoveringState::Initializing => "dpuinitializing",
                DpuDiscoveringState::Configuring => "dpuconfiguring",
                DpuDiscoveringState::DisableSecureBoot { .. } => "disablesecureboot",
                DpuDiscoveringState::EnableSecureBoot { .. } => "enablesecureboot",
                DpuDiscoveringState::SetUefiHttpBoot => "setuefihttpboot",
                DpuDiscoveringState::RebootAllDPUS => "rebootalldpus",
                DpuDiscoveringState::EnableRshim => "enablershim",
            }
        }

        fn instance_state_name(instance_state: &InstanceState) -> &'static str {
            match instance_state {
                InstanceState::Init => "init",
                InstanceState::WaitingForNetworkSegmentToBeReady => {
                    "waitingfornetworksegmenttobeready"
                }
                InstanceState::WaitingForNetworkConfig => "waitingfornetworkconfig",
                InstanceState::WaitingForStorageConfig => "waitingforstorageconfig",
                InstanceState::WaitingForExtensionServicesConfig => {
                    "waitingforextensionservicesconfig"
                }
                InstanceState::WaitingForRebootToReady => "waitingforreboottoready",
                InstanceState::Ready => "ready",
                InstanceState::BootingWithDiscoveryImage { .. } => "bootingwithdiscoveryimage",
                InstanceState::SwitchToAdminNetwork => "switchtoadminnetwork",
                InstanceState::WaitingForNetworkReconfig => "waitingfornetworkreconfig",
                InstanceState::DPUReprovision { .. } => "dpureprovisioning",
                InstanceState::Failed { .. } => "failed",
                InstanceState::HostReprovision { .. } => "hostreprovisioning",
                InstanceState::NetworkConfigUpdate { .. } => "networkconfigupdate",
                InstanceState::WaitingForDpusToUp => "waitingfordpustoup",
                InstanceState::HostPlatformConfiguration { .. } => "hostplatformconfiguration",
                InstanceState::DpaProvisioning => "dpaprovisioning",
                InstanceState::WaitingForDpaToBeReady => "waitingfordpatobeready",
            }
        }

        fn measuring_state_name(measuring_state: &MeasuringState) -> &'static str {
            match measuring_state {
                MeasuringState::WaitingForMeasurements => "waitingformeasurements",
                MeasuringState::PendingBundle => "pendingbundle",
            }
        }

        fn spdm_measuring_state_name(spdm_measuring_state: &SpdmMeasuringState) -> &'static str {
            match spdm_measuring_state {
                SpdmMeasuringState::TriggerMeasurements => "triggermeasurements",
                SpdmMeasuringState::PollResult => "pollresult",
            }
        }

        fn cleanup_state_name(cleanup_state: &CleanupState) -> &'static str {
            match cleanup_state {
                CleanupState::Init => "init",
                CleanupState::SecureEraseBoss { .. } => "secureeraseboss",
                CleanupState::HostCleanup { .. } => "hostcleanup",
                CleanupState::CreateBossVolume { .. } => "createbossvolume",
                CleanupState::DisableBIOSBMCLockdown => "disablebmclockdown",
            }
        }

        fn machine_validation_state_name(
            validation_state: &MachineValidatingState,
        ) -> &'static str {
            match validation_state {
                MachineValidatingState::MachineValidating { .. } => "machinevalidating",
                MachineValidatingState::RebootHost { .. } => "reboothost",
                MachineValidatingState::PrepareBootRepair { .. } => "preparebootrepair",
                MachineValidatingState::UnlockForBootRepair { .. } => "unlockforbootrepair",
                MachineValidatingState::CheckBootConfigForRepair { .. } => {
                    "checkbootconfigforrepair"
                }
                MachineValidatingState::ConfigureBootBios { .. } => "configurebootbios",
                MachineValidatingState::WaitingForBootBiosJob { .. } => "waitingforbootbiosjob",
                MachineValidatingState::PollingBootBiosSetup { .. } => "pollingbootbiossetup",
                MachineValidatingState::RepairBootConfig { .. } => "repairbootconfig",
                MachineValidatingState::LockAfterBootRepair { .. } => "lockafterbootrepair",
            }
        }

        fn ready_boot_config_state_name(state: &ReadyBootConfigState) -> &'static str {
            match state {
                ReadyBootConfigState::Prepare => "prepare",
                ReadyBootConfigState::UnlockHost { .. } => "unlockhost",
                ReadyBootConfigState::CheckHostConfig => "checkhostconfig",
                ReadyBootConfigState::ConfigureBios { .. } => "configurebios",
                ReadyBootConfigState::WaitingForBiosJob { .. } => "waitingforbiosjob",
                ReadyBootConfigState::PollingBiosSetup { .. } => "pollingbiossetup",
                ReadyBootConfigState::SetBootOrder { .. } => "setbootorder",
                ReadyBootConfigState::LockHost { .. } => "lockhost",
                ReadyBootConfigState::Failed { .. } => "failed",
            }
        }

        match state {
            ManagedHostState::ConfigureAstra {
                configure_astra_state,
            } => (
                "configureastra",
                match configure_astra_state {
                    ConfigureAstraState::EnableNics => "enablenics",
                    ConfigureAstraState::WaitingForPowercycle => "waitingforpowercycle",
                },
            ),
            ManagedHostState::DpuDiscoveringState { dpu_states } => {
                // Min state indicates the least processed DPU. The state machine is blocked
                // becasue of this.
                let dpu_state = dpu_states.states.values().min();
                let Some(dpu_state) = dpu_state else {
                    return ("unknown", "dpu");
                };
                ("dpudiscovering", discovering_state_name(dpu_state))
            }
            ManagedHostState::DPUInit { dpu_states } => {
                // Min state indicates the least processed DPU. The state machine is blocked
                // becasue of this.
                let dpu_state = dpu_states.states.values().min();
                let Some(dpu_state) = dpu_state else {
                    return ("unknown", "dpu");
                };
                ("dpunotready", dpuinit_state_name(dpu_state))
            }
            ManagedHostState::HostInit { machine_state } => {
                ("hostnotready", machine_state_name(machine_state))
            }
            ManagedHostState::Ready => ("ready", ""),
            ManagedHostState::Decommissioning {
                decommissioning_state,
            } => match decommissioning_state {
                DecommissioningState::SuppressingSiteExplorer => {
                    ("decommissioning", "suppressingsiteexplorer")
                }
                DecommissioningState::DeconfiguringHost { .. } => {
                    ("decommissioning", "deconfiguringhost")
                }
                DecommissioningState::DeconfiguringDpus { .. } => {
                    ("decommissioning", "deconfiguringdpus")
                }
                DecommissioningState::SuppressingOobDhcp => {
                    ("decommissioning", "suppressingoobdhcp")
                }
                DecommissioningState::PowerCyclingHost => ("decommissioning", "powercyclinghost"),
                DecommissioningState::PoweringOnHost => ("decommissioning", "poweringonhost"),
                DecommissioningState::WaitingForOobDhcpAcknowledgement => {
                    ("decommissioning", "waitingforoobdhcpacknowledgement")
                }
                DecommissioningState::SuppressingBmcDhcp => {
                    ("decommissioning", "suppressingbmcdhcp")
                }
                DecommissioningState::FactoryResettingBmcs { .. } => {
                    ("decommissioning", "factoryresettingbmcs")
                }
                DecommissioningState::WaitingForBmcDhcpAcknowledgement => {
                    ("decommissioning", "waitingforbmcdhcpacknowledgement")
                }
                DecommissioningState::DeletingManagedCredentials => {
                    ("decommissioning", "deletingmanagedcredentials")
                }
                DecommissioningState::Decommissioned => ("decommissioning", "decommissioned"),
            },
            ManagedHostState::BootConfiguring {
                boot_config_state, ..
            } => (
                "bootconfiguring",
                ready_boot_config_state_name(boot_config_state),
            ),
            ManagedHostState::Maintenance { operation } => {
                let op = match operation {
                    MachineMaintenanceOperation::PowerOn => "power_on",
                    MachineMaintenanceOperation::PowerOff => "power_off",
                    MachineMaintenanceOperation::Reset => "reset",
                };
                ("maintenance", op)
            }
            ManagedHostState::Assigned { instance_state } => {
                ("assigned", instance_state_name(instance_state))
            }
            ManagedHostState::WaitingForCleanup { cleanup_state, .. } => {
                ("waitingforcleanup", cleanup_state_name(cleanup_state))
            }
            ManagedHostState::Created => ("created", ""),
            ManagedHostState::ForceDeletion => ("forcedeletion", ""),
            ManagedHostState::Failed { .. } => ("failed", ""),
            ManagedHostState::DPUReprovision { .. } => ("reprovisioning", ""),
            ManagedHostState::HostReprovision { .. } => ("hostreprovisioning", ""),
            ManagedHostState::RotatingBmc { .. } => ("rotatingbmc", ""),
            ManagedHostState::RotatingHostUefi { .. } => ("rotatinghostuefi", ""),
            ManagedHostState::RotatingDpuUefi { .. } => ("rotatingdpuuefi", ""),
            ManagedHostState::RotatingNicLockdown => ("rotatingniclockdown", ""),
            ManagedHostState::Measuring { measuring_state } => {
                ("measuring", measuring_state_name(measuring_state))
            }
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state,
            } => (
                "preassignedmeasuringspdm",
                spdm_measuring_state_name(spdm_measuring_state),
            ),
            ManagedHostState::PostAssignedMeasuring { attestation_mode } => {
                match attestation_mode {
                    AttestationMode::MeasuredBoot { measuring_state } => (
                        "postassignedmeasuringmeasuredboot",
                        measuring_state_name(measuring_state),
                    ),
                    AttestationMode::SpdmAttestation {
                        spdm_measuring_state,
                    } => (
                        "postassignedmeasuringspdm",
                        spdm_measuring_state_name(spdm_measuring_state),
                    ),
                }
            }
            ManagedHostState::StartAssignmentCycle => ("startassignmentcycle", ""),
            ManagedHostState::BomValidating {
                bom_validating_state,
            } => match bom_validating_state {
                machine::BomValidating::MatchingSku(_) => ("bomvalidating", "matchingsku"),
                machine::BomValidating::UpdatingInventory(_) => {
                    ("bomvalidating", "updatinginventory")
                }
                machine::BomValidating::VerifyingSku(_) => ("bomvalidating", "verifyingsku"),
                machine::BomValidating::SkuVerificationFailed(_) => {
                    ("bomvalidating", "skuverificationfailed")
                }
                machine::BomValidating::WaitingForSkuAssignment(_) => {
                    ("bomvalidating", "waitingforskuassignment")
                }
                machine::BomValidating::SkuMissing(_) => ("bomvalidating", "skumissing"),
            },
            ManagedHostState::Validation { validation_state } => match validation_state {
                ValidationState::MachineValidation { machine_validation } => (
                    "validation",
                    machine_validation_state_name(machine_validation),
                ),
            },
        }
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        match state {
            // A Failed state carrying NoError is not actionable by an operator.
            ManagedHostState::Failed { details, .. }
                if details.cause != model::machine::FailureCause::NoError =>
            {
                Some(details.cause.metric_label())
            }
            ManagedHostState::BootConfiguring {
                boot_config_state: ReadyBootConfigState::Failed { .. },
                ..
            } => Some("boot_config_convergence_failed"),
            _ => None,
        }
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        object_state: &Self::State,
    ) -> StateSla {
        machine::state_sla(
            &object_state.host_snapshot.id,
            &state.value,
            &state.version,
            &object_state.aggregate_health,
            &self.sla_config,
        )
    }
}

#[cfg(test)]
mod tests {
    use model::machine_boot_interface::MachineBootInterfaceTarget;

    use super::*;

    #[test]
    fn terminal_ready_boot_config_requires_manual_intervention() {
        let state = ManagedHostState::BootConfiguring {
            desired_version: ConfigVersion::initial(),
            desired_boot_interface: MachineBootInterfaceTarget::MacOnly(
                "02:00:00:00:00:01".parse().unwrap(),
            ),
            post_lock_verification_retry_count: 0,
            boot_config_state: ReadyBootConfigState::Failed {
                failure: "exhausted".to_string(),
            },
        };

        assert_eq!(
            <MachineStateControllerIO as StateControllerIO>::manual_intervention_reason(&state),
            Some("boot_config_convergence_failed"),
        );
    }
}
