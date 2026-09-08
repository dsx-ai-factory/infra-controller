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
use std::collections::HashSet;
use std::ops::Deref;

use base64::prelude::*;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineType};
use health_report::HealthReport;
use model::errors::{ModelError, ModelResult};
use model::health::HealthReportSources;
use model::machine::{
    Dpf, DpfState, DpuInfo, DpuInfoStatusObservation, DpuInitState, DpuOsOperationalState,
    DpuRepresentorStatus, FailureCause, InstanceState, Machine, MachineInterfaceSnapshot,
    MachineValidationFilter, ManagedHostState, ManagedHostStateSnapshot, ReprovisionRequest,
    ReprovisionState, slas, state_sla,
};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::forge_agent_control_response as fac;
use crate::model::RpcTryFrom;
use crate::model::instance::snapshot::instance_snapshot_derive_status;

pub mod capabilities;
pub mod infiniband;
pub mod machine_id;
pub mod machine_search_config;
pub mod network;
pub mod nvlink;
pub mod spx;
pub mod upgrade_policy;

impl From<DpuOsOperationalState> for rpc::forge::DpuOsOperationalState {
    fn from(state: DpuOsOperationalState) -> Self {
        Self {
            state_detail: state.state_detail,
        }
    }
}

impl From<DpuRepresentorStatus> for rpc::forge::DpuRepresentorStatus {
    fn from(status: DpuRepresentorStatus) -> Self {
        Self {
            name: status.name,
            carrier_up: status.carrier_up,
            state: status.state,
        }
    }
}

impl From<DpuInfoStatusObservation> for rpc::forge::DpuInfoStatusObservation {
    fn from(observation: DpuInfoStatusObservation) -> Self {
        Self {
            os_operational_state: observation.os_operational_state.map(Into::into),
            firmware_version: observation.firmware_version,
            representors: observation
                .representors
                .into_iter()
                .map(Into::into)
                .collect(),
            last_heartbeat: observation.last_heartbeat.map(rpc::Timestamp::from),
        }
    }
}

impl From<DpuInfo> for rpc::forge::DpuInfo {
    fn from(info: DpuInfo) -> Self {
        rpc::forge::DpuInfo {
            id: info.id,
            loopback_ip: info.loopback_ip,
            observed_status: info.observed_status.map(Into::into),
        }
    }
}

impl RpcTryFrom<ManagedHostStateSnapshot> for Option<rpc::Instance> {
    type Error = RpcDataConversionError;

    fn rpc_try_from(mut snapshot: ManagedHostStateSnapshot) -> Result<Self, Self::Error> {
        let Some(instance) = snapshot.instance.take() else {
            return Ok(None);
        };

        // TODO: If multiple DPUs have reprovisioning requested, we might not get
        // the expected response
        let mut reprovision_request = snapshot.host_snapshot.reprovision_requested.clone();
        for dpu in &snapshot.dpu_snapshots {
            if let Some(reprovision_requested) = dpu.reprovision_requested.as_ref() {
                reprovision_request = Some(reprovision_requested.clone());
            }
        }
        let (_, dpu_id_to_device_map) = snapshot
            .host_snapshot
            .get_dpu_device_and_id_mappings()
            .map_err(|e| {
                RpcDataConversionError::InvalidValue(
                    "dpu_id_to_device_map".to_string(),
                    e.to_string(),
                )
            })?;
        let status = instance_snapshot_derive_status(
            &instance,
            dpu_id_to_device_map,
            snapshot.host_snapshot.primary_attached_dpu_machine_id(),
            snapshot.managed_state.clone(),
            reprovision_request,
            snapshot
                .host_snapshot
                .status
                .infiniband_status_observation
                .as_ref(),
            snapshot
                .host_snapshot
                .status
                .nvlink_status_observation
                .as_ref(),
            snapshot
                .host_snapshot
                .status
                .spx_status_observation
                .as_ref(),
            &snapshot.host_snapshot.health_reports,
        )?;

        Ok(Some(rpc::Instance {
            id: Some(instance.id),
            machine_id: Some(instance.machine_id),
            config: Some(instance.config.try_into()?),
            status: Some(status.try_into()?),
            config_version: instance.config_version.version_string(),
            network_config_version: instance.network_config_version.version_string(),
            ib_config_version: instance.ib_config_version.version_string(),
            dpu_extension_service_version: instance
                .extension_services_config_version
                .version_string(),
            instance_type_id: instance.instance_type_id.map(|i| i.to_string()),
            metadata: Some(instance.metadata.into()),
            tpm_ek_certificate: snapshot.host_snapshot.status.hardware_info.and_then(|hi| {
                hi.tpm_ek_certificate
                    .map(|cert| BASE64_STANDARD.encode(cert.into_bytes()))
            }),
            nvlink_config_version: instance.nvlink_config_version.version_string(),
        }))
    }
}

impl From<Machine> for rpc::forge::dpf_state_response::DpfState {
    fn from(value: Machine) -> Self {
        Self {
            machine_id: value.id.into(),
            enabled: value.config.dpf.enabled,
            used_for_ingestion: value.config.dpf.used_for_ingestion,
        }
    }
}

pub struct RpcMachineTypeWrapper(rpc::forge::MachineType);

impl From<MachineType> for RpcMachineTypeWrapper {
    fn from(value: MachineType) -> Self {
        RpcMachineTypeWrapper(match value {
            MachineType::PredictedHost | MachineType::Host => rpc::forge::MachineType::Host,
            MachineType::Dpu => rpc::forge::MachineType::Dpu,
        })
    }
}

impl Deref for RpcMachineTypeWrapper {
    type Target = rpc::forge::MachineType;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Dpf> for rpc::forge::DpfMachineState {
    fn from(dpf: Dpf) -> Self {
        rpc::forge::DpfMachineState {
            enabled: dpf.enabled,
            used_for_ingestion: dpf.used_for_ingestion,
        }
    }
}

// The deprecated flat fields on `rpc::forge::Machine` must still be populated here for
// backwards-compat until a follow-up PR migrates callers to the new config/status sub-messages.
#[allow(deprecated)]
impl From<Machine> for rpc::forge::Machine {
    fn from(mut machine: Machine) -> Self {
        let dpu_machine_id = machine.dpu_machine_id().ok();
        // Capture source origins before the DPU arm below empties machine.health_reports via
        // std::mem::take, which would otherwise leave health_sources empty for DPU machines.
        let health_sources: Vec<rpc::forge::HealthSourceOrigin> = machine
            .health_reports
            .iter()
            .map(|(report, mode)| rpc::forge::HealthSourceOrigin {
                mode: mode as i32,
                source: report.source.clone(),
            })
            .collect();

        let health = match dpu_machine_id {
            Some(_) => {
                // Taking the reports moves the selected one into the RPC
                // machine; hosts keep theirs for the maintenance fields below.
                let HealthReportSources {
                    replace,
                    mut merges,
                } = std::mem::take(&mut machine.health_reports);
                match replace {
                    Some(over) => over,
                    None => {
                        let mut health = merges
                            .remove(HealthReport::DPU_AGENT_SOURCE)
                            .unwrap_or_else(|| {
                                HealthReport::heartbeat_timeout(
                                    HealthReport::DPU_AGENT_SOURCE.to_string(),
                                    HealthReport::DPU_AGENT_SOURCE.to_string(),
                                    "No health data was received from DPU".to_string(),
                                    true,
                                    false,
                                )
                            });
                        for over in merges.values() {
                            health.merge(over);
                        }
                        health
                    }
                }
            }
            None => HealthReport::empty("aggregate-health".to_string()), // Health is written by ManagedHostStateSnapshot
        };

        let maintenance_reference = machine.config.maintenance_reference.clone();
        let maintenance_start_time = machine.config.maintenance_start_time;

        let dpf = match &dpu_machine_id {
            Some(_) => None, // DPF state is stored on the host.
            None => Some(machine.config.dpf.clone().into()),
        };

        let associated_dpu_machine_ids = machine.associated_dpu_machine_ids();
        let instance_network_restrictions = Some(machine_instance_network_restrictions(&machine));
        let capabilities = machine.to_capabilities().map(|mut c| {
            c.sort();
            c.into()
        });

        let failure_details: Option<String> =
            if machine.status.failure_details.cause != FailureCause::NoError {
                Some(machine.status.failure_details.to_string())
            } else {
                None
            };

        let ib_status = machine
            .status
            .infiniband_status_observation
            .clone()
            .map(|s| s.into());

        let placement_in_rack = Some(rpc::forge::PlacementInRack {
            slot_number: machine.status.slot_number,
            tray_index: machine.status.tray_index,
        });

        // Pre-compute lifecycle state fields shared between status.lifecycle and the flat
        // Machine aliases (state, state_version, state_reason, state_sla).
        let rpc_state = match dpu_machine_id {
            Some(dpu_machine_id) => machine.state.value.dpu_state_string(&dpu_machine_id),
            None => machine.state.value.to_string(),
        };
        let rpc_state_version = machine.state.version.version_string();
        let rpc_state_reason: Option<rpc::forge::ControllerStateReason> =
            machine.controller_state_outcome.map(Into::into);

        let quarantine_state = machine
            .network_config
            .quarantine_state
            .take()
            .map(Into::into);

        let last_observation_time = machine
            .network_status_observation
            .as_ref()
            .map(|obs| obs.observed_at.into());

        let dpu_agent_version = machine
            .network_status_observation
            .take()
            .and_then(|obs| obs.agent_version);

        let interfaces_rpc: Vec<rpc::forge::MachineInterface> = machine
            .status
            .interfaces
            .into_iter()
            .map(|i| i.into())
            .collect();
        let discovery_info =
            machine
                .status
                .hardware_info
                .and_then(|hw_info| match hw_info.try_into() {
                    Ok(di) => Some(di),
                    Err(e) => {
                        tracing::warn!(
                            machine_id = %machine.id,
                            error = %e,
                            "Hardware information couldn't be parsed into discovery info",
                        );
                        None
                    }
                });

        // -- Build the new structured config sub-message --
        let config_msg = rpc::forge::MachineConfig {
            maintenance_reference: maintenance_reference.clone(),
            maintenance_start_time: maintenance_start_time.map(rpc::Timestamp::from),
            firmware_autoupdate: machine.config.firmware_autoupdate,
            instance_type_id: machine
                .config
                .instance_type_id
                .as_ref()
                .map(|i| i.to_string()),
            dpf,
            hw_sku: machine.config.hw_sku.clone(),
        };

        // -- Build the new structured status sub-message --
        let status_msg = rpc::forge::MachineStatus {
            interfaces: interfaces_rpc.clone(),
            discovery_info: discovery_info.clone(),
            last_reboot_time: machine.status.last_reboot_time.map(|t| t.into()),
            last_observation_time,
            associated_host_machine_id: None, // Gets filled in the `ManagedHostStateSnapshot` conversion
            associated_dpu_machine_ids: associated_dpu_machine_ids
                .iter()
                .copied()
                .map(Into::into)
                .collect(),
            last_reboot_requested_time: machine
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.time.into()),
            last_reboot_requested_mode: machine
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.mode.to_string()),
            dpu_agent_version: dpu_agent_version.clone(),
            health: Some(health.clone().into()),
            health_sources: health_sources.clone(),
            failure_details: failure_details.clone(),
            infiniband: ib_status.clone(),
            capabilities: capabilities.clone(),
            hw_sku: machine.status.hw_sku.clone().map(|s| s.into()),
            quarantine: quarantine_state.clone(),
            hw_sku_device_type: machine.status.hw_sku_device_type.clone(),
            update_complete: machine.status.update_complete,
            nvlink_info: machine.status.nvlink_info.clone().map(|i| i.into()),
            nvlink: machine
                .status
                .nvlink_status_observation
                .clone()
                .map(|s| s.into()),
            spx: machine
                .status
                .spx_status_observation
                .clone()
                .map(|s| s.into()),
            last_scout_observed_version: machine.status.last_scout_observed_version.clone(),
            instance_network_restrictions: instance_network_restrictions.clone(),
            lifecycle: Some(rpc::forge::LifecycleStatus {
                state: rpc_state.clone(),
                version: rpc_state_version.clone(),
                state_reason: rpc_state_reason.clone(),
                sla: None, // calculated at RPC handler, see ManagedHostStateSnapshot::rpc_machine_state
            }),
        };

        rpc::Machine {
            id: Some(machine.id),
            rack_id: machine.rack_id.clone(),
            state: rpc_state,
            capabilities,
            instance_type_id: machine.config.instance_type_id.map(|i| i.to_string()),
            state_version: rpc_state_version,
            // calculated at RPC handler, see ManagedHostStateSnapshot::rpc_machine_state
            state_sla: None,
            machine_type: *RpcMachineTypeWrapper::from(machine.id.machine_type()) as _,
            metadata: Some(machine.metadata.into()),
            version: machine.version.version_string(),
            events: machine
                .history
                .into_iter()
                .map(|event| event.into())
                .collect(),
            interfaces: interfaces_rpc,
            discovery_info,
            bmc_info: Some(machine.status.bmc_info.into()),
            last_reboot_time: machine.status.last_reboot_time.map(|t| t.into()),
            last_observation_time,
            dpu_agent_version,
            maintenance_reference,
            maintenance_start_time: maintenance_start_time.map(rpc::Timestamp::from),
            associated_host_machine_id: None, // Gets filled in the `ManagedHostStateSnapshot` conversion
            associated_dpu_machine_ids: associated_dpu_machine_ids
                .into_iter()
                .map(Into::into)
                .collect(),
            inventory: Some(machine.status.inventory.unwrap_or_default().into()),
            last_reboot_requested_time: machine
                .status
                .last_reboot_requested
                .as_ref()
                .map(|x| x.time.into()),
            last_reboot_requested_mode: machine
                .status
                .last_reboot_requested
                .map(|x| x.mode.to_string()),
            state_reason: rpc_state_reason,
            health: Some(health.into()),
            firmware_autoupdate: machine.config.firmware_autoupdate,
            health_sources,
            failure_details,
            ib_status,
            instance_network_restrictions,
            hw_sku: machine.config.hw_sku.clone(),
            hw_sku_status: machine.status.hw_sku.map(|s| s.into()),
            quarantine_state,
            hw_sku_device_type: machine.status.hw_sku_device_type,
            update_complete: machine.status.update_complete,
            nvlink_info: machine.status.nvlink_info.map(|info| info.into()),
            nvlink_status_observation: machine
                .status
                .nvlink_status_observation
                .map(|status| status.into()),
            spx_status_observation: machine
                .status
                .spx_status_observation
                .map(|status| status.into()),
            placement_in_rack,
            last_scout_observed_version: machine.status.last_scout_observed_version,
            dpf,
            config: Some(config_msg),
            status: Some(status_msg),
        }
    }
}

impl From<ReprovisionRequest> for rpc::forge::InstanceUpdateStatus {
    fn from(value: ReprovisionRequest) -> Self {
        rpc::forge::InstanceUpdateStatus {
            module: rpc::forge::instance_update_status::Module::Dpu as i32,
            initiator: value.initiator,
            trigger_received_at: Some(value.requested_at.into()),
            update_triggered_at: value.started_at.map(|x| x.into()),
            user_approval_received: value.user_approval_received,
        }
    }
}

impl From<rpc::forge_agent_control_response::MachineValidationFilter> for MachineValidationFilter {
    fn from(filter: rpc::forge_agent_control_response::MachineValidationFilter) -> Self {
        Self {
            tags: filter.tags,
            allowed_tests: filter.allowed_tests,
            run_unverfied_tests: filter.run_unverfied_tests,
            contexts: filter.contexts.map(|c| c.items),
        }
    }
}

impl From<MachineValidationFilter> for fac::MachineValidationFilter {
    fn from(filter: MachineValidationFilter) -> Self {
        Self {
            tags: filter.tags,
            allowed_tests: filter.allowed_tests,
            run_unverfied_tests: filter.run_unverfied_tests,
            contexts: filter
                .contexts
                .map(|items| rpc::common::StringList { items }),
        }
    }
}

pub fn get_action_for_dpu_state(
    state: &ManagedHostState,
    dpu_machine_id: &DpuMachineId,
) -> ModelResult<fac::Action> {
    Ok(match state {
        ManagedHostState::DPUReprovision { .. }
        | ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision { .. },
        } => {
            let dpu_state = state
                .as_reprovision_state(dpu_machine_id)
                .ok_or(ModelError::MissingDpu((*dpu_machine_id).into()))?;
            match dpu_state {
                ReprovisionState::BufferTime => fac::Action::retry(),
                ReprovisionState::WaitingForNetworkInstall
                | ReprovisionState::DpfStates {
                    substate: DpfState::WaitingForReady { .. },
                } => fac::Action::Discovery(fac::Discovery {}),
                _ => {
                    tracing::info!(
                        dpu_machine_id = %dpu_machine_id,
                        machine_type = "DPU",
                        agent_control_state = %state,
                        "forge agent control",
                    );
                    fac::Action::noop()
                }
            }
        }
        ManagedHostState::DPUInit { dpu_states } => {
            let dpu_state = dpu_states
                .states
                .get(dpu_machine_id)
                .ok_or(ModelError::MissingDpu((*dpu_machine_id).into()))?;

            match dpu_state {
                DpuInitState::Init
                | DpuInitState::DpfStates {
                    state: DpfState::WaitingForReady { .. },
                } => fac::Action::Discovery(fac::Discovery {}),
                _ => {
                    tracing::info!(
                        dpu_machine_id = %dpu_machine_id,
                        machine_type = "DPU",
                        agent_control_state = %state,
                        "forge agent control",
                    );
                    fac::Action::noop()
                }
            }
        }
        _ => {
            // Later this might go to site admin dashboard for manual intervention
            tracing::info!(
                dpu_machine_id = %dpu_machine_id,
                machine_type = "DPU",
                agent_control_state = %state,
                "forge agent control",
            );
            fac::Action::noop()
        }
    })
}

impl From<MachineInterfaceSnapshot> for rpc::MachineInterface {
    #[allow(deprecated)]
    fn from(machine_interface: MachineInterfaceSnapshot) -> rpc::MachineInterface {
        let is_bmc = machine_interface.interface_type == InterfaceType::Bmc;

        rpc::MachineInterface {
            id: Some(machine_interface.id),
            attached_dpu_machine_id: machine_interface.attached_dpu_machine_id.map(Into::into),
            machine_id: machine_interface.machine_id,
            segment_id: Some(machine_interface.segment_id),
            hostname: machine_interface.hostname,
            domain_id: machine_interface.domain_id,
            mac_address: machine_interface.mac_address.to_string(),
            primary_interface: machine_interface.primary_interface,
            address: machine_interface
                .addresses
                .iter()
                .map(|addr| addr.to_string())
                .collect(),
            vendor: machine_interface.vendors.last().cloned(),
            created: Some(machine_interface.created.into()),
            last_dhcp: machine_interface.last_dhcp.map(|t| t.into()),
            power_shelf_id: machine_interface.power_shelf_id,
            is_bmc: Some(is_bmc),
            interface_type: Some(machine_interface.interface_type as i32),
            switch_id: machine_interface.switch_id,
            association_type: machine_interface.association_type.map(|t| t as i32),
        }
    }
}

pub trait ManagedHostStateSnapshotRpc {
    fn into_rpc_machine_state(
        self,
        dpu_machine_id: Option<&MachineId>,
        sla_config: &slas::MachineSlaConfig,
    ) -> Option<rpc::forge::Machine>;
}

impl ManagedHostStateSnapshotRpc for ManagedHostStateSnapshot {
    /// Creates an RPC Machine representation for either the Host or one of the DPUs.
    ///
    /// Consumes the snapshot so the selected `Machine` moves into the
    /// conversion instead of being deep-cloned here; callers that hand over
    /// an owned snapshot per machine (like `snapshot_map_to_rpc_machines`)
    /// get a clone-free conversion. DPU-keyed loads still deep-clone the
    /// snapshot one frame earlier (`managed_host::load_by_machine_ids`) —
    /// the next candidate for this same move-instead-of-clone treatment.
    fn into_rpc_machine_state(
        self,
        dpu_machine_id: Option<&MachineId>,
        sla_config: &slas::MachineSlaConfig,
    ) -> Option<rpc::forge::Machine> {
        let ManagedHostStateSnapshot {
            host_snapshot,
            dpu_snapshots,
            aggregate_health,
            ..
        } = self;
        match dpu_machine_id {
            None => {
                let sla: rpc::forge::StateSla = state_sla(
                    &host_snapshot.id,
                    &host_snapshot.state.value,
                    &host_snapshot.state.version,
                    &aggregate_health,
                    sla_config,
                )
                .into();
                let mut rpc_machine: rpc::forge::Machine = host_snapshot.into();
                rpc_machine.state_sla = Some(sla);
                if let Some(status) = rpc_machine.status.as_mut() {
                    status.health = Some(aggregate_health.clone().into());
                    if let Some(lifecycle) = status.lifecycle.as_mut() {
                        lifecycle.sla = Some(sla);
                    }
                }
                #[allow(deprecated)]
                {
                    rpc_machine.health = Some(aggregate_health.into());
                }
                Some(rpc_machine)
            }
            Some(dpu_machine_id) => {
                let dpu_snapshot = dpu_snapshots
                    .into_iter()
                    .find(|dpu| dpu.id == *dpu_machine_id)?;
                let mut rpc_machine: rpc::forge::Machine = dpu_snapshot.clone().into();
                let sla: rpc::forge::StateSla = state_sla(
                    &dpu_snapshot.id,
                    &dpu_snapshot.state.value,
                    &dpu_snapshot.state.version,
                    &aggregate_health,
                    sla_config,
                )
                .into();
                // In case the DPU does not know the associated Host - we can backfill the data here
                #[allow(deprecated)]
                {
                    rpc_machine.associated_host_machine_id = Some(host_snapshot.id);
                }
                rpc_machine.state_sla = Some(sla);
                if let Some(status) = rpc_machine.status.as_mut() {
                    status.associated_host_machine_id = Some(host_snapshot.id);
                    if let Some(lifecycle) = status.lifecycle.as_mut() {
                        lifecycle.sla = Some(sla);
                    }
                }
                Some(rpc_machine)
            }
        }
    }
}

fn machine_instance_network_restrictions(
    machine: &Machine,
) -> rpc::forge::InstanceNetworkRestrictions {
    let inband_interfaces = machine
        .status
        .interfaces
        .iter()
        .filter(|i| matches!(i.network_segment_type, Some(NetworkSegmentType::HostInband)))
        .collect::<Vec<_>>();

    // If there are no HostInband interfaces, this currently means this machine has DPUs and is
    // not restricted to being in particular network segments
    if inband_interfaces.is_empty() {
        return rpc::forge::InstanceNetworkRestrictions {
            network_segment_membership_type:
                rpc::forge::InstanceNetworkSegmentMembershipType::TenantConfigurable as i32,
            network_segment_ids: vec![],
        };
    }

    // The machine has interfaces on HostInband segments, meaning its network segment
    // memebership is static (cannot be configured at instance allocation time.)

    // Get unique segment ID's and VPC ID's from each HostInband interface
    let inband_network_segment_ids = inband_interfaces
        .iter()
        .map(|iface| iface.segment_id)
        .collect::<HashSet<_>>();

    rpc::forge::InstanceNetworkRestrictions {
        network_segment_membership_type: rpc::forge::InstanceNetworkSegmentMembershipType::Static
            as i32,
        network_segment_ids: inband_network_segment_ids.into_iter().collect(),
    }
}

#[cfg(test)]
mod test {
    use model::machine::{ManagedHostStateSnapshot, slas, state_sla};
    use model::test_support::machine_snapshot;

    use crate as rpc;
    use crate::model::machine::{
        DpuInfo, DpuInfoStatusObservation, DpuOsOperationalState, DpuRepresentorStatus,
        ManagedHostStateSnapshotRpc,
    };

    /// The clone-based wrapper `into_rpc_machine_state` replaced: deep-copy
    /// the selected machine out of a borrowed snapshot, then convert by
    /// value.
    ///
    /// Both wrappers intentionally funnel through the same `From<Machine>`
    /// impl, so the parity tests below pin the move-vs-clone wrapper
    /// restructure — host/DPU selection, aggregate-health and SLA wiring,
    /// and the associated-host id backfill — not the inner conversion, which
    /// the capability exact-equality tests cover.
    #[allow(deprecated)]
    fn clone_based_rpc_machine_state(
        snapshot: &ManagedHostStateSnapshot,
        dpu_machine_id: Option<&carbide_uuid::machine::MachineId>,
        sla_config: &slas::MachineSlaConfig,
    ) -> Option<rpc::forge::Machine> {
        match dpu_machine_id {
            None => {
                let sla: rpc::forge::StateSla = state_sla(
                    &snapshot.host_snapshot.id,
                    &snapshot.host_snapshot.state.value,
                    &snapshot.host_snapshot.state.version,
                    &snapshot.aggregate_health,
                    sla_config,
                )
                .into();
                let mut rpc_machine: rpc::forge::Machine = snapshot.host_snapshot.clone().into();
                rpc_machine.state_sla = Some(sla);
                if let Some(status) = rpc_machine.status.as_mut() {
                    status.health = Some(snapshot.aggregate_health.clone().into());
                    if let Some(lifecycle) = status.lifecycle.as_mut() {
                        lifecycle.sla = Some(sla);
                    }
                }
                #[allow(deprecated)]
                {
                    rpc_machine.health = Some(snapshot.aggregate_health.clone().into());
                }
                Some(rpc_machine)
            }
            Some(dpu_machine_id) => {
                let dpu_snapshot = snapshot
                    .dpu_snapshots
                    .iter()
                    .find(|dpu| dpu.id == *dpu_machine_id)?;
                let sla: rpc::forge::StateSla = state_sla(
                    &dpu_snapshot.id,
                    &dpu_snapshot.state.value,
                    &dpu_snapshot.state.version,
                    &snapshot.aggregate_health,
                    sla_config,
                )
                .into();
                let mut rpc_machine: rpc::forge::Machine = dpu_snapshot.clone().into();
                rpc_machine.state_sla = Some(sla);
                #[allow(deprecated)]
                {
                    rpc_machine.associated_host_machine_id = Some(snapshot.host_snapshot.id);
                }
                if let Some(status) = rpc_machine.status.as_mut() {
                    status.associated_host_machine_id = Some(snapshot.host_snapshot.id);
                    if let Some(lifecycle) = status.lifecycle.as_mut() {
                        lifecycle.sla = Some(sla);
                    }
                }
                Some(rpc_machine)
            }
        }
    }

    /// Sorts the set-derived proto fields whose order is not defined, so two
    /// equivalent conversions compare equal.
    #[allow(deprecated)]
    fn normalized(mut machine: rpc::forge::Machine) -> rpc::forge::Machine {
        if let Some(restrictions) = machine.instance_network_restrictions.as_mut() {
            restrictions.network_segment_ids.sort();
        }
        if let Some(status) = machine.status.as_mut()
            && let Some(restrictions) = status.instance_network_restrictions.as_mut()
        {
            restrictions.network_segment_ids.sort();
        }
        machine
    }

    #[test]
    fn moving_host_conversion_matches_clone_based_conversion() {
        let sla_config = slas::MachineSlaConfig::default();
        let snapshot = machine_snapshot::managed_host_state_snapshot();

        let expected = clone_based_rpc_machine_state(&snapshot, None, &sla_config)
            .expect("host conversion produces a machine");
        let actual = snapshot
            .into_rpc_machine_state(None, &sla_config)
            .expect("host conversion produces a machine");

        // The fixture populates every heavyweight field; make sure the parity
        // check exercises them rather than comparing empty options.
        #[allow(deprecated)]
        let _ = (
            actual.discovery_info.is_some(),
            actual.capabilities.is_some(),
            actual.interfaces.is_empty(),
        );
        assert!(
            actual
                .status
                .as_ref()
                .and_then(|s| s.discovery_info.as_ref())
                .is_some()
        );
        assert!(
            actual
                .status
                .as_ref()
                .and_then(|s| s.capabilities.as_ref())
                .is_some()
        );
        assert!(
            !actual
                .status
                .as_ref()
                .map(|s| s.interfaces.is_empty())
                .unwrap_or(true)
        );
        assert!(!actual.events.is_empty());
        assert_eq!(normalized(expected), normalized(actual));
    }

    #[test]
    fn moving_dpu_conversion_matches_clone_based_conversion() {
        let sla_config = slas::MachineSlaConfig::default();
        let snapshot = machine_snapshot::managed_host_state_snapshot();
        let dpu_id = snapshot.dpu_snapshots[1].id;

        let expected = clone_based_rpc_machine_state(&snapshot, Some(&dpu_id), &sla_config)
            .expect("DPU conversion produces a machine");
        let actual = snapshot
            .into_rpc_machine_state(Some(&dpu_id), &sla_config)
            .expect("DPU conversion produces a machine");

        #[allow(deprecated)]
        let _ = actual.associated_host_machine_id;
        assert_eq!(
            actual
                .status
                .as_ref()
                .and_then(|s| s.associated_host_machine_id),
            Some(machine_snapshot::host_machine_id()),
        );
        assert_eq!(normalized(expected), normalized(actual));
    }

    #[test]
    fn unknown_dpu_id_converts_to_none() {
        let sla_config = slas::MachineSlaConfig::default();
        let snapshot = machine_snapshot::managed_host_state_snapshot();
        let unknown = machine_snapshot::dpu_machine_id(9);

        assert!(
            snapshot
                .into_rpc_machine_state(Some(&unknown), &sla_config)
                .is_none()
        );
    }

    #[test]
    fn dpu_info_to_rpc() {
        let info = DpuInfo {
            id: "dpu-123".to_string(),
            loopback_ip: "10.0.0.1".to_string(),
            observed_status: Some(DpuInfoStatusObservation {
                os_operational_state: Some(DpuOsOperationalState {
                    state_detail: "Ready".to_string(),
                }),
                firmware_version: Some("24.42.1000".to_string()),
                representors: vec![
                    DpuRepresentorStatus {
                        name: "pf0hpf_if".to_string(),
                        carrier_up: Some(true),
                        state: Some("up".to_string()),
                    },
                    DpuRepresentorStatus {
                        name: "pf0vf0_if_r".to_string(),
                        carrier_up: Some(false),
                        state: Some("down".to_string()),
                    },
                ],
                last_heartbeat: Some(chrono::Utc::now()),
            }),
        };
        let expected_last_heartbeat = info
            .observed_status
            .as_ref()
            .and_then(|observation| observation.last_heartbeat)
            .map(rpc::Timestamp::from);
        let rpc_info: rpc::forge::DpuInfo = info.into();
        assert_eq!(rpc_info.id, "dpu-123");
        assert_eq!(rpc_info.loopback_ip, "10.0.0.1");
        let observed_status = rpc_info.observed_status.as_ref().unwrap();
        assert_eq!(
            observed_status
                .os_operational_state
                .as_ref()
                .map(|state| state.state_detail.as_str()),
            Some("Ready")
        );
        assert_eq!(
            observed_status.firmware_version.as_deref(),
            Some("24.42.1000")
        );
        assert_eq!(observed_status.representors.len(), 2);
        assert_eq!(observed_status.representors[0].name, "pf0hpf_if");
        assert_eq!(observed_status.representors[0].carrier_up, Some(true));
        assert_eq!(observed_status.representors[0].state.as_deref(), Some("up"));
        assert_eq!(observed_status.representors[1].name, "pf0vf0_if_r");
        assert_eq!(observed_status.representors[1].carrier_up, Some(false));
        assert_eq!(
            observed_status.representors[1].state.as_deref(),
            Some("down")
        );
        assert_eq!(observed_status.last_heartbeat, expected_last_heartbeat);
    }
}
