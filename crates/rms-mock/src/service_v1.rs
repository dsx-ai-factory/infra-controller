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
//! silently returning a router 404.

use librms::protos::rack_manager::rack_manager_server::RackManager;

use crate::{RmsMock, rms};

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
        unimplemented { $($method:ident($req:ident) -> $res:ident,)* }
    ) => {
        #[tonic::async_trait]
        impl RackManager for RmsMock {
            $($implemented)*

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
            let unmatched: Vec<&str> = refs
                .iter()
                .filter(|r| !r.matched())
                .map(|r| r.node_id)
                .collect();

            let total = refs.len() as u32;
            let matched = node_device_details.len() as u32;
            let (status, message) = if unmatched.is_empty() {
                (rms::ReturnCode::Success, String::new())
            } else {
                (
                    rms::ReturnCode::Failure,
                    format!(
                        "{} of {total} nodes did not match any simulated device: {}",
                        unmatched.len(),
                        unmatched.join(", ")
                    ),
                )
            };

            Ok(tonic::Response::new(rms::BatchGetNodeDeviceInfoResponse {
                // Proto3 leaves this at UNSPECIFIED, which callers read as a
                // failure, so it must be set explicitly on every path.
                status: status as i32,
                message,
                node_device_details,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: total,
                    successful_nodes: matched,
                    failed_nodes: total - matched,
                }),
            }))
        }
    }

    unimplemented {
        set_power_state(SetPowerStateRequest) -> SetPowerStateResponse,
        batch_set_power_state(BatchSetPowerStateRequest) -> BatchSetPowerStateResponse,
        get_power_state(GetPowerStateRequest) -> GetPowerStateResponse,
        batch_get_power_state(BatchGetPowerStateRequest) -> BatchGetPowerStateResponse,
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
        list_firmware_objects(ListFirmwareObjectsRequest) -> ListFirmwareObjectsResponse,
        delete_firmware_object(DeleteFirmwareObjectRequest) -> DeleteFirmwareObjectResponse,
        set_default_firmware_object(SetDefaultFirmwareObjectRequest) -> SetDefaultFirmwareObjectResponse,
        apply_stored_firmware_object(ApplyStoredFirmwareObjectRequest) -> ApplyStoredFirmwareObjectResponse,
        apply_firmware_object(ApplyFirmwareObjectRequest) -> ApplyFirmwareObjectResponse,
        apply_switch_system_image(ApplySwitchSystemImageRequest) -> ApplySwitchSystemImageResponse,
        apply_stored_switch_system_image(ApplyStoredSwitchSystemImageRequest) -> ApplyStoredSwitchSystemImageResponse,
        get_firmware_object_history(GetFirmwareObjectHistoryRequest) -> GetFirmwareObjectHistoryResponse,
        list_switch_firmware(ListSwitchFirmwareRequest) -> ListSwitchFirmwareResponse,
        push_switch_firmware(PushSwitchFirmwareRequest) -> PushSwitchFirmwareResponse,
        batch_reset_switch_factory_default(BatchResetSwitchFactoryDefaultRequest) -> BatchResetSwitchFactoryDefaultResponse,
        configure_scale_up_fabric_manager(ConfigureScaleUpFabricManagerRequest) -> ConfigureScaleUpFabricManagerResponse,
        get_scale_up_fabric_status(GetScaleUpFabricStatusRequest) -> GetScaleUpFabricStatusResponse,
        batch_reset_switch_sdn_factory_default(BatchResetSwitchSdnFactoryDefaultRequest) -> BatchResetSwitchSdnFactoryDefaultResponse,
        batch_set_scale_up_fabric_state(BatchSetScaleUpFabricStateRequest) -> BatchSetScaleUpFabricStateResponse,
        batch_get_scale_up_fabric_service_status(BatchGetScaleUpFabricServiceStatusRequest) -> BatchGetScaleUpFabricServiceStatusResponse,
        get_scale_up_fabric_state(GetScaleUpFabricStateRequest) -> GetScaleUpFabricStateResponse,
        set_scale_up_fabric_telemetry_interface_state(SetScaleUpFabricTelemetryInterfaceStateRequest) -> SetScaleUpFabricTelemetryInterfaceStateResponse,
        configure_switch_certificate(ConfigureSwitchCertificateRequest) -> ConfigureSwitchCertificateResponse,
        batch_disable_switch_mtls(BatchDisableSwitchMtlsRequest) -> BatchDisableSwitchMtlsResponse,
        get_configure_switch_certificate_job_status(GetConfigureSwitchCertificateJobStatusRequest) -> GetConfigureSwitchCertificateJobStatusResponse,
        list_switch_system_images(ListSwitchSystemImagesRequest) -> ListSwitchSystemImagesResponse,
        get_switch_system_image_job_status(GetSwitchSystemImageJobStatusRequest) -> GetSwitchSystemImageJobStatusResponse,
        update_switch_system_password(UpdateSwitchSystemPasswordRequest) -> UpdateSwitchSystemPasswordResponse,
        get_firmware_job_status(GetFirmwareJobStatusRequest) -> GetFirmwareJobStatusResponse,
        get_job_status(GetJobStatusRequest) -> GetJobStatusResponse,
    }
}
