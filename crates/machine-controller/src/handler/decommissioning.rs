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

use std::collections::{HashMap, HashSet};

use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, CredentialWriter};
use carbide_uuid::machine::MachineId;
use libredfish::model::task::TaskState;
use libredfish::model::update_service::TransferProtocolType;
use libredfish::{EnabledDisabled, JobState, PowerState, RedfishError, SystemPowerControl};
use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};
use model::dpa_interface::{DpaInterfaceControllerState, DpaInterfaceType, DpaLockMode};
use model::machine::{
    DecommissioningState, DeconfiguringDpuState, DeconfiguringHostState, ManagedHostState,
    ManagedHostStateSnapshot,
};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;
use crate::dpf::{DpfOperations, dpf_dpudevices_and_dpunode_crs_noexist};
use crate::redfish::{did_dpu_finish_booting, host_power_control};

fn deconfiguring_dpus_after_host_reset(state: &ManagedHostStateSnapshot) -> ManagedHostState {
    ManagedHostState::Decommissioning {
        decommissioning_state: DecommissioningState::DeconfiguringDpus {
            dpu_states: state
                .dpu_snapshots
                .iter()
                .map(|dpu| {
                    (
                        dpu.id,
                        if state.host_snapshot.config.dpf.used_for_ingestion {
                            DeconfiguringDpuState::DeletingFromDpf
                        } else {
                            DeconfiguringDpuState::InstallingBfb
                        },
                    )
                })
                .collect(),
        },
    }
}

pub(super) async fn handle_suppressing_site_explorer(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let machine_id = state.host_snapshot.id;
    let mut bmc_mac_addresses = Vec::with_capacity(state.dpu_snapshots.len() + 1);
    for machine in std::iter::once(&state.host_snapshot).chain(&state.dpu_snapshots) {
        let bmc_mac_address =
            machine
                .status
                .bmc_info
                .mac
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: machine.id.to_string(),
                    missing: "bmc_mac",
                })?;
        bmc_mac_addresses.push(bmc_mac_address);
    }

    let mut txn = ctx.services.db_pool.begin().await?;
    let mut pending_macs = Vec::new();
    for bmc_mac_address in bmc_mac_addresses {
        let suppression = db::bmc_suppression::upsert(
            &mut txn,
            &NewBmcSuppression {
                bmc_mac_address,
                subsystem: BmcSuppressionSubsystem::SiteExplorer,
                reason: format!("managed host {machine_id} is being decommissioned"),
            },
        )
        .await?;
        if suppression.acknowledged_at.is_none() {
            pending_macs.push(bmc_mac_address);
        }
    }

    let outcome = if pending_macs.is_empty() {
        StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::DeconfiguringHost {
                deconfiguring_state: DeconfiguringHostState::DisableLockdown,
            },
        })
    } else {
        StateHandlerOutcome::wait(format!(
            "waiting for Site Explorer suppression acknowledgement: {}",
            pending_macs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };

    Ok(outcome.with_txn(txn))
}

pub(super) async fn handle_deconfiguring_host(
    deconfiguring_state: &DeconfiguringHostState,
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let machine = &state.host_snapshot;
    match deconfiguring_state {
        DeconfiguringHostState::DisableLockdown => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            match redfish_client.lockdown_bmc(EnabledDisabled::Disabled).await {
                Ok(()) | Err(RedfishError::NotSupported(_)) => {}
                Err(error) => {
                    return Err(StateHandlerError::GenericError(eyre::eyre!(
                        "failed to disable host BMC lockdown: {error}"
                    )));
                }
            }

            let next = if machine.bmc_vendor().is_supermicro() {
                DeconfiguringHostState::RebootAfterLockdown
            } else {
                DeconfiguringHostState::ClearSuperNicLockdown
            };
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: next,
                    },
                },
            ))
        }
        DeconfiguringHostState::RebootAfterLockdown => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            host_power_control(
                redfish_client.as_ref(),
                machine,
                SystemPowerControl::ForceRestart,
                ctx,
            )
            .await
            .map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to reboot host after disabling BMC lockdown: {error}"
                ))
            })?;
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: DeconfiguringHostState::ClearSuperNicLockdown,
                    },
                },
            ))
        }
        DeconfiguringHostState::ClearUefiPassword => {
            if machine.bios_password_set_time.is_none() {
                return Ok(StateHandlerOutcome::transition(
                    deconfiguring_dpus_after_host_reset(state),
                ));
            }

            let bmc_access_info = ctx.services.bmc_access_info_for_machine(machine).await?;

            let bmc_mac =
                machine
                    .status
                    .bmc_info
                    .mac
                    .ok_or_else(|| StateHandlerError::MissingData {
                        object_id: machine.id.to_string(),
                        missing: "bmc_mac",
                    })?;
            let mut conn = ctx.services.db_pool.acquire().await?;
            let status = db::credential_rotation::device_rotation_status(
                &mut conn,
                db::credential_rotation::CredentialRotationType::HostUefi,
                bmc_mac,
            )
            .await?
            .ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "host UEFI credential version is not recorded for {bmc_mac}"
                ))
            })?;
            let version = status.current_version.ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "host UEFI credential version is not established for {bmc_mac}"
                ))
            })?;
            let version = u32::try_from(version).map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "invalid host UEFI credential version {version}: {error}"
                ))
            })?;
            drop(conn);

            let key = CredentialKey::host_uefi_site_default(version);
            let credentials = ctx
                .services
                .bmc_credential_ops
                .credential_reader()
                .get_credentials(&key)
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to read host UEFI credential: {error}"
                    ))
                })?
                .ok_or_else(|| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "host UEFI credential {key:?} is not set"
                    ))
                })?;

            let job_id = ctx
                .services
                .bmc_credential_ops
                .clear_host_uefi_password(&bmc_access_info, credentials)
                .await
                .map_err(|error| match error {
                    // Client creation failed (credential store, TCP, or the
                    // vendor probe): propagate as-is so the framework
                    // retries normally.
                    carbide_redfish::libredfish::CredentialOpError::ClientCreation(e) => e.into(),
                    error => StateHandlerError::GenericError(eyre::eyre!(
                        "failed to clear host UEFI password: {error}"
                    )),
                })?;

            let next = match job_id {
                Some(job_id) => DeconfiguringHostState::WaitForUefiPasswordJobScheduled { job_id },
                None => {
                    return Ok(StateHandlerOutcome::transition(
                        deconfiguring_dpus_after_host_reset(state),
                    ));
                }
            };
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: next,
                    },
                },
            ))
        }
        DeconfiguringHostState::WaitForUefiPasswordJobScheduled { job_id } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            let job_state = redfish_client
                .get_job_state(job_id)
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to read UEFI password job {job_id}: {error}"
                    ))
                })?;
            if !matches!(job_state, JobState::Scheduled) {
                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for UEFI password job {job_id} to be scheduled; current state: {job_state:?}"
                )));
            }
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: DeconfiguringHostState::RebootAfterUefiPassword {
                            job_id: job_id.clone(),
                        },
                    },
                },
            ))
        }
        DeconfiguringHostState::RebootAfterUefiPassword { job_id } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            host_power_control(
                redfish_client.as_ref(),
                machine,
                SystemPowerControl::ForceRestart,
                ctx,
            )
            .await
            .map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to reboot host for UEFI password job {job_id}: {error}"
                ))
            })?;
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state:
                            DeconfiguringHostState::WaitForUefiPasswordJobCompletion {
                                job_id: job_id.clone(),
                            },
                    },
                },
            ))
        }
        DeconfiguringHostState::WaitForUefiPasswordJobCompletion { job_id } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            let job_state = redfish_client
                .get_job_state(job_id)
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to read UEFI password job {job_id}: {error}"
                    ))
                })?;
            if !matches!(job_state, JobState::Completed) {
                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for UEFI password job {job_id} to complete; current state: {job_state:?}"
                )));
            }
            let mut txn = ctx.services.db_pool.begin().await?;
            db::machine::clear_bios_password_set_time(&machine.id, &mut txn).await?;
            Ok(
                StateHandlerOutcome::transition(deconfiguring_dpus_after_host_reset(state))
                    .with_txn(txn),
            )
        }
        DeconfiguringHostState::ClearSuperNicLockdown => {
            let super_nics = state
                .dpa_interface_snapshots
                .iter()
                .filter(|interface| interface.interface_type == DpaInterfaceType::Svpc)
                .collect::<Vec<_>>();
            if super_nics.is_empty() {
                return Ok(StateHandlerOutcome::transition(
                    ManagedHostState::Decommissioning {
                        decommissioning_state: DecommissioningState::DeconfiguringHost {
                            deconfiguring_state: DeconfiguringHostState::ResetUefiSettings,
                        },
                    },
                ));
            }

            let mut txn = ctx.services.db_pool.begin().await?;
            for interface in super_nics {
                db::dpa_interface::try_update_controller_state(
                    &mut txn,
                    interface.id,
                    interface.controller_state.version,
                    interface.controller_state.version.increment(),
                    &DpaInterfaceControllerState::Unlocking,
                )
                .await?;
            }
            Ok(
                StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: DeconfiguringHostState::WaitForSuperNicLockdown,
                    },
                })
                .with_txn(txn),
            )
        }
        DeconfiguringHostState::WaitForSuperNicLockdown => {
            let unlocked = state
                .dpa_interface_snapshots
                .iter()
                .filter(|interface| interface.interface_type == DpaInterfaceType::Svpc)
                .all(|interface| {
                    interface
                        .card_state
                        .as_ref()
                        .and_then(|card| card.lockmode.clone())
                        == Some(DpaLockMode::Unlocked)
                });
            if !unlocked {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for all SuperNICs to report unlocked".to_string(),
                ));
            }
            // Every NIC is now unlocked. Record the unlock against each
            // card's NIC-MAC-keyed lockdown_ikm row.
            let mut txn = ctx.services.db_pool.begin().await?;
            for interface in state
                .dpa_interface_snapshots
                .iter()
                .filter(|interface| interface.interface_type == DpaInterfaceType::Svpc)
            {
                db::credential_rotation::record_device_unlocked(
                    txn.as_mut(),
                    interface.mac_address,
                    db::credential_rotation::CredentialRotationType::LockdownIkm,
                )
                .await
                .map_err(|e| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "record decommission unlock for {}: {e}",
                        interface.mac_address
                    ))
                })?;
            }
            Ok(
                StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: DeconfiguringHostState::ResetUefiSettings,
                    },
                })
                .with_txn(txn),
            )
        }
        DeconfiguringHostState::ResetUefiSettings => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(machine)
                .await?;
            redfish_client.reset_bios().await.map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to reset host UEFI settings: {error}"
                ))
            })?;
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringHost {
                        deconfiguring_state: DeconfiguringHostState::ClearUefiPassword,
                    },
                },
            ))
        }
    }
}

pub(super) async fn handle_deconfiguring_dpus(
    dpu_states: &HashMap<MachineId, DeconfiguringDpuState>,
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: Option<&dyn DpfOperations>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    if dpu_states.is_empty() {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::SuppressingOobDhcp,
            },
        ));
    }

    let Some((&dpu_id, dpu_state)) = dpu_states
        .iter()
        .find(|(_, dpu_state)| !matches!(dpu_state, DeconfiguringDpuState::Complete))
    else {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::SuppressingOobDhcp,
            },
        ));
    };
    let dpu = state
        .dpu_snapshots
        .iter()
        .find(|dpu| dpu.id == dpu_id)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: state.host_snapshot.id.to_string(),
            missing: "dpu_snapshot",
        })?;
    let mut next_states = dpu_states.clone();

    match dpu_state {
        DeconfiguringDpuState::DeletingFromDpf => {
            let dpf_sdk = dpf_sdk.ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "managed host {} was provisioned by DPF, but DPF is not configured",
                    state.host_snapshot.id
                ))
            })?;
            let host_dpf_id =
                state
                    .host_snapshot
                    .dpf_id()
                    .ok_or_else(|| StateHandlerError::MissingData {
                        object_id: state.host_snapshot.id.to_string(),
                        missing: "dpf_id",
                    })?;
            let dpu_dpf_ids = state
                .dpu_snapshots
                .iter()
                .map(|dpu| {
                    dpu.dpf_id().ok_or_else(|| StateHandlerError::MissingData {
                        object_id: dpu.id.to_string(),
                        missing: "dpf_id",
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            dpf_sdk
                .force_delete_host(&host_dpf_id, &dpu_dpf_ids)
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to delete managed host {} from DPF: {error}",
                        state.host_snapshot.id
                    ))
                })?;
            if !dpf_dpudevices_and_dpunode_crs_noexist(state, dpf_sdk)
                .await
                .map_err(|error| StateHandlerError::GenericError(error.into()))?
            {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for managed host DPF resources to be deleted".to_string(),
                ));
            }
            let next_states = dpu_states
                .keys()
                .map(|&id| (id, DeconfiguringDpuState::InstallingBfb))
                .collect();
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringDpus {
                        dpu_states: next_states,
                    },
                },
            ))
        }
        DeconfiguringDpuState::InstallingBfb => {
            let redfish_client = ctx.services.create_redfish_client_from_machine(dpu).await?;
            let task = redfish_client
                .update_firmware_simple_update(
                    "carbide-pxe.forge//public/blobs/internal/aarch64/preingestion.bfb",
                    vec!["redfish/v1/UpdateService/FirmwareInventory/DPU_OS".to_string()],
                    TransferProtocolType::HTTP,
                )
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to install vanilla BFB on DPU {dpu_id}: {error}"
                    ))
                })?;
            next_states.insert(
                dpu_id,
                DeconfiguringDpuState::WaitForInstallComplete { task_id: task.id },
            );
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringDpus {
                        dpu_states: next_states,
                    },
                },
            ))
        }
        DeconfiguringDpuState::WaitForInstallComplete { task_id } => {
            let redfish_client = ctx.services.create_redfish_client_from_machine(dpu).await?;
            let task = redfish_client.get_task(task_id).await.map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to verify vanilla BFB install task {task_id} on DPU {dpu_id}: {error}"
                ))
            })?;
            match task.task_state {
                Some(TaskState::Completed) => {
                    next_states
                        .insert(dpu_id, DeconfiguringDpuState::WaitingForBootAfterBfbInstall);
                    Ok(StateHandlerOutcome::transition(
                        ManagedHostState::Decommissioning {
                            decommissioning_state: DecommissioningState::DeconfiguringDpus {
                                dpu_states: next_states,
                            },
                        },
                    ))
                }
                Some(TaskState::Running | TaskState::New | TaskState::Starting) => {
                    Ok(StateHandlerOutcome::wait(format!(
                        "waiting for vanilla BFB install on DPU {dpu_id} to complete: {}%",
                        task.percent_complete.unwrap_or_default()
                    )))
                }
                task_state => Err(StateHandlerError::GenericError(eyre::eyre!(
                    "vanilla BFB install task {task_id} on DPU {dpu_id} failed with state {task_state:?}"
                ))),
            }
        }
        DeconfiguringDpuState::WaitingForBootAfterBfbInstall => {
            let redfish_client = ctx.services.create_redfish_client_from_machine(dpu).await?;
            let (finished_booting, boot_progress) = did_dpu_finish_booting(redfish_client.as_ref())
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to check whether DPU {dpu_id} finished booting after vanilla BFB installation: {error}"
                    ))
                })?;
            if !finished_booting {
                return Ok(StateHandlerOutcome::wait(format!(
                    "waiting for DPU {dpu_id} to finish booting after vanilla BFB installation: {boot_progress:?}"
                )));
            }
            next_states.insert(dpu_id, DeconfiguringDpuState::Complete);
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::Decommissioning {
                    decommissioning_state: DecommissioningState::DeconfiguringDpus {
                        dpu_states: next_states,
                    },
                },
            ))
        }
        DeconfiguringDpuState::Complete => unreachable!("complete DPU states are skipped"),
    }
}

async fn unacknowledged_dhcp_suppression_macs(
    mac_addresses: impl IntoIterator<Item = mac_address::MacAddress>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Vec<mac_address::MacAddress>, StateHandlerError> {
    let mut pending = Vec::new();
    for mac_address in mac_addresses {
        let suppression = db::bmc_suppression::find(
            &ctx.services.db_pool,
            mac_address,
            BmcSuppressionSubsystem::Dhcp,
        )
        .await?;
        if suppression.is_none_or(|suppression| suppression.acknowledged_at.is_none()) {
            pending.push(mac_address);
        }
    }
    Ok(pending)
}

fn bmc_mac_addresses(
    state: &ManagedHostStateSnapshot,
) -> Result<Vec<mac_address::MacAddress>, StateHandlerError> {
    std::iter::once(&state.host_snapshot)
        .chain(&state.dpu_snapshots)
        .map(|machine| {
            machine
                .status
                .bmc_info
                .mac
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: machine.id.to_string(),
                    missing: "bmc_mac",
                })
        })
        .collect()
}

async fn oob_interface_mac_addresses(
    machine_id: MachineId,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Vec<mac_address::MacAddress>, StateHandlerError> {
    let mut conn = ctx.services.db_pool.acquire().await?;
    let interfaces = db::machine_interface::find_by_machine_ids(&mut conn, &[machine_id]).await?;
    Ok(interfaces
        .into_values()
        .flatten()
        // Underlay can include BMC interface rows; keep those for the later
        // SuppressingBmcDhcp step so BMC DHCP is not blocked before factory reset.
        .filter(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Underlay)
                && interface.interface_type != InterfaceType::Bmc
        })
        .map(|interface| interface.mac_address)
        .collect())
}

async fn all_oob_interface_mac_addresses(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Vec<mac_address::MacAddress>, StateHandlerError> {
    let mut mac_addresses = Vec::new();
    for machine in std::iter::once(&state.host_snapshot).chain(&state.dpu_snapshots) {
        mac_addresses.extend(oob_interface_mac_addresses(machine.id, ctx).await?);
    }
    Ok(mac_addresses)
}

pub(super) async fn handle_suppressing_oob_dhcp(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let oob_mac_addresses = all_oob_interface_mac_addresses(state, ctx).await?;
    let mut txn = ctx.services.db_pool.begin().await?;
    for mac_address in oob_mac_addresses {
        db::bmc_suppression::upsert(
            &mut txn,
            &NewBmcSuppression {
                bmc_mac_address: mac_address,
                subsystem: BmcSuppressionSubsystem::Dhcp,
                reason: format!(
                    "managed host {} is being decommissioned; suppressing OOB DHCP",
                    state.host_snapshot.id
                ),
            },
        )
        .await?;
    }
    // Not deleting interface rows: bumps machine_interfaces_deletion so the
    // DHCP server restarts, clears its cache, and honors the suppressions above.
    db::machine_interface::record_deletion(&mut txn).await?;
    Ok(
        StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::PowerCyclingHost,
        })
        .with_txn(txn),
    )
}

pub(super) async fn handle_power_cycling_host(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host = &state.host_snapshot;
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(host)
        .await?;
    host_power_control(
        redfish_client.as_ref(),
        host,
        SystemPowerControl::ACPowercycle,
        ctx,
    )
    .await
    .map_err(|error| {
        StateHandlerError::GenericError(eyre::eyre!(
            "failed to power cycle host after suppressing OOB DHCP: {error}"
        ))
    })?;
    Ok(StateHandlerOutcome::transition(
        ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::PoweringOnHost,
        },
    ))
}

pub(super) async fn handle_powering_on_host(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    power_down_wait: chrono::Duration,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host = &state.host_snapshot;
    let basetime = host
        .status
        .last_reboot_requested
        .as_ref()
        .map(|reboot| reboot.time)
        .unwrap_or(host.state.version.timestamp());

    if super::wait(&basetime, power_down_wait) {
        return Ok(StateHandlerOutcome::wait(format!(
            "waiting for power-down grace period before powering on {host_id}; power_down_wait: {power_down_wait}",
            host_id = host.id,
        )));
    }

    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(host)
        .await?;
    let power_state = super::host_power_state(redfish_client.as_ref()).await?;
    match power_state {
        PowerState::On => Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::WaitingForOobDhcpAcknowledgement,
            },
        )),
        PowerState::PoweringOff => Ok(StateHandlerOutcome::wait(format!(
            "waiting for {host_id} to finish powering off before powering on; current power state: {power_state}",
            host_id = host.id,
        ))),
        PowerState::PoweringOn => Ok(StateHandlerOutcome::wait(format!(
            "waiting for {host_id} to finish powering on; current power state: {power_state}",
            host_id = host.id,
        ))),
        _ => {
            tracing::info!(
                machine_id = %host.id,
                %power_state,
                "Host not On after decommissioning power cycle; powering on"
            );
            host_power_control(redfish_client.as_ref(), host, SystemPowerControl::On, ctx)
                .await
                .map_err(|error| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "failed to power on host after decommissioning power cycle: {error}"
                    ))
                })?;
            Ok(StateHandlerOutcome::wait(format!(
                "waiting for {host_id} to power on after decommissioning power cycle; current power state: {power_state}",
                host_id = host.id,
            )))
        }
    }
}

pub(super) async fn handle_waiting_for_oob_dhcp_acknowledgement(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let oob_mac_addresses = all_oob_interface_mac_addresses(state, ctx).await?;
    let pending_macs = unacknowledged_dhcp_suppression_macs(oob_mac_addresses, ctx).await?;
    if pending_macs.is_empty() {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::SuppressingBmcDhcp,
            },
        ));
    }
    Ok(StateHandlerOutcome::wait(format!(
        "waiting for OOB DHCP suppression acknowledgement: {}",
        pending_macs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(super) async fn handle_suppressing_bmc_dhcp(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    for bmc_mac_address in bmc_mac_addresses(state)? {
        db::bmc_suppression::upsert(
            &mut txn,
            &NewBmcSuppression {
                bmc_mac_address,
                subsystem: BmcSuppressionSubsystem::Dhcp,
                reason: format!(
                    "managed host {} is being decommissioned; suppressing BMC DHCP",
                    state.host_snapshot.id
                ),
            },
        )
        .await?;
    }
    // Not deleting interface rows: bumps machine_interfaces_deletion so the
    // DHCP server restarts, clears its cache, and honors the suppressions above.
    db::machine_interface::record_deletion(&mut txn).await?;
    Ok(
        StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::FactoryResettingBmcs {
                completed: HashSet::new(),
            },
        })
        .with_txn(txn),
    )
}

pub(super) async fn handle_factory_resetting_bmcs(
    completed: &HashSet<MachineId>,
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let Some(machine) = std::iter::once(&state.host_snapshot)
        .chain(&state.dpu_snapshots)
        .find(|machine| !completed.contains(&machine.id))
    else {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::WaitingForBmcDhcpAcknowledgement,
            },
        ));
    };
    ctx.services
        .create_redfish_client_from_machine(machine)
        .await?
        .bmc_reset_to_defaults()
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "failed to factory reset BMC for {}: {error}",
                machine.id
            ))
        })?;
    let mut completed = completed.clone();
    completed.insert(machine.id);
    Ok(StateHandlerOutcome::transition(
        ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::FactoryResettingBmcs { completed },
        },
    ))
}

pub(super) async fn handle_waiting_for_bmc_dhcp_acknowledgement(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let pending_macs = unacknowledged_dhcp_suppression_macs(bmc_mac_addresses(state)?, ctx).await?;
    if pending_macs.is_empty() {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::Decommissioning {
                decommissioning_state: DecommissioningState::DeletingManagedCredentials,
            },
        ));
    }
    Ok(StateHandlerOutcome::wait(format!(
        "waiting for BMC DHCP suppression acknowledgement after factory reset: {}",
        pending_macs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(super) async fn handle_deleting_managed_credentials(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut rotation_cleanups = Vec::new();

    for machine in std::iter::once(&state.host_snapshot).chain(&state.dpu_snapshots) {
        let Some(bmc_mac_address) = machine.status.bmc_info.mac else {
            return Err(StateHandlerError::MissingData {
                object_id: machine.id.to_string(),
                missing: "bmc_mac",
            });
        };

        ctx.services
            .credential_manager
            .delete_credentials(&CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            })
            .await
            .map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "failed to delete managed BMC credentials for {}: {error}",
                    machine.id
                ))
            })?;

        rotation_cleanups.push((
            bmc_mac_address,
            db::credential_rotation::CredentialRotationType::Bmc,
        ));

        if machine.is_dpu() {
            for credential_key in [
                CredentialKey::DpuSsh {
                    machine_id: machine.id,
                },
                CredentialKey::DpuHbn {
                    machine_id: machine.id,
                },
            ] {
                ctx.services
                    .credential_manager
                    .delete_credentials(&credential_key)
                    .await
                    .map_err(|error| {
                        StateHandlerError::GenericError(eyre::eyre!(
                            "failed to delete managed credential for DPU {}: {error}",
                            machine.id
                        ))
                    })?;
            }
            rotation_cleanups.push((
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::DpuUefi,
            ));
        } else {
            rotation_cleanups.push((
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::HostUefi,
            ));
        }
    }

    // NIC lockdown_ikm rows are keyed by the card's NIC MAC, not a BMC MAC,
    // so the per-machine loop above never reaches them.
    for interface in state
        .dpa_interface_snapshots
        .iter()
        .filter(|iface| iface.interface_type == DpaInterfaceType::Svpc)
    {
        rotation_cleanups.push((
            interface.mac_address,
            db::credential_rotation::CredentialRotationType::LockdownIkm,
        ));
    }

    let mut txn = ctx.services.db_pool.begin().await?;
    for (bmc_mac_address, credential_type) in rotation_cleanups {
        db::credential_rotation::delete_device_converged(
            &mut txn,
            bmc_mac_address,
            credential_type,
        )
        .await?;
    }

    Ok(
        StateHandlerOutcome::transition(ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::Decommissioned,
        })
        .with_txn(txn),
    )
}
