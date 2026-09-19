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

//! `RackManager` (V1) service implementation.
//!
//! The generated trait has 50 required methods and tonic emits no default
//! bodies, so every method must exist here even when it is out of scope. The
//! out-of-scope ones are generated rather than written out, so that a `librms`
//! bump which adds an RPC fails to compile in one obvious place instead of
//! silently returning a router 404. The firmware, NVOS, and switch lifecycle
//! RPCs are delegated to their own modules.

use librms::protos::rack_manager::rack_manager_server::RackManager;

use crate::envelope::{
    BatchOutcome, NodeResult, UNMATCHED_NODE, job_states, matched_or_not, node_batch,
    require_job_id,
};
use crate::fabric::Candidate;
use crate::resolve::NodeRef;
use crate::{RmsMock, SimPowerState, rms};

/// The BMC MAC a power request reaches a node by.
///
/// A node no device matches, or one whose device has no BMC, is a per-node
/// failure with a reason.
fn bmc_mac_of(r: &NodeRef<'_>) -> eyre::Result<mac_address::MacAddress> {
    let node = r.node.ok_or_else(|| eyre::eyre!(UNMATCHED_NODE))?;
    node.bmc_mac
        .ok_or_else(|| eyre::eyre!("the simulated device has no BMC"))
}

/// Builds the whole `RackManager` impl.
///
/// The macro emits the `#[tonic::async_trait]` attribute itself rather than
/// being invoked underneath one. Attribute macros expand before the
/// function-like macros in their body, so an `unimplemented_rpcs!` invocation
/// placed inside an already-annotated impl block would emit plain `async fn`
/// methods that `async_trait` never desugars, and every one of them would fail
/// to match the trait signature.
macro_rules! rack_manager_impl {
    (
        implemented { $($implemented:tt)* }
        delegated { $($delegated:ident($dreq:ident) -> $dres:ident => $handler:path,)* }
        unimplemented { $($method:ident($req:ident) -> $res:ident,)* }
    ) => {
        #[tonic::async_trait]
        impl RackManager for RmsMock {
            $($implemented)*

            $(
                /// Served by its domain module.
                async fn $delegated(
                    &self,
                    request: tonic::Request<rms::$dreq>,
                ) -> std::result::Result<tonic::Response<rms::$dres>, tonic::Status> {
                    $handler(self, request).await
                }
            )*

            $(
                /// Out of scope for the mock.
                ///
                /// `UNIMPLEMENTED` is what the issue specifies for these, and
                /// also what a real RMS returns for an RPC it does not serve,
                /// so a client cannot distinguish the two.
                async fn $method(
                    &self,
                    _request: tonic::Request<rms::$req>,
                ) -> std::result::Result<tonic::Response<rms::$res>, tonic::Status> {
                    Err(tonic::Status::unimplemented(concat!(
                        "the machine-a-tron RMS mock does not implement ",
                        stringify!($method),
                    )))
                }
            )*
        }
    };
}

rack_manager_impl! {
    implemented {
        /// `librms` calls this to probe a new connection, retrying it 60
        /// times before giving up, so a client cannot reach any other method
        /// until this one answers.
        async fn get_version(
            &self,
            _request: tonic::Request<rms::GetVersionRequest>,
        ) -> std::result::Result<tonic::Response<rms::GetVersionResponse>, tonic::Status> {
            Ok(tonic::Response::new(rms::GetVersionResponse {
                version: self.config.version_string.clone(),
            }))
        }

        /// Report each node's physical placement.
        ///
        /// Placement is read from the simulated hardware rather than derived
        /// here, so the slot and tray reported over RMS are necessarily the
        /// same ones the node's Redfish chassis reports.
        ///
        /// As the proto specifies, `node_device_details` holds only the nodes
        /// that were found, and the batch fails when any node was not: the
        /// switch caller records the batch message as the node's error, and
        /// the compute caller reports a missing entry, which is more useful
        /// than a placement with every field unset.
        async fn batch_get_node_device_info(
            &self,
            request: tonic::Request<rms::BatchGetNodeDeviceInfoRequest>,
        ) -> std::result::Result<tonic::Response<rms::BatchGetNodeDeviceInfoResponse>, tonic::Status>
        {
            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());

            let node_device_details: Vec<rms::NodeDeviceInfo> = refs
                .iter()
                .filter_map(|r| {
                    let node = r.node?;
                    Some(rms::NodeDeviceInfo {
                        node_id: r.node_id.to_string(),
                        // machine-a-tron serials are hex MAC strings, so there
                        // is no meaningful integer to report here.
                        chassis_sn: None,
                        slot_number: node.slot_number,
                        tray_index: node.tray_index,
                    })
                })
                .collect();

            let outcome = BatchOutcome::of(&matched_or_not(&refs));
            Ok(tonic::Response::new(rms::BatchGetNodeDeviceInfoResponse {
                // Proto3 leaves this at UNSPECIFIED, which callers read as a
                // failure, so it must be set explicitly on every path.
                status: outcome.status as i32,
                message: outcome.message,
                node_device_details,
                stats: Some(outcome.stats),
            }))
        }

        /// Report each node's power, as its simulated BMC sees it.
        ///
        /// `node_power_states` holds only the nodes that were read; a node
        /// that matches nothing or cannot be read is a per-node failure.
        async fn batch_get_power_state(
            &self,
            request: tonic::Request<rms::BatchGetPowerStateRequest>,
        ) -> std::result::Result<tonic::Response<rms::BatchGetPowerStateResponse>, tonic::Status> {
            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());

            let read: Vec<(&str, Result<SimPowerState, String>)> = refs
                .iter()
                .map(|r| {
                    let power = bmc_mac_of(r)
                        .and_then(|mac| self.inventory.power_state(mac))
                        .map_err(|e| e.to_string());
                    (r.node_id, power)
                })
                .collect();

            let node_power_states = read
                .iter()
                .filter_map(|(node_id, power)| {
                    Some(rms::NodePowerState {
                        node_id: (*node_id).to_owned(),
                        pstate: power.as_ref().ok()?.as_pstate().to_owned(),
                    })
                })
                .collect();
            let results: Vec<NodeResult<'_>> = read
                .into_iter()
                .map(|(node_id, power)| (node_id, power.map(drop)))
                .collect();

            Ok(tonic::Response::new(rms::BatchGetPowerStateResponse {
                response: Some(node_batch(&results, "")),
                node_power_states,
            }))
        }

        /// Change power on each node.
        ///
        /// The host applies the same rules as for a Redfish request, and a
        /// node it refuses is a per-node failure. An unspecified operation
        /// is rejected before any node is touched.
        async fn batch_set_power_state(
            &self,
            request: tonic::Request<rms::BatchSetPowerStateRequest>,
        ) -> std::result::Result<tonic::Response<rms::BatchSetPowerStateResponse>, tonic::Status> {
            let op = match rms::PowerOperation::try_from(request.get_ref().operation) {
                Ok(rms::PowerOperation::Unspecified) | Err(_) => {
                    return Err(tonic::Status::invalid_argument(
                        "power operation is unspecified",
                    ));
                }
                Ok(op) => op,
            };

            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());
            let results: Vec<NodeResult<'_>> = refs
                .iter()
                .map(|r| {
                    let outcome = bmc_mac_of(r)
                        .and_then(|mac| self.inventory.set_power(mac, op))
                        .map_err(|e| e.to_string());
                    (r.node_id, outcome)
                })
                .collect();

            Ok(tonic::Response::new(rms::BatchSetPowerStateResponse {
                response: Some(node_batch(&results, "")),
            }))
        }

        /// Report the state of a job and, when asked, of its children.
        ///
        /// The requested id is always echoed, even for a job unknown to this
        /// process; a failed job carries its reason in `error_message`.
        async fn get_job_status(
            &self,
            request: tonic::Request<rms::GetJobStatusRequest>,
        ) -> std::result::Result<tonic::Response<rms::GetJobStatusResponse>, tonic::Status> {
            let req = request.get_ref();
            require_job_id(&req.job_id)?;
            let status = self.observe_job(&req.job_id);

            Ok(tonic::Response::new(rms::GetJobStatusResponse {
                job_states: job_states(&status, req.include_child_job_states),
            }))
        }

        /// Report the fabric's membership and per-switch health.
        ///
        /// A switch the mock has no device for reads back disabled with an
        /// error; the response as a whole still succeeds.
        async fn get_scale_up_fabric_status(
            &self,
            request: tonic::Request<rms::GetScaleUpFabricStatusRequest>,
        ) -> std::result::Result<tonic::Response<rms::GetScaleUpFabricStatusResponse>, tonic::Status>
        {
            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());

            // A rack read before it was configured here still gets a primary.
            self.fabric.ensure_primaries(
                refs.iter()
                    .filter_map(|r| Some((r.rack_id, Candidate::of(r)?))),
            );

            let switches = refs
                .iter()
                .map(|r| {
                    if r.matched() {
                        rms::ScaleUpFabricSwitchStatus {
                            node_id: r.node_id.to_owned(),
                            enabled: self.fabric.is_primary(r.rack_id, r.node_id),
                            fabric_manager_status: crate::fabric::FABRIC_MANAGER_OK.to_owned(),
                            error_message: String::new(),
                        }
                    } else {
                        rms::ScaleUpFabricSwitchStatus {
                            node_id: r.node_id.to_owned(),
                            enabled: false,
                            // Empty when unavailable, as the proto specifies.
                            fabric_manager_status: String::new(),
                            error_message: UNMATCHED_NODE.to_owned(),
                        }
                    }
                })
                .collect();

            Ok(tonic::Response::new(rms::GetScaleUpFabricStatusResponse {
                status: rms::ReturnCode::Success as i32,
                fabric_status: Some(rms::ScaleUpFabricStatus {
                    // Not read by NICo, which takes the topology from the rack profile.
                    topology_type: String::new(),
                    extra_static_configs: Vec::new(),
                    switches,
                }),
                error_message: String::new(),
            }))
        }

        /// Report each switch's fabric-manager service health.
        ///
        /// Only the primary reports a configured control plane; a switch the
        /// mock has no device for is a per-node failure with no body.
        async fn batch_get_scale_up_fabric_service_status(
            &self,
            request: tonic::Request<rms::BatchGetScaleUpFabricServiceStatusRequest>,
        ) -> std::result::Result<
            tonic::Response<rms::BatchGetScaleUpFabricServiceStatusResponse>,
            tonic::Status,
        > {
            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());
            self.fabric.ensure_primaries(
                refs.iter()
                    .filter_map(|r| Some((r.rack_id, Candidate::of(r)?))),
            );

            let service_statuses = refs
                .iter()
                .map(|r| {
                    let entry = if r.matched() {
                        rms::ScaleUpFabricServiceStatusEntry {
                            status_json: crate::fabric::status_json(
                                self.fabric.is_primary(r.rack_id, r.node_id),
                            ),
                            error_message: String::new(),
                        }
                    } else {
                        rms::ScaleUpFabricServiceStatusEntry {
                            status_json: String::new(),
                            error_message: UNMATCHED_NODE.to_owned(),
                        }
                    };
                    (r.node_id.to_owned(), entry)
                })
                .collect();

            let outcome = BatchOutcome::of(&matched_or_not(&refs));
            Ok(tonic::Response::new(
                rms::BatchGetScaleUpFabricServiceStatusResponse {
                    status: outcome.status as i32,
                    service_statuses,
                    stats: Some(outcome.stats),
                },
            ))
        }

        /// Begin configuring certificates on the given switches.
        ///
        /// Nothing is installed; the batch is a parent job with a child per
        /// matched node, and an unmatched node is a per-node failure with no
        /// job.
        async fn configure_switch_certificate(
            &self,
            request: tonic::Request<rms::ConfigureSwitchCertificateRequest>,
        ) -> std::result::Result<tonic::Response<rms::ConfigureSwitchCertificateResponse>, tonic::Status>
        {
            let inventory = self.inventory.nodes();
            let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());

            let batch = self
                .jobs
                .start_batch(refs.iter().filter(|r| r.matched()), None);
            let jobs = batch
                .children
                .into_iter()
                .map(|(node_id, job_id)| rms::ConfigureSwitchCertificateJobInfo {
                    node_id: node_id.to_owned(),
                    job_id,
                })
                .collect();

            Ok(tonic::Response::new(rms::ConfigureSwitchCertificateResponse {
                response: Some(node_batch(&matched_or_not(&refs), &batch.parent)),
                jobs,
            }))
        }

        /// Report progress of a certificate configuration job, parent or
        /// child.
        ///
        /// A job id the mock has no record of is reported completed.
        async fn get_configure_switch_certificate_job_status(
            &self,
            request: tonic::Request<rms::GetConfigureSwitchCertificateJobStatusRequest>,
        ) -> std::result::Result<
            tonic::Response<rms::GetConfigureSwitchCertificateJobStatusResponse>,
            tonic::Status,
        > {
            let job_id = &request.get_ref().job_id;
            require_job_id(job_id)?;
            let status = self.observe_job(job_id);

            Ok(tonic::Response::new(
                rms::GetConfigureSwitchCertificateJobStatusResponse {
                    // The RPC succeeded; `state` reports the job.
                    status: rms::ReturnCode::Success as i32,
                    job_id: job_id.clone(),
                    state: status.state.as_wire_str().to_owned(),
                    message: String::new(),
                    rack_id: status.rack_id,
                    node_id: status.node_id,
                    error_message: status.error_message,
                    result_json: String::new(),
                    created_at: None,
                    updated_at: None,
                },
            ))
        }
    }

    delegated {
        list_firmware_objects(ListFirmwareObjectsRequest) -> ListFirmwareObjectsResponse => crate::firmware::list_firmware_objects,
        apply_firmware_object(ApplyFirmwareObjectRequest) -> ApplyFirmwareObjectResponse => crate::firmware::apply_firmware_object,
        get_firmware_job_status(GetFirmwareJobStatusRequest) -> GetFirmwareJobStatusResponse => crate::firmware::get_firmware_job_status,
        apply_switch_system_image(ApplySwitchSystemImageRequest) -> ApplySwitchSystemImageResponse => crate::nvos::apply_switch_system_image,
        get_switch_system_image_job_status(GetSwitchSystemImageJobStatusRequest) -> GetSwitchSystemImageJobStatusResponse => crate::nvos::get_switch_system_image_job_status,
        update_switch_system_password(UpdateSwitchSystemPasswordRequest) -> UpdateSwitchSystemPasswordResponse => crate::lifecycle::update_switch_system_password,
        batch_reset_switch_factory_default(BatchResetSwitchFactoryDefaultRequest) -> BatchResetSwitchFactoryDefaultResponse => crate::lifecycle::batch_reset_switch_factory_default,
    }

    unimplemented {
        set_power_state(SetPowerStateRequest) -> SetPowerStateResponse,
        get_power_state(GetPowerStateRequest) -> GetPowerStateResponse,
        sequence_rack_power(SequenceRackPowerRequest) -> SequenceRackPowerResponse,
        list_node_inventory(ListNodeInventoryRequest) -> ListNodeInventoryResponse,
        create_nodes(CreateNodesRequest) -> CreateNodesResponse,
        update_node(UpdateNodeRequest) -> UpdateNodeResponse,
        delete_node(DeleteNodeRequest) -> DeleteNodeResponse,
        get_rack_power_on_sequence(GetRackPowerOnSequenceRequest) -> GetRackPowerOnSequenceResponse,
        set_rack_power_on_sequence(SetRackPowerOnSequenceRequest) -> SetRackPowerOnSequenceResponse,
        list_racks(ListRacksRequest) -> ListRacksResponse,
        get_node_device_info(GetNodeDeviceInfoRequest) -> GetNodeDeviceInfoResponse,
        list_node_device_info_by_node_type(ListNodeDeviceInfoByNodeTypeRequest) -> ListNodeDeviceInfoByNodeTypeResponse,
        get_node_firmware_inventory(GetNodeFirmwareInventoryRequest) -> GetNodeFirmwareInventoryResponse,
        update_firmware(UpdateFirmwareRequest) -> UpdateFirmwareResponse,
        batch_update_firmware_by_node_type(BatchUpdateFirmwareByNodeTypeRequest) -> BatchUpdateFirmwareByNodeTypeResponse,
        batch_update_firmware(BatchUpdateFirmwareRequest) -> BatchUpdateFirmwareResponse,
        update_switch_system_image(UpdateSwitchSystemImageRequest) -> UpdateSwitchSystemImageResponse,
        get_rack_firmware_inventory(GetRackFirmwareInventoryRequest) -> GetRackFirmwareInventoryResponse,
        add_firmware_object(AddFirmwareObjectRequest) -> AddFirmwareObjectResponse,
        get_firmware_object(GetFirmwareObjectRequest) -> GetFirmwareObjectResponse,
        delete_firmware_object(DeleteFirmwareObjectRequest) -> DeleteFirmwareObjectResponse,
        set_default_firmware_object(SetDefaultFirmwareObjectRequest) -> SetDefaultFirmwareObjectResponse,
        apply_stored_firmware_object(ApplyStoredFirmwareObjectRequest) -> ApplyStoredFirmwareObjectResponse,
        apply_stored_switch_system_image(ApplyStoredSwitchSystemImageRequest) -> ApplyStoredSwitchSystemImageResponse,
        get_firmware_object_history(GetFirmwareObjectHistoryRequest) -> GetFirmwareObjectHistoryResponse,
        list_switch_firmware(ListSwitchFirmwareRequest) -> ListSwitchFirmwareResponse,
        push_switch_firmware(PushSwitchFirmwareRequest) -> PushSwitchFirmwareResponse,
        configure_scale_up_fabric_manager(ConfigureScaleUpFabricManagerRequest) -> ConfigureScaleUpFabricManagerResponse,
        batch_reset_switch_sdn_factory_default(BatchResetSwitchSdnFactoryDefaultRequest) -> BatchResetSwitchSdnFactoryDefaultResponse,
        get_scale_up_fabric_state(GetScaleUpFabricStateRequest) -> GetScaleUpFabricStateResponse,
        batch_set_scale_up_fabric_state(BatchSetScaleUpFabricStateRequest) -> BatchSetScaleUpFabricStateResponse,
        set_scale_up_fabric_telemetry_interface_state(SetScaleUpFabricTelemetryInterfaceStateRequest) -> SetScaleUpFabricTelemetryInterfaceStateResponse,
        batch_disable_switch_mtls(BatchDisableSwitchMtlsRequest) -> BatchDisableSwitchMtlsResponse,
        list_switch_system_images(ListSwitchSystemImagesRequest) -> ListSwitchSystemImagesResponse,
    }
}
