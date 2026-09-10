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

use carbide_uuid::machine::{DpuMachineId as MachineId, DpuMachineId};
use model::machine::{
    DpfState, DpuDiscoveringState, DpuDiscoveringStates, DpuInitNextStateResolver, DpuInitState,
    DpuInitStates, DpuMachine, DpuReprovisionStates, HostMachine, HostReprovisionState,
    InstallDpuOsState, InstanceNextStateResolver, InstanceState, MachineNextStateResolver,
    MachineState, ManagedHostState, ManagedHostStateSnapshot, ReprovisionState,
};
use state_controller::state_handler::StateHandlerError;

pub(super) trait NextState {
    fn next_bfb_install_state(
        &self,
        current_state: &ManagedHostState,
        install_os_substate: &InstallDpuOsState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError>;

    fn next_state(
        &self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
        host_snapshot: &HostMachine,
    ) -> Result<ManagedHostState, StateHandlerError>;

    fn next_state_with_all_dpus_updated(
        &self,
        state: &ManagedHostStateSnapshot,
        current_reprovision_state: &ReprovisionState,
    ) -> Result<ManagedHostState, StateHandlerError> {
        let dpu_ids_for_reprov = state
            .dpu_snapshots
            .iter()
            .filter(|machine| machine.reprovision_requested.is_some())
            .map(|dpu_machine| dpu_machine.id)
            .collect::<Vec<_>>();

        let all_machine_ids = state
            .dpu_snapshots
            .iter()
            .map(|dpu_machine| dpu_machine.id)
            .collect::<Vec<_>>();

        match current_reprovision_state {
            ReprovisionState::BmcFirmwareUpgrade { .. } => ReprovisionState::FirmwareUpgrade
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    // Mark all DPUs in PowerDown state.
                    dpu_ids_for_reprov,
                ),
            ReprovisionState::FirmwareUpgrade => ReprovisionState::WaitingForNetworkInstall
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    // Mark all DPUs in PowerDown state.
                    all_machine_ids,
                ),
            ReprovisionState::WaitingForNetworkInstall => ReprovisionState::PoweringOffHost
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
            ReprovisionState::PoweringOffHost => ReprovisionState::PowerDown
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    // Mark all DPUs in PowerDown state.
                    all_machine_ids,
                ),
            ReprovisionState::PowerDown => ReprovisionState::VerifyFirmareVersions
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    // Move only DPUs in WaitingForNetworkInstall for which reprovision is
                    // triggered.
                    dpu_ids_for_reprov,
                ),
            ReprovisionState::BufferTime => ReprovisionState::VerifyFirmareVersions
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    dpu_ids_for_reprov,
                ),
            ReprovisionState::WaitingForNetworkConfig => ReprovisionState::PrepareHostBootRepair
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    dpu_ids_for_reprov,
                ),
            ReprovisionState::SetHostBootOrder { .. } => ReprovisionState::LockHostAfterBootRepair
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
            ReprovisionState::LockHostAfterBootRepair => ReprovisionState::RebootHostBmc
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
            ReprovisionState::RebootHostBmc => ReprovisionState::RebootHost
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
            ReprovisionState::DpfStates { substate } => match substate {
                DpfState::WaitingForReady { .. }
                | DpfState::HandleReboot { .. }
                | DpfState::DeviceReady => ReprovisionState::PoweringOffHost
                    .next_state_with_all_dpus_updated(
                        &state.managed_state,
                        &state.dpu_snapshots,
                        all_machine_ids,
                    ),
                DpfState::Reprovisioning => ReprovisionState::DpfStates {
                    substate: DpfState::Provisioning,
                }
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
                DpfState::Provisioning => ReprovisionState::DpfStates {
                    substate: DpfState::WaitingForReady { phase_detail: None },
                }
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
                DpfState::Unknown => ReprovisionState::DpfStates {
                    substate: DpfState::Provisioning,
                }
                .next_state_with_all_dpus_updated(
                    &state.managed_state,
                    &state.dpu_snapshots,
                    all_machine_ids,
                ),
            },
            _ => Err(StateHandlerError::InvalidState(format!(
                "Unhandled {current_reprovision_state} state for all dpu handling."
            ))),
        }
    }
}

pub(super) trait DpuDiscoveringStateHelper {
    fn next_state(
        self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError>;
}

impl DpuDiscoveringStateHelper for DpuDiscoveringState {
    fn next_state(
        self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError> {
        match current_state {
            ManagedHostState::DpuDiscoveringState { dpu_states } => {
                let mut states = dpu_states.states.clone();
                let entry = states.entry(*dpu_id).or_insert(self.clone());
                *entry = self;

                Ok(ManagedHostState::DpuDiscoveringState {
                    dpu_states: DpuDiscoveringStates { states },
                })
            }
            _ => Err(StateHandlerError::InvalidState(
                "Invalid State passed to DpuDiscoveringState::next_state.".to_string(),
            )),
        }
    }
}

pub(super) trait DpuInitStateHelper {
    fn next_state(
        self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError>;

    fn next_state_with_all_dpus_updated(
        self,
        current_state: &ManagedHostState,
    ) -> Result<ManagedHostState, StateHandlerError>;
}

impl DpuInitStateHelper for DpuInitState {
    fn next_state(
        self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError> {
        match current_state {
            ManagedHostState::DPUInit { dpu_states } => {
                let mut states = dpu_states.states.clone();
                let entry = states.entry(*dpu_id).or_insert(self.clone());
                *entry = self;

                Ok(ManagedHostState::DPUInit {
                    dpu_states: DpuInitStates { states },
                })
            }

            ManagedHostState::DpuDiscoveringState { dpu_states } => {
                // All DPUs must be moved to same DPUInit state.
                let states = dpu_states
                    .states
                    .keys()
                    .map(|x| (*x, self.clone()))
                    .collect::<HashMap<MachineId, DpuInitState>>();
                Ok(ManagedHostState::DPUInit {
                    dpu_states: DpuInitStates { states },
                })
            }

            _ => Err(StateHandlerError::InvalidState(format!(
                "Invalid State passed to DpuNotReady::next_state. Current state: {current_state:?}."
            ))),
        }
    }

    fn next_state_with_all_dpus_updated(
        self,
        current_state: &ManagedHostState,
    ) -> Result<ManagedHostState, StateHandlerError> {
        match current_state {
            ManagedHostState::DPUInit { dpu_states } => {
                let states = dpu_states
                    .states
                    .keys()
                    .map(|x| (*x, self.clone()))
                    .collect::<HashMap<MachineId, DpuInitState>>();

                Ok(ManagedHostState::DPUInit {
                    dpu_states: DpuInitStates { states },
                })
            }
            ManagedHostState::DpuDiscoveringState { dpu_states } => {
                // All DPUs must be moved to same DPUInit state.
                let states = dpu_states
                    .states
                    .keys()
                    .map(|x| (*x, self.clone()))
                    .collect::<HashMap<MachineId, DpuInitState>>();
                Ok(ManagedHostState::DPUInit {
                    dpu_states: DpuInitStates { states },
                })
            }
            _ => Err(StateHandlerError::InvalidState(
                "Invalid State passed to DpuNotReady::next_state_all_dpu.".to_string(),
            )),
        }
    }
}

impl NextState for MachineNextStateResolver {
    fn next_state(
        &self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
        _host_snapshot: &HostMachine,
    ) -> Result<ManagedHostState, StateHandlerError> {
        let reprovision_state = current_state.as_reprovision_state(dpu_id).ok_or_else(|| {
            StateHandlerError::MissingData {
                object_id: dpu_id.to_string(),
                missing: "dpu_state",
            }
        })?;

        let mut dpu_states = match current_state {
            ManagedHostState::DPUReprovision { dpu_states } => dpu_states.states.clone(),
            _ => {
                return Err(StateHandlerError::InvalidState(format!(
                    "Unhandled {current_state} state for Machine handling."
                )));
            }
        };

        match reprovision_state {
            ReprovisionState::RebootHost => Ok(ManagedHostState::HostInit {
                machine_state: MachineState::Discovered {
                    skip_reboot_wait: false,
                },
            }),
            ReprovisionState::VerifyFirmareVersions => {
                dpu_states.insert(*dpu_id, ReprovisionState::WaitingForNetworkConfig);
                Ok(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states: dpu_states },
                })
            }
            _ => Err(StateHandlerError::InvalidState(format!(
                "Unhandled {reprovision_state} state for Non-Instance handling."
            ))),
        }
    }

    fn next_bfb_install_state(
        &self,
        current_state: &ManagedHostState,
        install_os_substate: &InstallDpuOsState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError> {
        let mut dpu_states = match current_state {
            ManagedHostState::DPUReprovision { dpu_states } => dpu_states.states.clone(),
            _ => {
                return Err(StateHandlerError::InvalidState(format!(
                    "Unhandled {current_state} state for Non-Instance handling."
                )));
            }
        };
        match install_os_substate {
            InstallDpuOsState::Completed => {
                dpu_states.insert(*dpu_id, ReprovisionState::WaitingForNetworkInstall);
                Ok(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states: dpu_states },
                })
            }
            _ => {
                dpu_states.insert(
                    *dpu_id,
                    ReprovisionState::InstallDpuOs {
                        substate: install_os_substate.clone(),
                    },
                );
                Ok(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states: dpu_states },
                })
            }
        }
    }
}

impl NextState for InstanceNextStateResolver {
    fn next_state(
        &self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
        host_snapshot: &HostMachine,
    ) -> Result<ManagedHostState, StateHandlerError> {
        let reprovision_state = current_state.as_reprovision_state(dpu_id).ok_or_else(|| {
            StateHandlerError::MissingData {
                object_id: dpu_id.to_string(),
                missing: "dpu_state",
            }
        })?;

        let mut dpu_states = match current_state {
            ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision { dpu_states },
            } => dpu_states.states.clone(),
            _ => {
                return Err(StateHandlerError::InvalidState(format!(
                    "Unhandled {current_state} state for Instance handling."
                )));
            }
        };

        match reprovision_state {
            ReprovisionState::RebootHost => {
                if host_snapshot.host_reprovision_requested.is_some() {
                    Ok(ManagedHostState::Assigned {
                        instance_state: InstanceState::HostReprovision {
                            reprovision_state: HostReprovisionState::CheckingFirmwareV2 {
                                firmware_type: None,
                                firmware_number: None,
                            },
                        },
                    })
                } else {
                    Ok(ManagedHostState::Assigned {
                        instance_state: InstanceState::Ready,
                    })
                }
            }
            ReprovisionState::VerifyFirmareVersions => {
                dpu_states.insert(*dpu_id, ReprovisionState::WaitingForNetworkConfig);
                Ok(ManagedHostState::Assigned {
                    instance_state: InstanceState::DPUReprovision {
                        dpu_states: DpuReprovisionStates { states: dpu_states },
                    },
                })
            }
            _ => Err(StateHandlerError::InvalidState(format!(
                "Unhandled {reprovision_state} state for Instance handling."
            ))),
        }
    }

    fn next_bfb_install_state(
        &self,
        current_state: &ManagedHostState,
        install_os_substate: &InstallDpuOsState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError> {
        let mut dpu_states = match current_state {
            ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision { dpu_states },
            } => dpu_states.states.clone(),
            _ => {
                return Err(StateHandlerError::InvalidState(format!(
                    "Unhandled {current_state} state for Instance handling."
                )));
            }
        };
        match install_os_substate {
            InstallDpuOsState::Completed => {
                dpu_states.insert(*dpu_id, ReprovisionState::WaitingForNetworkInstall);
                Ok(ManagedHostState::Assigned {
                    instance_state: InstanceState::DPUReprovision {
                        dpu_states: DpuReprovisionStates { states: dpu_states },
                    },
                })
            }
            _ => {
                dpu_states.insert(
                    *dpu_id,
                    ReprovisionState::InstallDpuOs {
                        substate: install_os_substate.clone(),
                    },
                );
                Ok(ManagedHostState::Assigned {
                    instance_state: InstanceState::DPUReprovision {
                        dpu_states: DpuReprovisionStates { states: dpu_states },
                    },
                })
            }
        }
    }
}

impl NextState for DpuInitNextStateResolver {
    fn next_state(
        &self,
        current_state: &ManagedHostState,
        dpu_id: &MachineId,
        _host_snapshot: &HostMachine,
    ) -> Result<ManagedHostState, StateHandlerError> {
        DpuInitState::Init.next_state(current_state, dpu_id)
    }

    fn next_bfb_install_state(
        &self,
        current_state: &ManagedHostState,
        install_os_substate: &InstallDpuOsState,
        dpu_id: &MachineId,
    ) -> Result<ManagedHostState, StateHandlerError> {
        match install_os_substate {
            // Move to DpuInit state
            InstallDpuOsState::Completed => DpuInitState::Init.next_state(current_state, dpu_id),
            _ => Ok(DpuInitState::InstallDpuOs {
                substate: install_os_substate.clone(),
            }
            .next_state(current_state, dpu_id)?),
        }
    }
}

pub(super) trait ReprovisionStateHelper {
    fn next_state_with_all_dpus_updated(
        self,
        current_state: &ManagedHostState,
        dpu_snapshots: &[DpuMachine],
        dpu_ids_to_process: Vec<DpuMachineId>,
    ) -> Result<ManagedHostState, StateHandlerError>;
}

impl ReprovisionStateHelper for ReprovisionState {
    // This is normal case when user wants to reprovision only one DPU. In this condition, this
    // function will update state only for those DPU for which reprovision is triggered. Reset will
    // be updated as NotUnderReprovision state.
    fn next_state_with_all_dpus_updated(
        self,
        current_state: &ManagedHostState,
        dpu_snapshots: &[DpuMachine],
        dpu_ids_to_process: Vec<DpuMachineId>,
    ) -> Result<ManagedHostState, StateHandlerError> {
        match current_state {
            ManagedHostState::Ready => {
                let states = dpu_snapshots
                    .iter()
                    .map(|x| {
                        let dpu_id = x.id;
                        (
                            dpu_id,
                            if dpu_ids_to_process.contains(&dpu_id) {
                                self.clone()
                            } else {
                                ReprovisionState::NotUnderReprovision
                            },
                        )
                    })
                    .collect::<HashMap<MachineId, ReprovisionState>>();

                Ok(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states },
                })
            }
            ManagedHostState::DPUReprovision { dpu_states: _ } => {
                let states = dpu_snapshots
                    .iter()
                    .map(|x| {
                        let dpu_id = x.id;
                        (
                            dpu_id,
                            if dpu_ids_to_process.contains(&dpu_id) {
                                self.clone()
                            } else {
                                ReprovisionState::NotUnderReprovision
                            },
                        )
                    })
                    .collect::<HashMap<MachineId, ReprovisionState>>();
                Ok(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates { states },
                })
            }
            ManagedHostState::Assigned { instance_state } => match instance_state {
                InstanceState::DPUReprovision { .. }
                | InstanceState::BootingWithDiscoveryImage { .. }
                | InstanceState::Failed { .. } => {
                    let states = dpu_snapshots
                        .iter()
                        .map(|x| {
                            let dpu_id = x.id;
                            (
                                dpu_id,
                                if dpu_ids_to_process.contains(&dpu_id) {
                                    self.clone()
                                } else {
                                    ReprovisionState::NotUnderReprovision
                                },
                            )
                        })
                        .collect::<HashMap<MachineId, ReprovisionState>>();

                    Ok(ManagedHostState::Assigned {
                        instance_state: InstanceState::DPUReprovision {
                            dpu_states: DpuReprovisionStates { states },
                        },
                    })
                }

                _ => Err(StateHandlerError::InvalidState(format!(
                    "Invalid State {current_state:?} passed to Reprovision::Assigned::next_state_with_all_dpus."
                ))),
            },
            _ => Err(StateHandlerError::InvalidState(format!(
                "Invalid State {current_state:?} passed to Reprovision::next_state_with_all_dpus."
            ))),
        }
    }
}

pub(super) trait ManagedHostStateHelper {
    fn all_dpu_states_in_sync(&self) -> Result<bool, StateHandlerError>;
}

impl ManagedHostStateHelper for ManagedHostState {
    fn all_dpu_states_in_sync(&self) -> Result<bool, StateHandlerError> {
        match self {
            // Don't now why but if I use itertools::Itertools in header, EnumIter creates problem.
            ManagedHostState::DpuDiscoveringState { dpu_states } => all_equal(
                &itertools::Itertools::collect_vec(dpu_states.states.values()),
            ),
            ManagedHostState::DPUInit { dpu_states } => all_equal(
                &itertools::Itertools::collect_vec(dpu_states.states.values()),
            ),
            ManagedHostState::DPUReprovision { dpu_states } => {
                reprovision_dpf_states_in_sync(dpu_states)
            }
            ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision { dpu_states },
            } => reprovision_dpf_states_in_sync(dpu_states),
            _ => Ok(true),
        }
    }
}

/// Check whether all DPUs under active DPF reprovisioning have the same
/// sub-state. `NotUnderReprovision` DPUs are excluded because they were never
/// requested for reprovisioning and do not participate in the DPF flow.
/// Including them would cause `all_equal` to always return `false` (e.g.
/// `DeviceReady` vs `NotUnderReprovision`), permanently blocking the sync
/// barrier even though those DPUs have nothing to wait on.
fn reprovision_dpf_states_in_sync(
    dpu_states: &DpuReprovisionStates,
) -> Result<bool, StateHandlerError> {
    all_equal(&itertools::Itertools::collect_vec(
        dpu_states
            .states
            .values()
            .filter(|s| !matches!(s, ReprovisionState::NotUnderReprovision)),
    ))
}

pub(super) fn all_equal<A>(states: &[A]) -> Result<bool, StateHandlerError>
where
    A: PartialEq,
{
    let Some(first) = states.first() else {
        return Err(StateHandlerError::MissingData {
            object_id: "NA".to_string(),
            missing: "DPU states.",
        });
    };

    Ok(states.iter().all(|x| x == first))
}
