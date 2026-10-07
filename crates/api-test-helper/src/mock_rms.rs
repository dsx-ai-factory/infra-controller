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

//! MockRmsApi is a configurable mock implementation of librms::RmsApi.
//!
//! The idea is tests queue up responses for each method they care about
//! and want to test, then hand an `Arc<MockRmsApi>` to the code under test.
//! As methods are called, queued responses are popped off and returned in
//! order. Recorded requests can be inspected after the call to verify the
//! correct arguments were sent.
//!
//! # Example
//!
//! ```ignore
//! let mock = MockRmsApi::new();
//! mock.enqueue_set_power_state(Ok(MockRmsApi::power_ok())).await;
//!
//! let backend = RmsPowerShelfBackend::new(Arc::new(mock));
//! let results = backend.power_control(&endpoints, PowerAction::On).await.unwrap();
//!
//! assert!(results[0].success);
//! let calls = mock.set_power_state_calls().await;
//! assert_eq!(calls[0].node_id, "ps-001");
//! ```

use std::collections::VecDeque;
use std::sync::Arc;

use librms::protos::rack_manager as rms;
use librms::{RackManagerError, RmsApi};
use tokio::sync::Mutex;

/// A configurable mock `RmsApi` client that lets tests queue responses
/// per method and inspect what requests were sent.
///
/// Every `RmsApi` method has a corresponding:
/// - `enqueue_*()` - push a response to be returned on the next call
/// - `*_calls()` - retrieve all recorded requests for that method
///
/// If no response is queued when a method is called, it returns a
/// clear "no response queued" error so tests fail explicitly.
pub struct MockRmsApi {
    // Power control calls.
    set_power_state_responses:
        Mutex<VecDeque<Result<rms::SetPowerStateResponse, RackManagerError>>>,
    set_power_state_calls: Mutex<Vec<rms::SetPowerStateRequest>>,

    batch_set_power_state_responses:
        Mutex<VecDeque<Result<rms::BatchSetPowerStateResponse, RackManagerError>>>,
    batch_set_power_state_calls: Mutex<Vec<rms::BatchSetPowerStateRequest>>,

    get_power_state_responses:
        Mutex<VecDeque<Result<rms::GetPowerStateResponse, RackManagerError>>>,
    get_power_state_calls: Mutex<Vec<rms::GetPowerStateRequest>>,

    batch_get_power_state_responses:
        Mutex<VecDeque<Result<rms::BatchGetPowerStateResponse, RackManagerError>>>,
    batch_get_power_state_calls: Mutex<Vec<rms::BatchGetPowerStateRequest>>,

    sequence_rack_power_responses:
        Mutex<VecDeque<Result<rms::SequenceRackPowerResponse, RackManagerError>>>,
    sequence_rack_power_calls: Mutex<Vec<rms::SequenceRackPowerRequest>>,

    // Inventory calls.
    list_node_inventory_responses:
        Mutex<VecDeque<Result<rms::ListNodeInventoryResponse, RackManagerError>>>,
    list_node_inventory_calls: Mutex<Vec<rms::ListNodeInventoryRequest>>,

    create_nodes_responses: Mutex<VecDeque<Result<rms::CreateNodesResponse, RackManagerError>>>,
    create_nodes_calls: Mutex<Vec<rms::CreateNodesRequest>>,

    update_node_responses: Mutex<VecDeque<Result<rms::UpdateNodeResponse, RackManagerError>>>,
    update_node_calls: Mutex<Vec<rms::UpdateNodeRequest>>,

    delete_node_responses: Mutex<VecDeque<Result<rms::DeleteNodeResponse, RackManagerError>>>,
    delete_node_calls: Mutex<Vec<rms::DeleteNodeRequest>>,

    list_racks_responses: Mutex<VecDeque<Result<rms::ListRacksResponse, RackManagerError>>>,
    list_racks_calls: Mutex<Vec<rms::ListRacksRequest>>,

    // Device info calls.
    get_node_device_info_responses:
        Mutex<VecDeque<Result<rms::GetNodeDeviceInfoResponse, RackManagerError>>>,
    get_node_device_info_calls: Mutex<Vec<rms::GetNodeDeviceInfoRequest>>,

    list_node_device_info_by_node_type_responses:
        Mutex<VecDeque<Result<rms::ListNodeDeviceInfoByNodeTypeResponse, RackManagerError>>>,
    list_node_device_info_by_node_type_calls: Mutex<Vec<rms::ListNodeDeviceInfoByNodeTypeRequest>>,

    batch_get_node_device_info_responses:
        Mutex<VecDeque<Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError>>>,
    batch_get_node_device_info_calls: Mutex<Vec<rms::BatchGetNodeDeviceInfoRequest>>,

    // Power-on sequence calls.
    get_rack_power_on_sequence_responses:
        Mutex<VecDeque<Result<rms::GetRackPowerOnSequenceResponse, RackManagerError>>>,
    get_rack_power_on_sequence_calls: Mutex<Vec<rms::GetRackPowerOnSequenceRequest>>,

    set_rack_power_on_sequence_responses:
        Mutex<VecDeque<Result<rms::SetRackPowerOnSequenceResponse, RackManagerError>>>,
    set_rack_power_on_sequence_calls: Mutex<Vec<rms::SetRackPowerOnSequenceRequest>>,

    // Node firmware calls.
    get_node_firmware_inventory_responses:
        Mutex<VecDeque<Result<rms::GetNodeFirmwareInventoryResponse, RackManagerError>>>,
    get_node_firmware_inventory_calls: Mutex<Vec<rms::GetNodeFirmwareInventoryRequest>>,

    get_rack_firmware_inventory_responses:
        Mutex<VecDeque<Result<rms::GetRackFirmwareInventoryResponse, RackManagerError>>>,
    get_rack_firmware_inventory_calls: Mutex<Vec<rms::GetRackFirmwareInventoryRequest>>,

    add_firmware_object_responses:
        Mutex<VecDeque<Result<rms::AddFirmwareObjectResponse, RackManagerError>>>,
    add_firmware_object_calls: Mutex<Vec<rms::AddFirmwareObjectRequest>>,

    get_firmware_object_responses:
        Mutex<VecDeque<Result<rms::GetFirmwareObjectResponse, RackManagerError>>>,
    get_firmware_object_calls: Mutex<Vec<rms::GetFirmwareObjectRequest>>,

    list_firmware_objects_responses:
        Mutex<VecDeque<Result<rms::ListFirmwareObjectsResponse, RackManagerError>>>,
    list_firmware_objects_calls: Mutex<Vec<rms::ListFirmwareObjectsRequest>>,

    delete_firmware_object_responses:
        Mutex<VecDeque<Result<rms::DeleteFirmwareObjectResponse, RackManagerError>>>,
    delete_firmware_object_calls: Mutex<Vec<rms::DeleteFirmwareObjectRequest>>,

    set_default_firmware_object_responses:
        Mutex<VecDeque<Result<rms::SetDefaultFirmwareObjectResponse, RackManagerError>>>,
    set_default_firmware_object_calls: Mutex<Vec<rms::SetDefaultFirmwareObjectRequest>>,

    apply_stored_firmware_object_responses:
        Mutex<VecDeque<Result<rms::ApplyStoredFirmwareObjectResponse, RackManagerError>>>,
    apply_stored_firmware_object_calls: Mutex<Vec<rms::ApplyStoredFirmwareObjectRequest>>,

    apply_firmware_object_responses:
        Mutex<VecDeque<Result<rms::ApplyFirmwareObjectResponse, RackManagerError>>>,
    apply_firmware_object_calls: Mutex<Vec<rms::ApplyFirmwareObjectRequest>>,

    apply_switch_system_image_responses:
        Mutex<VecDeque<Result<rms::ApplySwitchSystemImageResponse, RackManagerError>>>,
    apply_switch_system_image_calls: Mutex<Vec<rms::ApplySwitchSystemImageRequest>>,

    apply_stored_switch_system_image_responses:
        Mutex<VecDeque<Result<rms::ApplyStoredSwitchSystemImageResponse, RackManagerError>>>,
    apply_stored_switch_system_image_calls: Mutex<Vec<rms::ApplyStoredSwitchSystemImageRequest>>,

    get_firmware_object_history_responses:
        Mutex<VecDeque<Result<rms::GetFirmwareObjectHistoryResponse, RackManagerError>>>,
    get_firmware_object_history_calls: Mutex<Vec<rms::GetFirmwareObjectHistoryRequest>>,

    update_firmware_responses:
        Mutex<VecDeque<Result<rms::UpdateFirmwareResponse, RackManagerError>>>,
    update_firmware_calls: Mutex<Vec<rms::UpdateFirmwareRequest>>,

    batch_update_firmware_by_node_type_responses:
        Mutex<VecDeque<Result<rms::BatchUpdateFirmwareByNodeTypeResponse, RackManagerError>>>,
    batch_update_firmware_by_node_type_calls: Mutex<Vec<rms::BatchUpdateFirmwareByNodeTypeRequest>>,

    batch_update_firmware_responses:
        Mutex<VecDeque<Result<rms::BatchUpdateFirmwareResponse, RackManagerError>>>,
    batch_update_firmware_calls: Mutex<Vec<rms::BatchUpdateFirmwareRequest>>,

    update_switch_system_image_responses:
        Mutex<VecDeque<Result<rms::UpdateSwitchSystemImageResponse, RackManagerError>>>,
    update_switch_system_image_calls: Mutex<Vec<rms::UpdateSwitchSystemImageRequest>>,

    update_switch_system_password_responses:
        Mutex<VecDeque<Result<rms::UpdateSwitchSystemPasswordResponse, RackManagerError>>>,
    update_switch_system_password_calls: Mutex<Vec<rms::UpdateSwitchSystemPasswordRequest>>,

    batch_reset_switch_sdn_factory_default_responses:
        Mutex<VecDeque<Result<rms::BatchResetSwitchSdnFactoryDefaultResponse, RackManagerError>>>,
    batch_reset_switch_sdn_factory_default_calls:
        Mutex<Vec<rms::BatchResetSwitchSdnFactoryDefaultRequest>>,

    batch_reset_switch_factory_default_responses:
        Mutex<VecDeque<Result<rms::BatchResetSwitchFactoryDefaultResponse, RackManagerError>>>,
    batch_reset_switch_factory_default_calls:
        Mutex<Vec<rms::BatchResetSwitchFactoryDefaultRequest>>,

    get_job_status_responses: Mutex<VecDeque<Result<rms::GetJobStatusResponse, RackManagerError>>>,
    get_job_status_calls: Mutex<Vec<rms::GetJobStatusRequest>>,

    get_firmware_job_status_responses:
        Mutex<VecDeque<Result<rms::GetFirmwareJobStatusResponse, RackManagerError>>>,
    get_firmware_job_status_calls: Mutex<Vec<rms::GetFirmwareJobStatusRequest>>,

    // Switch firmware calls.
    list_switch_firmware_responses:
        Mutex<VecDeque<Result<rms::ListSwitchFirmwareResponse, RackManagerError>>>,
    list_switch_firmware_calls: Mutex<Vec<rms::ListSwitchFirmwareRequest>>,

    push_switch_firmware_responses:
        Mutex<VecDeque<Result<rms::PushSwitchFirmwareResponse, RackManagerError>>>,
    push_switch_firmware_calls: Mutex<Vec<rms::PushSwitchFirmwareRequest>>,

    // Switch certificate calls.
    configure_switch_certificate_responses:
        Mutex<VecDeque<Result<rms::ConfigureSwitchCertificateResponse, RackManagerError>>>,
    configure_switch_certificate_calls: Mutex<Vec<rms::ConfigureSwitchCertificateRequest>>,

    get_configure_switch_certificate_job_status_responses: Mutex<
        VecDeque<Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError>>,
    >,
    get_configure_switch_certificate_job_status_calls:
        Mutex<Vec<rms::GetConfigureSwitchCertificateJobStatusRequest>>,

    // Switch system images calls.
    list_switch_system_images_responses:
        Mutex<VecDeque<Result<rms::ListSwitchSystemImagesResponse, RackManagerError>>>,
    list_switch_system_images_calls: Mutex<Vec<rms::ListSwitchSystemImagesRequest>>,

    get_switch_system_image_job_status_responses:
        Mutex<VecDeque<Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError>>>,
    get_switch_system_image_job_status_calls: Mutex<Vec<rms::GetSwitchSystemImageJobStatusRequest>>,

    // Scale-up fabric calls.
    configure_scale_up_fabric_manager_responses:
        Mutex<VecDeque<Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError>>>,
    configure_scale_up_fabric_manager_calls: Mutex<Vec<rms::ConfigureScaleUpFabricManagerRequest>>,

    batch_set_scale_up_fabric_state_responses:
        Mutex<VecDeque<Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError>>>,
    batch_set_scale_up_fabric_state_calls: Mutex<Vec<rms::BatchSetScaleUpFabricStateRequest>>,

    batch_get_scale_up_fabric_service_status_responses:
        Mutex<VecDeque<Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError>>>,
    batch_get_scale_up_fabric_service_status_calls:
        Mutex<Vec<rms::BatchGetScaleUpFabricServiceStatusRequest>>,

    get_scale_up_fabric_state_responses:
        Mutex<VecDeque<Result<rms::GetScaleUpFabricStateResponse, RackManagerError>>>,
    get_scale_up_fabric_state_calls: Mutex<Vec<rms::GetScaleUpFabricStateRequest>>,

    set_scale_up_fabric_telemetry_interface_state_responses: Mutex<
        VecDeque<Result<rms::SetScaleUpFabricTelemetryInterfaceStateResponse, RackManagerError>>,
    >,
    set_scale_up_fabric_telemetry_interface_state_calls:
        Mutex<Vec<rms::SetScaleUpFabricTelemetryInterfaceStateRequest>>,

    // Version calls.
    version_responses: Mutex<VecDeque<Result<rms::GetVersionResponse, RackManagerError>>>,
    version_call_count: Mutex<u32>,
}

/// Generate enqueue + inspect methods for a request/response pair.
macro_rules! impl_enqueue_inspect {
    ($enqueue:ident, $inspect:ident, $responses:ident, $calls:ident, $req:ty, $resp:ty) => {
        pub async fn $enqueue(&self, resp: Result<$resp, RackManagerError>) {
            self.$responses.lock().await.push_back(resp);
        }

        pub async fn $inspect(&self) -> Vec<$req> {
            self.$calls.lock().await.clone()
        }
    };
}

impl MockRmsApi {
    pub fn new() -> Self {
        Self {
            set_power_state_responses: Default::default(),
            set_power_state_calls: Default::default(),
            batch_set_power_state_responses: Default::default(),
            batch_set_power_state_calls: Default::default(),
            get_power_state_responses: Default::default(),
            get_power_state_calls: Default::default(),
            batch_get_power_state_responses: Default::default(),
            batch_get_power_state_calls: Default::default(),
            sequence_rack_power_responses: Default::default(),
            sequence_rack_power_calls: Default::default(),
            list_node_inventory_responses: Default::default(),
            list_node_inventory_calls: Default::default(),
            create_nodes_responses: Default::default(),
            create_nodes_calls: Default::default(),
            update_node_responses: Default::default(),
            update_node_calls: Default::default(),
            delete_node_responses: Default::default(),
            delete_node_calls: Default::default(),
            list_racks_responses: Default::default(),
            list_racks_calls: Default::default(),
            get_node_device_info_responses: Default::default(),
            get_node_device_info_calls: Default::default(),
            list_node_device_info_by_node_type_responses: Default::default(),
            list_node_device_info_by_node_type_calls: Default::default(),
            batch_get_node_device_info_responses: Default::default(),
            batch_get_node_device_info_calls: Default::default(),
            get_rack_power_on_sequence_responses: Default::default(),
            get_rack_power_on_sequence_calls: Default::default(),
            set_rack_power_on_sequence_responses: Default::default(),
            set_rack_power_on_sequence_calls: Default::default(),
            get_node_firmware_inventory_responses: Default::default(),
            get_node_firmware_inventory_calls: Default::default(),
            get_rack_firmware_inventory_responses: Default::default(),
            get_rack_firmware_inventory_calls: Default::default(),
            add_firmware_object_responses: Default::default(),
            add_firmware_object_calls: Default::default(),
            get_firmware_object_responses: Default::default(),
            get_firmware_object_calls: Default::default(),
            list_firmware_objects_responses: Default::default(),
            list_firmware_objects_calls: Default::default(),
            delete_firmware_object_responses: Default::default(),
            delete_firmware_object_calls: Default::default(),
            set_default_firmware_object_responses: Default::default(),
            set_default_firmware_object_calls: Default::default(),
            apply_stored_firmware_object_responses: Default::default(),
            apply_stored_firmware_object_calls: Default::default(),
            apply_firmware_object_responses: Default::default(),
            apply_firmware_object_calls: Default::default(),
            apply_switch_system_image_responses: Default::default(),
            apply_switch_system_image_calls: Default::default(),
            apply_stored_switch_system_image_responses: Default::default(),
            apply_stored_switch_system_image_calls: Default::default(),
            get_firmware_object_history_responses: Default::default(),
            get_firmware_object_history_calls: Default::default(),
            update_firmware_responses: Default::default(),
            update_firmware_calls: Default::default(),
            batch_update_firmware_by_node_type_responses: Default::default(),
            batch_update_firmware_by_node_type_calls: Default::default(),
            batch_update_firmware_responses: Default::default(),
            batch_update_firmware_calls: Default::default(),
            update_switch_system_image_responses: Default::default(),
            update_switch_system_image_calls: Default::default(),
            update_switch_system_password_responses: Default::default(),
            update_switch_system_password_calls: Default::default(),
            batch_reset_switch_sdn_factory_default_responses: Default::default(),
            batch_reset_switch_sdn_factory_default_calls: Default::default(),
            batch_reset_switch_factory_default_responses: Default::default(),
            batch_reset_switch_factory_default_calls: Default::default(),
            get_job_status_responses: Default::default(),
            get_job_status_calls: Default::default(),
            get_firmware_job_status_responses: Default::default(),
            get_firmware_job_status_calls: Default::default(),
            list_switch_firmware_responses: Default::default(),
            list_switch_firmware_calls: Default::default(),
            push_switch_firmware_responses: Default::default(),
            push_switch_firmware_calls: Default::default(),
            configure_switch_certificate_responses: Default::default(),
            configure_switch_certificate_calls: Default::default(),
            get_configure_switch_certificate_job_status_responses: Default::default(),
            get_configure_switch_certificate_job_status_calls: Default::default(),
            list_switch_system_images_responses: Default::default(),
            list_switch_system_images_calls: Default::default(),
            get_switch_system_image_job_status_responses: Default::default(),
            get_switch_system_image_job_status_calls: Default::default(),
            configure_scale_up_fabric_manager_responses: Default::default(),
            configure_scale_up_fabric_manager_calls: Default::default(),
            batch_set_scale_up_fabric_state_responses: Default::default(),
            batch_set_scale_up_fabric_state_calls: Default::default(),
            batch_get_scale_up_fabric_service_status_responses: Default::default(),
            batch_get_scale_up_fabric_service_status_calls: Default::default(),
            get_scale_up_fabric_state_responses: Default::default(),
            get_scale_up_fabric_state_calls: Default::default(),
            set_scale_up_fabric_telemetry_interface_state_responses: Default::default(),
            set_scale_up_fabric_telemetry_interface_state_calls: Default::default(),
            version_responses: Default::default(),
            version_call_count: Default::default(),
        }
    }

    /// Wrap in `Arc` for passing to code that expects `Arc<dyn RmsApi>`.
    pub fn into_arc(self) -> Arc<dyn RmsApi> {
        Arc::new(self)
    }

    // Power control
    impl_enqueue_inspect!(
        enqueue_set_power_state,
        set_power_state_calls,
        set_power_state_responses,
        set_power_state_calls,
        rms::SetPowerStateRequest,
        rms::SetPowerStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_set_power_state,
        batch_set_power_state_calls,
        batch_set_power_state_responses,
        batch_set_power_state_calls,
        rms::BatchSetPowerStateRequest,
        rms::BatchSetPowerStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_power_state,
        get_power_state_calls,
        get_power_state_responses,
        get_power_state_calls,
        rms::GetPowerStateRequest,
        rms::GetPowerStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_get_power_state,
        batch_get_power_state_calls,
        batch_get_power_state_responses,
        batch_get_power_state_calls,
        rms::BatchGetPowerStateRequest,
        rms::BatchGetPowerStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_sequence_rack_power,
        sequence_rack_power_calls,
        sequence_rack_power_responses,
        sequence_rack_power_calls,
        rms::SequenceRackPowerRequest,
        rms::SequenceRackPowerResponse
    );

    // Inventory
    impl_enqueue_inspect!(
        enqueue_list_node_inventory,
        list_node_inventory_calls,
        list_node_inventory_responses,
        list_node_inventory_calls,
        rms::ListNodeInventoryRequest,
        rms::ListNodeInventoryResponse
    );
    impl_enqueue_inspect!(
        enqueue_create_nodes,
        create_nodes_calls,
        create_nodes_responses,
        create_nodes_calls,
        rms::CreateNodesRequest,
        rms::CreateNodesResponse
    );
    impl_enqueue_inspect!(
        enqueue_update_node,
        update_node_calls,
        update_node_responses,
        update_node_calls,
        rms::UpdateNodeRequest,
        rms::UpdateNodeResponse
    );
    impl_enqueue_inspect!(
        enqueue_delete_node,
        delete_node_calls,
        delete_node_responses,
        delete_node_calls,
        rms::DeleteNodeRequest,
        rms::DeleteNodeResponse
    );
    impl_enqueue_inspect!(
        enqueue_list_racks,
        list_racks_calls,
        list_racks_responses,
        list_racks_calls,
        rms::ListRacksRequest,
        rms::ListRacksResponse
    );

    // Device info
    impl_enqueue_inspect!(
        enqueue_get_node_device_info,
        get_node_device_info_calls,
        get_node_device_info_responses,
        get_node_device_info_calls,
        rms::GetNodeDeviceInfoRequest,
        rms::GetNodeDeviceInfoResponse
    );
    impl_enqueue_inspect!(
        enqueue_list_node_device_info_by_node_type,
        list_node_device_info_by_node_type_calls,
        list_node_device_info_by_node_type_responses,
        list_node_device_info_by_node_type_calls,
        rms::ListNodeDeviceInfoByNodeTypeRequest,
        rms::ListNodeDeviceInfoByNodeTypeResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_get_node_device_info,
        batch_get_node_device_info_calls,
        batch_get_node_device_info_responses,
        batch_get_node_device_info_calls,
        rms::BatchGetNodeDeviceInfoRequest,
        rms::BatchGetNodeDeviceInfoResponse
    );

    // Power-on sequence
    impl_enqueue_inspect!(
        enqueue_get_rack_power_on_sequence,
        get_rack_power_on_sequence_calls,
        get_rack_power_on_sequence_responses,
        get_rack_power_on_sequence_calls,
        rms::GetRackPowerOnSequenceRequest,
        rms::GetRackPowerOnSequenceResponse
    );
    impl_enqueue_inspect!(
        enqueue_set_rack_power_on_sequence,
        set_rack_power_on_sequence_calls,
        set_rack_power_on_sequence_responses,
        set_rack_power_on_sequence_calls,
        rms::SetRackPowerOnSequenceRequest,
        rms::SetRackPowerOnSequenceResponse
    );

    // Node firmware
    impl_enqueue_inspect!(
        enqueue_get_node_firmware_inventory,
        get_node_firmware_inventory_calls,
        get_node_firmware_inventory_responses,
        get_node_firmware_inventory_calls,
        rms::GetNodeFirmwareInventoryRequest,
        rms::GetNodeFirmwareInventoryResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_rack_firmware_inventory,
        get_rack_firmware_inventory_calls,
        get_rack_firmware_inventory_responses,
        get_rack_firmware_inventory_calls,
        rms::GetRackFirmwareInventoryRequest,
        rms::GetRackFirmwareInventoryResponse
    );
    impl_enqueue_inspect!(
        enqueue_add_firmware_object,
        add_firmware_object_calls,
        add_firmware_object_responses,
        add_firmware_object_calls,
        rms::AddFirmwareObjectRequest,
        rms::AddFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_firmware_object,
        get_firmware_object_calls,
        get_firmware_object_responses,
        get_firmware_object_calls,
        rms::GetFirmwareObjectRequest,
        rms::GetFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_list_firmware_objects,
        list_firmware_objects_calls,
        list_firmware_objects_responses,
        list_firmware_objects_calls,
        rms::ListFirmwareObjectsRequest,
        rms::ListFirmwareObjectsResponse
    );
    impl_enqueue_inspect!(
        enqueue_delete_firmware_object,
        delete_firmware_object_calls,
        delete_firmware_object_responses,
        delete_firmware_object_calls,
        rms::DeleteFirmwareObjectRequest,
        rms::DeleteFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_set_default_firmware_object,
        set_default_firmware_object_calls,
        set_default_firmware_object_responses,
        set_default_firmware_object_calls,
        rms::SetDefaultFirmwareObjectRequest,
        rms::SetDefaultFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_apply_stored_firmware_object,
        apply_stored_firmware_object_calls,
        apply_stored_firmware_object_responses,
        apply_stored_firmware_object_calls,
        rms::ApplyStoredFirmwareObjectRequest,
        rms::ApplyStoredFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_apply_firmware_object,
        apply_firmware_object_calls,
        apply_firmware_object_responses,
        apply_firmware_object_calls,
        rms::ApplyFirmwareObjectRequest,
        rms::ApplyFirmwareObjectResponse
    );
    impl_enqueue_inspect!(
        enqueue_apply_switch_system_image,
        apply_switch_system_image_calls,
        apply_switch_system_image_responses,
        apply_switch_system_image_calls,
        rms::ApplySwitchSystemImageRequest,
        rms::ApplySwitchSystemImageResponse
    );
    impl_enqueue_inspect!(
        enqueue_apply_stored_switch_system_image,
        apply_stored_switch_system_image_calls,
        apply_stored_switch_system_image_responses,
        apply_stored_switch_system_image_calls,
        rms::ApplyStoredSwitchSystemImageRequest,
        rms::ApplyStoredSwitchSystemImageResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_firmware_object_history,
        get_firmware_object_history_calls,
        get_firmware_object_history_responses,
        get_firmware_object_history_calls,
        rms::GetFirmwareObjectHistoryRequest,
        rms::GetFirmwareObjectHistoryResponse
    );
    impl_enqueue_inspect!(
        enqueue_update_firmware,
        update_firmware_calls,
        update_firmware_responses,
        update_firmware_calls,
        rms::UpdateFirmwareRequest,
        rms::UpdateFirmwareResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_update_firmware_by_node_type,
        batch_update_firmware_by_node_type_calls,
        batch_update_firmware_by_node_type_responses,
        batch_update_firmware_by_node_type_calls,
        rms::BatchUpdateFirmwareByNodeTypeRequest,
        rms::BatchUpdateFirmwareByNodeTypeResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_update_firmware,
        batch_update_firmware_calls,
        batch_update_firmware_responses,
        batch_update_firmware_calls,
        rms::BatchUpdateFirmwareRequest,
        rms::BatchUpdateFirmwareResponse
    );
    impl_enqueue_inspect!(
        enqueue_update_switch_system_image,
        update_switch_system_image_calls,
        update_switch_system_image_responses,
        update_switch_system_image_calls,
        rms::UpdateSwitchSystemImageRequest,
        rms::UpdateSwitchSystemImageResponse
    );
    impl_enqueue_inspect!(
        enqueue_update_switch_system_password,
        update_switch_system_password_calls,
        update_switch_system_password_responses,
        update_switch_system_password_calls,
        rms::UpdateSwitchSystemPasswordRequest,
        rms::UpdateSwitchSystemPasswordResponse
    );

    impl_enqueue_inspect!(
        enqueue_batch_reset_switch_sdn_factory_default,
        batch_reset_switch_sdn_factory_default_calls,
        batch_reset_switch_sdn_factory_default_responses,
        batch_reset_switch_sdn_factory_default_calls,
        rms::BatchResetSwitchSdnFactoryDefaultRequest,
        rms::BatchResetSwitchSdnFactoryDefaultResponse
    );

    impl_enqueue_inspect!(
        enqueue_batch_reset_switch_factory_default,
        batch_reset_switch_factory_default_calls,
        batch_reset_switch_factory_default_responses,
        batch_reset_switch_factory_default_calls,
        rms::BatchResetSwitchFactoryDefaultRequest,
        rms::BatchResetSwitchFactoryDefaultResponse
    );

    impl_enqueue_inspect!(
        enqueue_get_job_status,
        get_job_status_calls,
        get_job_status_responses,
        get_job_status_calls,
        rms::GetJobStatusRequest,
        rms::GetJobStatusResponse
    );

    impl_enqueue_inspect!(
        enqueue_get_firmware_job_status,
        get_firmware_job_status_calls,
        get_firmware_job_status_responses,
        get_firmware_job_status_calls,
        rms::GetFirmwareJobStatusRequest,
        rms::GetFirmwareJobStatusResponse
    );

    // Switch firmware
    impl_enqueue_inspect!(
        enqueue_list_switch_firmware,
        list_switch_firmware_calls,
        list_switch_firmware_responses,
        list_switch_firmware_calls,
        rms::ListSwitchFirmwareRequest,
        rms::ListSwitchFirmwareResponse
    );
    impl_enqueue_inspect!(
        enqueue_push_switch_firmware,
        push_switch_firmware_calls,
        push_switch_firmware_responses,
        push_switch_firmware_calls,
        rms::PushSwitchFirmwareRequest,
        rms::PushSwitchFirmwareResponse
    );

    // Switch certificate
    impl_enqueue_inspect!(
        enqueue_configure_switch_certificate,
        configure_switch_certificate_calls,
        configure_switch_certificate_responses,
        configure_switch_certificate_calls,
        rms::ConfigureSwitchCertificateRequest,
        rms::ConfigureSwitchCertificateResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_configure_switch_certificate_job_status,
        get_configure_switch_certificate_job_status_calls,
        get_configure_switch_certificate_job_status_responses,
        get_configure_switch_certificate_job_status_calls,
        rms::GetConfigureSwitchCertificateJobStatusRequest,
        rms::GetConfigureSwitchCertificateJobStatusResponse
    );

    // Switch system images
    impl_enqueue_inspect!(
        enqueue_list_switch_system_images,
        list_switch_system_images_calls,
        list_switch_system_images_responses,
        list_switch_system_images_calls,
        rms::ListSwitchSystemImagesRequest,
        rms::ListSwitchSystemImagesResponse
    );

    impl_enqueue_inspect!(
        enqueue_get_switch_system_image_job_status,
        get_switch_system_image_job_status_calls,
        get_switch_system_image_job_status_responses,
        get_switch_system_image_job_status_calls,
        rms::GetSwitchSystemImageJobStatusRequest,
        rms::GetSwitchSystemImageJobStatusResponse
    );

    // Scale-up fabric
    impl_enqueue_inspect!(
        enqueue_configure_scale_up_fabric_manager,
        configure_scale_up_fabric_manager_calls,
        configure_scale_up_fabric_manager_responses,
        configure_scale_up_fabric_manager_calls,
        rms::ConfigureScaleUpFabricManagerRequest,
        rms::ConfigureScaleUpFabricManagerResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_set_scale_up_fabric_state,
        batch_set_scale_up_fabric_state_calls,
        batch_set_scale_up_fabric_state_responses,
        batch_set_scale_up_fabric_state_calls,
        rms::BatchSetScaleUpFabricStateRequest,
        rms::BatchSetScaleUpFabricStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_batch_get_scale_up_fabric_service_status,
        batch_get_scale_up_fabric_service_status_calls,
        batch_get_scale_up_fabric_service_status_responses,
        batch_get_scale_up_fabric_service_status_calls,
        rms::BatchGetScaleUpFabricServiceStatusRequest,
        rms::BatchGetScaleUpFabricServiceStatusResponse
    );
    impl_enqueue_inspect!(
        enqueue_get_scale_up_fabric_state,
        get_scale_up_fabric_state_calls,
        get_scale_up_fabric_state_responses,
        get_scale_up_fabric_state_calls,
        rms::GetScaleUpFabricStateRequest,
        rms::GetScaleUpFabricStateResponse
    );
    impl_enqueue_inspect!(
        enqueue_set_scale_up_fabric_telemetry_interface_state,
        set_scale_up_fabric_telemetry_interface_state_calls,
        set_scale_up_fabric_telemetry_interface_state_responses,
        set_scale_up_fabric_telemetry_interface_state_calls,
        rms::SetScaleUpFabricTelemetryInterfaceStateRequest,
        rms::SetScaleUpFabricTelemetryInterfaceStateResponse
    );

    // The RmsApi wrapper exposes get_version without a request argument.
    pub async fn enqueue_version(&self, resp: Result<rms::GetVersionResponse, RackManagerError>) {
        self.version_responses.lock().await.push_back(resp);
    }

    pub async fn version_call_count(&self) -> u32 {
        *self.version_call_count.lock().await
    }

    // ...and put a few response builders/helpers in here.

    /// Success response for `set_power_state`.
    pub fn power_ok() -> rms::SetPowerStateResponse {
        rms::SetPowerStateResponse {
            status: rms::ReturnCode::Success as i32,
        }
    }

    /// Failure response for `set_power_state`.
    pub fn power_fail() -> rms::SetPowerStateResponse {
        rms::SetPowerStateResponse {
            status: rms::ReturnCode::Failure as i32,
        }
    }

    /// Success response for `batch_set_power_state` covering a
    /// single targeted node.
    pub fn batch_set_power_state_ok(node_id: &str) -> rms::BatchSetPowerStateResponse {
        rms::BatchSetPowerStateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Success as i32,
                    error_message: String::new(),
                }],
                ..Default::default()
            }),
        }
    }

    /// Failure response for `batch_set_power_state` covering a
    /// single targeted node, with an optional batch-level message and
    /// per-node `error_message`.
    pub fn batch_set_power_state_fail(
        node_id: &str,
        node_error_message: &str,
    ) -> rms::BatchSetPowerStateResponse {
        rms::BatchSetPowerStateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Failure as i32,
                message: String::new(),
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 0,
                    failed_nodes: 1,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: node_error_message.to_owned(),
                }],
                ..Default::default()
            }),
        }
    }

    /// Success response for `update_firmware` with a job ID.
    pub fn firmware_update_ok(job_id: &str) -> rms::UpdateFirmwareResponse {
        rms::UpdateFirmwareResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: job_id.to_owned(),
            ..Default::default()
        }
    }

    /// Failure response for `update_firmware`.
    pub fn firmware_update_fail(msg: &str) -> rms::UpdateFirmwareResponse {
        rms::UpdateFirmwareResponse {
            status: rms::ReturnCode::Failure as i32,
            message: msg.to_owned(),
            ..Default::default()
        }
    }

    /// Success response for `apply_firmware_object` with a child job ID.
    pub fn firmware_object_apply_ok(
        node_id: &str,
        job_id: &str,
    ) -> rms::ApplyFirmwareObjectResponse {
        rms::ApplyFirmwareObjectResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Success as i32,
                    error_message: String::new(),
                }],
                ..Default::default()
            }),
            jobs: vec![rms::NodeFirmwareJobInfo {
                node_id: node_id.to_owned(),
                job_id: job_id.to_owned(),
            }],
            object_id: "fw-json".to_owned(),
        }
    }

    /// Failure response for `apply_firmware_object`.
    pub fn firmware_object_apply_fail(
        node_id: &str,
        msg: &str,
    ) -> rms::ApplyFirmwareObjectResponse {
        rms::ApplyFirmwareObjectResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Failure as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 0,
                    failed_nodes: 1,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: msg.to_owned(),
                }],
                ..Default::default()
            }),
            jobs: Vec::new(),
            object_id: "fw-json".to_owned(),
        }
    }

    /// Success response for `apply_switch_system_image` with a child job ID.
    pub fn switch_system_image_apply_ok(
        node_id: &str,
        job_id: &str,
    ) -> rms::ApplySwitchSystemImageResponse {
        rms::ApplySwitchSystemImageResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Success as i32,
                    error_message: String::new(),
                }],
                ..Default::default()
            }),
            jobs: vec![rms::SwitchSystemImageUpdateJobInfo {
                node_id: node_id.to_owned(),
                job_id: job_id.to_owned(),
            }],
            object_id: "fw-json".to_owned(),
            image_filename: "nvos.img".to_owned(),
        }
    }

    /// Failure response for `apply_switch_system_image`.
    pub fn switch_system_image_apply_fail(
        node_id: &str,
        msg: &str,
    ) -> rms::ApplySwitchSystemImageResponse {
        rms::ApplySwitchSystemImageResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Failure as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 0,
                    failed_nodes: 1,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: msg.to_owned(),
                }],
                ..Default::default()
            }),
            jobs: Vec::new(),
            object_id: "fw-json".to_owned(),
            image_filename: "nvos.img".to_owned(),
        }
    }

    /// Success response for `get_firmware_job_status`.
    pub fn firmware_job_status_ok(
        state: rms::FirmwareJobState,
    ) -> rms::GetFirmwareJobStatusResponse {
        rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_state: state as i32,
            ..Default::default()
        }
    }

    /// Success response for `get_switch_system_image_job_status`.
    pub fn switch_system_image_job_status_ok(
        state: &str,
    ) -> rms::GetSwitchSystemImageJobStatusResponse {
        rms::GetSwitchSystemImageJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            state: state.to_owned(),
            ..Default::default()
        }
    }

    /// Success response for `configure_switch_certificate` with a child job ID.
    pub fn configure_switch_certificate_ok(
        node_id: &str,
        job_id: &str,
    ) -> rms::ConfigureSwitchCertificateResponse {
        rms::ConfigureSwitchCertificateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: node_id.to_owned(),
                    status: rms::ReturnCode::Success as i32,
                    error_message: String::new(),
                }],
                ..Default::default()
            }),
            jobs: vec![rms::ConfigureSwitchCertificateJobInfo {
                node_id: node_id.to_owned(),
                job_id: job_id.to_owned(),
            }],
        }
    }

    /// Success response for `get_configure_switch_certificate_job_status`.
    pub fn configure_switch_certificate_job_status_ok(
        state: &str,
    ) -> rms::GetConfigureSwitchCertificateJobStatusResponse {
        rms::GetConfigureSwitchCertificateJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            state: state.to_owned(),
            ..Default::default()
        }
    }

    pub async fn get_switch_system_image_job_status_for_test(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError> {
        self.get_switch_system_image_job_status_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .get_switch_system_image_job_status_responses
                .lock()
                .await,
        )
    }

    /// Success response for `get_node_firmware_inventory`.
    pub fn firmware_inventory_ok(
        versions: &[(&str, &str)],
    ) -> rms::GetNodeFirmwareInventoryResponse {
        rms::GetNodeFirmwareInventoryResponse {
            status: rms::ReturnCode::Success as i32,
            firmware_list: versions
                .iter()
                .map(|(name, ver)| rms::FirmwareInventoryInfo {
                    name: name.to_string(),
                    version: ver.to_string(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// Build an RMS API error (useful for simulating transport failures).
    pub fn unavailable(msg: &str) -> RackManagerError {
        RackManagerError::ApiInvocationError(tonic::Status::unavailable(msg))
    }
}

impl Default for MockRmsApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned when a test forgets to enqueue a response.
fn no_response_queued() -> RackManagerError {
    RackManagerError::ApiInvocationError(tonic::Status::internal("mock: no response queued"))
}

/// Pop the next queued response, or return a clear error if none was enqueued.
fn pop_or_err<T>(
    q: &mut tokio::sync::MutexGuard<'_, VecDeque<Result<T, RackManagerError>>>,
) -> Result<T, RackManagerError> {
    q.pop_front().unwrap_or(Err(no_response_queued()))
}

#[async_trait::async_trait]
impl RmsApi for MockRmsApi {
    async fn set_power_state(
        &self,
        cmd: rms::SetPowerStateRequest,
    ) -> Result<rms::SetPowerStateResponse, RackManagerError> {
        self.set_power_state_calls.lock().await.push(cmd);
        pop_or_err(&mut self.set_power_state_responses.lock().await)
    }
    async fn batch_set_power_state(
        &self,
        cmd: rms::BatchSetPowerStateRequest,
    ) -> Result<rms::BatchSetPowerStateResponse, RackManagerError> {
        self.batch_set_power_state_calls.lock().await.push(cmd);
        pop_or_err(&mut self.batch_set_power_state_responses.lock().await)
    }
    async fn update_switch_system_password(
        &self,
        cmd: rms::UpdateSwitchSystemPasswordRequest,
    ) -> Result<rms::UpdateSwitchSystemPasswordResponse, RackManagerError> {
        self.update_switch_system_password_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.update_switch_system_password_responses.lock().await)
    }

    async fn batch_reset_switch_sdn_factory_default(
        &self,
        cmd: rms::BatchResetSwitchSdnFactoryDefaultRequest,
    ) -> Result<rms::BatchResetSwitchSdnFactoryDefaultResponse, RackManagerError> {
        self.batch_reset_switch_sdn_factory_default_calls
            .lock()
            .await
            .push(cmd);

        pop_or_err(
            &mut self
                .batch_reset_switch_sdn_factory_default_responses
                .lock()
                .await,
        )
    }

    async fn batch_reset_switch_factory_default(
        &self,
        cmd: rms::BatchResetSwitchFactoryDefaultRequest,
    ) -> Result<rms::BatchResetSwitchFactoryDefaultResponse, RackManagerError> {
        self.batch_reset_switch_factory_default_calls
            .lock()
            .await
            .push(cmd);

        pop_or_err(
            &mut self
                .batch_reset_switch_factory_default_responses
                .lock()
                .await,
        )
    }

    async fn get_job_status(
        &self,
        cmd: rms::GetJobStatusRequest,
    ) -> Result<rms::GetJobStatusResponse, RackManagerError> {
        self.get_job_status_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_job_status_responses.lock().await)
    }

    async fn get_power_state(
        &self,
        cmd: rms::GetPowerStateRequest,
    ) -> Result<rms::GetPowerStateResponse, RackManagerError> {
        self.get_power_state_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_power_state_responses.lock().await)
    }

    async fn batch_get_power_state(
        &self,
        cmd: rms::BatchGetPowerStateRequest,
    ) -> Result<rms::BatchGetPowerStateResponse, RackManagerError> {
        self.batch_get_power_state_calls.lock().await.push(cmd);
        pop_or_err(&mut self.batch_get_power_state_responses.lock().await)
    }

    async fn sequence_rack_power(
        &self,
        cmd: rms::SequenceRackPowerRequest,
    ) -> Result<rms::SequenceRackPowerResponse, RackManagerError> {
        self.sequence_rack_power_calls.lock().await.push(cmd);
        pop_or_err(&mut self.sequence_rack_power_responses.lock().await)
    }
    async fn list_node_inventory(
        &self,
    ) -> Result<rms::ListNodeInventoryResponse, RackManagerError> {
        self.list_node_inventory_calls
            .lock()
            .await
            .push(rms::ListNodeInventoryRequest::default());
        pop_or_err(&mut self.list_node_inventory_responses.lock().await)
    }
    async fn create_nodes(
        &self,
        cmd: rms::CreateNodesRequest,
    ) -> Result<rms::CreateNodesResponse, RackManagerError> {
        self.create_nodes_calls.lock().await.push(cmd);
        pop_or_err(&mut self.create_nodes_responses.lock().await)
    }
    async fn update_node(
        &self,
        cmd: rms::UpdateNodeRequest,
    ) -> Result<rms::UpdateNodeResponse, RackManagerError> {
        self.update_node_calls.lock().await.push(cmd);
        pop_or_err(&mut self.update_node_responses.lock().await)
    }
    async fn delete_node(
        &self,
        cmd: rms::DeleteNodeRequest,
    ) -> Result<rms::DeleteNodeResponse, RackManagerError> {
        self.delete_node_calls.lock().await.push(cmd);
        pop_or_err(&mut self.delete_node_responses.lock().await)
    }
    async fn list_racks(&self) -> Result<rms::ListRacksResponse, RackManagerError> {
        self.list_racks_calls
            .lock()
            .await
            .push(rms::ListRacksRequest::default());
        pop_or_err(&mut self.list_racks_responses.lock().await)
    }
    async fn get_node_device_info(
        &self,
        cmd: rms::GetNodeDeviceInfoRequest,
    ) -> Result<rms::GetNodeDeviceInfoResponse, RackManagerError> {
        self.get_node_device_info_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_node_device_info_responses.lock().await)
    }
    async fn list_node_device_info_by_node_type(
        &self,
        cmd: rms::ListNodeDeviceInfoByNodeTypeRequest,
    ) -> Result<rms::ListNodeDeviceInfoByNodeTypeResponse, RackManagerError> {
        self.list_node_device_info_by_node_type_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .list_node_device_info_by_node_type_responses
                .lock()
                .await,
        )
    }
    async fn batch_get_node_device_info(
        &self,
        cmd: rms::BatchGetNodeDeviceInfoRequest,
    ) -> Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError> {
        self.batch_get_node_device_info_calls.lock().await.push(cmd);
        pop_or_err(&mut self.batch_get_node_device_info_responses.lock().await)
    }
    async fn get_rack_power_on_sequence(
        &self,
        cmd: rms::GetRackPowerOnSequenceRequest,
    ) -> Result<rms::GetRackPowerOnSequenceResponse, RackManagerError> {
        self.get_rack_power_on_sequence_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_rack_power_on_sequence_responses.lock().await)
    }
    async fn set_rack_power_on_sequence(
        &self,
        cmd: rms::SetRackPowerOnSequenceRequest,
    ) -> Result<rms::SetRackPowerOnSequenceResponse, RackManagerError> {
        self.set_rack_power_on_sequence_calls.lock().await.push(cmd);
        pop_or_err(&mut self.set_rack_power_on_sequence_responses.lock().await)
    }
    async fn get_node_firmware_inventory(
        &self,
        cmd: rms::GetNodeFirmwareInventoryRequest,
    ) -> Result<rms::GetNodeFirmwareInventoryResponse, RackManagerError> {
        self.get_node_firmware_inventory_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.get_node_firmware_inventory_responses.lock().await)
    }
    async fn get_rack_firmware_inventory(
        &self,
        cmd: rms::GetRackFirmwareInventoryRequest,
    ) -> Result<rms::GetRackFirmwareInventoryResponse, RackManagerError> {
        self.get_rack_firmware_inventory_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.get_rack_firmware_inventory_responses.lock().await)
    }
    async fn add_firmware_object(
        &self,
        cmd: rms::AddFirmwareObjectRequest,
    ) -> Result<rms::AddFirmwareObjectResponse, RackManagerError> {
        self.add_firmware_object_calls.lock().await.push(cmd);
        pop_or_err(&mut self.add_firmware_object_responses.lock().await)
    }

    async fn get_firmware_object(
        &self,
        cmd: rms::GetFirmwareObjectRequest,
    ) -> Result<rms::GetFirmwareObjectResponse, RackManagerError> {
        self.get_firmware_object_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_firmware_object_responses.lock().await)
    }

    async fn list_firmware_objects(
        &self,
        cmd: rms::ListFirmwareObjectsRequest,
    ) -> Result<rms::ListFirmwareObjectsResponse, RackManagerError> {
        self.list_firmware_objects_calls.lock().await.push(cmd);
        pop_or_err(&mut self.list_firmware_objects_responses.lock().await)
    }
    async fn delete_firmware_object(
        &self,
        cmd: rms::DeleteFirmwareObjectRequest,
    ) -> Result<rms::DeleteFirmwareObjectResponse, RackManagerError> {
        self.delete_firmware_object_calls.lock().await.push(cmd);
        pop_or_err(&mut self.delete_firmware_object_responses.lock().await)
    }

    async fn set_default_firmware_object(
        &self,
        cmd: rms::SetDefaultFirmwareObjectRequest,
    ) -> Result<rms::SetDefaultFirmwareObjectResponse, RackManagerError> {
        self.set_default_firmware_object_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.set_default_firmware_object_responses.lock().await)
    }

    async fn apply_stored_firmware_object(
        &self,
        cmd: rms::ApplyStoredFirmwareObjectRequest,
    ) -> Result<rms::ApplyStoredFirmwareObjectResponse, RackManagerError> {
        self.apply_stored_firmware_object_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.apply_stored_firmware_object_responses.lock().await)
    }

    async fn apply_firmware_object(
        &self,
        cmd: rms::ApplyFirmwareObjectRequest,
    ) -> Result<rms::ApplyFirmwareObjectResponse, RackManagerError> {
        self.apply_firmware_object_calls.lock().await.push(cmd);
        pop_or_err(&mut self.apply_firmware_object_responses.lock().await)
    }

    async fn apply_switch_system_image(
        &self,
        cmd: rms::ApplySwitchSystemImageRequest,
    ) -> Result<rms::ApplySwitchSystemImageResponse, RackManagerError> {
        self.apply_switch_system_image_calls.lock().await.push(cmd);
        pop_or_err(&mut self.apply_switch_system_image_responses.lock().await)
    }

    async fn apply_stored_switch_system_image(
        &self,
        cmd: rms::ApplyStoredSwitchSystemImageRequest,
    ) -> Result<rms::ApplyStoredSwitchSystemImageResponse, RackManagerError> {
        self.apply_stored_switch_system_image_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.apply_stored_switch_system_image_responses.lock().await)
    }
    async fn get_firmware_object_history(
        &self,
        cmd: rms::GetFirmwareObjectHistoryRequest,
    ) -> Result<rms::GetFirmwareObjectHistoryResponse, RackManagerError> {
        self.get_firmware_object_history_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.get_firmware_object_history_responses.lock().await)
    }
    async fn update_firmware(
        &self,
        cmd: rms::UpdateFirmwareRequest,
    ) -> Result<rms::UpdateFirmwareResponse, RackManagerError> {
        self.update_firmware_calls.lock().await.push(cmd);
        pop_or_err(&mut self.update_firmware_responses.lock().await)
    }
    async fn batch_update_firmware_by_node_type(
        &self,
        cmd: rms::BatchUpdateFirmwareByNodeTypeRequest,
    ) -> Result<rms::BatchUpdateFirmwareByNodeTypeResponse, RackManagerError> {
        self.batch_update_firmware_by_node_type_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .batch_update_firmware_by_node_type_responses
                .lock()
                .await,
        )
    }
    async fn batch_update_firmware(
        &self,
        cmd: rms::BatchUpdateFirmwareRequest,
    ) -> Result<rms::BatchUpdateFirmwareResponse, RackManagerError> {
        self.batch_update_firmware_calls.lock().await.push(cmd);
        pop_or_err(&mut self.batch_update_firmware_responses.lock().await)
    }

    async fn update_switch_system_image(
        &self,
        cmd: rms::UpdateSwitchSystemImageRequest,
    ) -> Result<rms::UpdateSwitchSystemImageResponse, RackManagerError> {
        self.update_switch_system_image_calls.lock().await.push(cmd);
        pop_or_err(&mut self.update_switch_system_image_responses.lock().await)
    }

    async fn get_firmware_job_status(
        &self,
        cmd: rms::GetFirmwareJobStatusRequest,
    ) -> Result<rms::GetFirmwareJobStatusResponse, RackManagerError> {
        self.get_firmware_job_status_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_firmware_job_status_responses.lock().await)
    }
    async fn list_switch_firmware(
        &self,
        cmd: rms::ListSwitchFirmwareRequest,
    ) -> Result<rms::ListSwitchFirmwareResponse, RackManagerError> {
        self.list_switch_firmware_calls.lock().await.push(cmd);
        pop_or_err(&mut self.list_switch_firmware_responses.lock().await)
    }
    async fn push_switch_firmware(
        &self,
        cmd: rms::PushSwitchFirmwareRequest,
    ) -> Result<rms::PushSwitchFirmwareResponse, RackManagerError> {
        self.push_switch_firmware_calls.lock().await.push(cmd);
        pop_or_err(&mut self.push_switch_firmware_responses.lock().await)
    }

    async fn configure_switch_certificate(
        &self,
        cmd: rms::ConfigureSwitchCertificateRequest,
    ) -> Result<rms::ConfigureSwitchCertificateResponse, RackManagerError> {
        self.configure_switch_certificate_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.configure_switch_certificate_responses.lock().await)
    }

    async fn batch_disable_switch_mtls(
        &self,
        _cmd: rms::BatchDisableSwitchMtlsRequest,
    ) -> Result<rms::BatchDisableSwitchMtlsResponse, RackManagerError> {
        Err(
            tonic::Status::unimplemented("BatchDisableSwitchMtls is not implemented by MockRmsApi")
                .into(),
        )
    }

    async fn get_configure_switch_certificate_job_status(
        &self,
        cmd: rms::GetConfigureSwitchCertificateJobStatusRequest,
    ) -> Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError> {
        self.get_configure_switch_certificate_job_status_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .get_configure_switch_certificate_job_status_responses
                .lock()
                .await,
        )
    }
    async fn list_switch_system_images(
        &self,
        cmd: rms::ListSwitchSystemImagesRequest,
    ) -> Result<rms::ListSwitchSystemImagesResponse, RackManagerError> {
        self.list_switch_system_images_calls.lock().await.push(cmd);
        pop_or_err(&mut self.list_switch_system_images_responses.lock().await)
    }

    async fn get_switch_system_image_job_status(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError> {
        self.get_switch_system_image_job_status_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .get_switch_system_image_job_status_responses
                .lock()
                .await,
        )
    }

    async fn configure_scale_up_fabric_manager(
        &self,
        cmd: rms::ConfigureScaleUpFabricManagerRequest,
    ) -> Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError> {
        self.configure_scale_up_fabric_manager_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .configure_scale_up_fabric_manager_responses
                .lock()
                .await,
        )
    }
    async fn batch_set_scale_up_fabric_state(
        &self,
        cmd: rms::BatchSetScaleUpFabricStateRequest,
    ) -> Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError> {
        self.batch_set_scale_up_fabric_state_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(&mut self.batch_set_scale_up_fabric_state_responses.lock().await)
    }

    async fn batch_get_scale_up_fabric_service_status(
        &self,
        cmd: rms::BatchGetScaleUpFabricServiceStatusRequest,
    ) -> Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError> {
        self.batch_get_scale_up_fabric_service_status_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .batch_get_scale_up_fabric_service_status_responses
                .lock()
                .await,
        )
    }

    async fn get_scale_up_fabric_state(
        &self,
        cmd: rms::GetScaleUpFabricStateRequest,
    ) -> Result<rms::GetScaleUpFabricStateResponse, RackManagerError> {
        self.get_scale_up_fabric_state_calls.lock().await.push(cmd);
        pop_or_err(&mut self.get_scale_up_fabric_state_responses.lock().await)
    }

    async fn set_scale_up_fabric_telemetry_interface_state(
        &self,
        cmd: rms::SetScaleUpFabricTelemetryInterfaceStateRequest,
    ) -> Result<rms::SetScaleUpFabricTelemetryInterfaceStateResponse, RackManagerError> {
        self.set_scale_up_fabric_telemetry_interface_state_calls
            .lock()
            .await
            .push(cmd);
        pop_or_err(
            &mut self
                .set_scale_up_fabric_telemetry_interface_state_responses
                .lock()
                .await,
        )
    }
    async fn get_version(&self) -> Result<rms::GetVersionResponse, RackManagerError> {
        *self.version_call_count.lock().await += 1;
        self.version_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::GetVersionResponse::default()))
    }
}
