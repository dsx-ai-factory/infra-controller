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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
use librms::{RackManagerError, RmsApi};
use tokio::sync::Mutex;

/// RMS simulation for testing, similar to RedfishSim
pub struct RmsSim {
    fail_create_nodes: Arc<AtomicBool>,
    fail_inventory_get: Arc<AtomicBool>,
    registered_nodes: Arc<Mutex<Vec<rms::NodeInventoryInfo>>>,
    firmware_objects: Arc<Mutex<HashMap<String, rms::FirmwareObject>>>,
    submitted_firmware_requests: Arc<Mutex<Vec<rms::BatchUpdateFirmwareRequest>>>,
    queued_firmware_responses: Arc<Mutex<VecDeque<rms::BatchUpdateFirmwareResponse>>>,
    submitted_apply_stored_firmware_object_requests:
        Arc<Mutex<Vec<rms::ApplyStoredFirmwareObjectRequest>>>,
    queued_firmware_object_apply_responses: Arc<Mutex<VecDeque<rms::ApplyFirmwareObjectResponse>>>,
    submitted_apply_firmware_object_requests: Arc<Mutex<Vec<rms::ApplyFirmwareObjectRequest>>>,
    firmware_job_statuses: Arc<Mutex<HashMap<String, rms::GetFirmwareJobStatusResponse>>>,
    firmware_job_errors: Arc<Mutex<HashMap<String, String>>>,
    submitted_apply_switch_system_image_requests:
        Arc<Mutex<Vec<rms::ApplySwitchSystemImageRequest>>>,
    queued_apply_switch_system_image_responses:
        Arc<Mutex<VecDeque<rms::ApplySwitchSystemImageResponse>>>,
    switch_system_image_job_statuses:
        Arc<Mutex<HashMap<String, rms::GetSwitchSystemImageJobStatusResponse>>>,
    switch_system_image_job_errors: Arc<Mutex<HashMap<String, String>>>,
    submitted_batch_get_node_device_info_requests:
        Arc<Mutex<Vec<rms::BatchGetNodeDeviceInfoRequest>>>,
    queued_batch_get_node_device_info_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError>>>>,
    submitted_batch_get_power_state_requests: Arc<Mutex<Vec<rms::BatchGetPowerStateRequest>>>,
    queued_batch_get_power_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchGetPowerStateResponse, RackManagerError>>>>,
    submitted_configure_scale_up_fabric_manager_requests:
        Arc<Mutex<Vec<rms::ConfigureScaleUpFabricManagerRequest>>>,
    queued_configure_scale_up_fabric_manager_responses:
        Arc<Mutex<VecDeque<Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError>>>>,
    submitted_configure_scale_up_fabric_manager_v2_requests:
        Arc<Mutex<Vec<rms_v2::ConfigureScaleUpFabricManagerRequest>>>,
    queued_configure_scale_up_fabric_manager_v2_responses: Arc<
        Mutex<VecDeque<Result<rms_v2::ConfigureScaleUpFabricManagerResponse, RackManagerError>>>,
    >,
    submitted_get_job_status_requests: Arc<Mutex<Vec<rms::GetJobStatusRequest>>>,
    queued_get_job_status_responses:
        Arc<Mutex<VecDeque<Result<rms::GetJobStatusResponse, RackManagerError>>>>,
    submitted_get_scale_up_fabric_status_requests:
        Arc<Mutex<Vec<rms::GetScaleUpFabricStatusRequest>>>,
    queued_get_scale_up_fabric_status_responses:
        Arc<Mutex<VecDeque<Result<rms::GetScaleUpFabricStatusResponse, RackManagerError>>>>,
    submitted_configure_switch_certificate_requests:
        Arc<Mutex<Vec<rms::ConfigureSwitchCertificateRequest>>>,
    queued_configure_switch_certificate_responses:
        Arc<Mutex<VecDeque<Result<rms::ConfigureSwitchCertificateResponse, RackManagerError>>>>,
    submitted_get_configure_switch_certificate_job_status_requests:
        Arc<Mutex<Vec<rms::GetConfigureSwitchCertificateJobStatusRequest>>>,
    queued_get_configure_switch_certificate_job_status_responses: Arc<
        Mutex<
            VecDeque<Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError>>,
        >,
    >,
    submitted_batch_set_scale_up_fabric_state_requests:
        Arc<Mutex<Vec<rms::BatchSetScaleUpFabricStateRequest>>>,
    queued_batch_set_scale_up_fabric_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError>>>>,
    submitted_batch_get_scale_up_fabric_service_status_requests:
        Arc<Mutex<Vec<rms::BatchGetScaleUpFabricServiceStatusRequest>>>,
    queued_batch_get_scale_up_fabric_service_status_responses: Arc<
        Mutex<VecDeque<Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError>>>,
    >,
    submitted_batch_set_power_state_requests: Arc<Mutex<Vec<rms::BatchSetPowerStateRequest>>>,
    queued_batch_set_power_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchSetPowerStateResponse, RackManagerError>>>>,
}

impl Default for RmsSim {
    fn default() -> Self {
        Self {
            fail_create_nodes: Arc::new(AtomicBool::new(false)),
            fail_inventory_get: Arc::new(AtomicBool::new(false)),
            registered_nodes: Arc::new(Mutex::new(Vec::new())),
            firmware_objects: Arc::new(Mutex::new(HashMap::new())),
            submitted_firmware_requests: Arc::new(Mutex::new(Vec::new())),
            queued_firmware_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_apply_stored_firmware_object_requests: Arc::new(Mutex::new(Vec::new())),
            queued_firmware_object_apply_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_apply_firmware_object_requests: Arc::new(Mutex::new(Vec::new())),
            firmware_job_statuses: Arc::new(Mutex::new(HashMap::new())),
            firmware_job_errors: Arc::new(Mutex::new(HashMap::new())),
            submitted_apply_switch_system_image_requests: Arc::new(Mutex::new(Vec::new())),
            queued_apply_switch_system_image_responses: Arc::new(Mutex::new(VecDeque::new())),
            switch_system_image_job_statuses: Arc::new(Mutex::new(HashMap::new())),
            switch_system_image_job_errors: Arc::new(Mutex::new(HashMap::new())),
            submitted_batch_get_node_device_info_requests: Arc::new(Mutex::new(Vec::new())),
            queued_batch_get_node_device_info_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_batch_get_power_state_requests: Arc::new(Mutex::new(Vec::new())),
            queued_batch_get_power_state_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_configure_scale_up_fabric_manager_requests: Arc::new(Mutex::new(Vec::new())),
            queued_configure_scale_up_fabric_manager_responses: Arc::new(Mutex::new(
                VecDeque::new(),
            )),
            submitted_configure_scale_up_fabric_manager_v2_requests: Arc::new(Mutex::new(
                Vec::new(),
            )),
            queued_configure_scale_up_fabric_manager_v2_responses: Arc::new(Mutex::new(
                VecDeque::new(),
            )),
            submitted_get_job_status_requests: Arc::new(Mutex::new(Vec::new())),
            queued_get_job_status_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_get_scale_up_fabric_status_requests: Arc::new(Mutex::new(Vec::new())),
            queued_get_scale_up_fabric_status_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_configure_switch_certificate_requests: Arc::new(Mutex::new(Vec::new())),
            queued_configure_switch_certificate_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_get_configure_switch_certificate_job_status_requests: Arc::new(Mutex::new(
                Vec::new(),
            )),
            queued_get_configure_switch_certificate_job_status_responses: Arc::new(Mutex::new(
                VecDeque::new(),
            )),
            submitted_batch_set_scale_up_fabric_state_requests: Arc::new(Mutex::new(Vec::new())),
            queued_batch_set_scale_up_fabric_state_responses: Arc::new(Mutex::new(VecDeque::new())),
            submitted_batch_get_scale_up_fabric_service_status_requests: Arc::new(Mutex::new(
                Vec::new(),
            )),
            queued_batch_get_scale_up_fabric_service_status_responses: Arc::new(Mutex::new(
                VecDeque::new(),
            )),
            submitted_batch_set_power_state_requests: Arc::new(Mutex::new(Vec::new())),
            queued_batch_set_power_state_responses: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl RmsSim {
    /// Convert RmsSim to the type expected by Api and StateHandlerServices
    pub fn as_rms_client(&self) -> Option<Arc<dyn RmsApi>> {
        Some(Arc::new(self.build_mock_client()))
    }

    fn build_mock_client(&self) -> MockRmsClient {
        MockRmsClient {
            fail_create_nodes: self.fail_create_nodes.clone(),
            fail_inventory_get: self.fail_inventory_get.clone(),
            registered_nodes: self.registered_nodes.clone(),
            firmware_objects: self.firmware_objects.clone(),
            submitted_firmware_requests: self.submitted_firmware_requests.clone(),
            queued_firmware_responses: self.queued_firmware_responses.clone(),
            submitted_apply_stored_firmware_object_requests: self
                .submitted_apply_stored_firmware_object_requests
                .clone(),
            queued_firmware_object_apply_responses: self
                .queued_firmware_object_apply_responses
                .clone(),
            submitted_apply_firmware_object_requests: self
                .submitted_apply_firmware_object_requests
                .clone(),
            firmware_job_statuses: self.firmware_job_statuses.clone(),
            firmware_job_errors: self.firmware_job_errors.clone(),
            submitted_apply_switch_system_image_requests: self
                .submitted_apply_switch_system_image_requests
                .clone(),
            queued_apply_switch_system_image_responses: self
                .queued_apply_switch_system_image_responses
                .clone(),
            switch_system_image_job_statuses: self.switch_system_image_job_statuses.clone(),
            switch_system_image_job_errors: self.switch_system_image_job_errors.clone(),
            submitted_batch_get_node_device_info_requests: self
                .submitted_batch_get_node_device_info_requests
                .clone(),
            queued_batch_get_node_device_info_responses: self
                .queued_batch_get_node_device_info_responses
                .clone(),
            submitted_batch_get_power_state_requests: self
                .submitted_batch_get_power_state_requests
                .clone(),
            queued_batch_get_power_state_responses: self
                .queued_batch_get_power_state_responses
                .clone(),
            submitted_configure_scale_up_fabric_manager_requests: self
                .submitted_configure_scale_up_fabric_manager_requests
                .clone(),
            queued_configure_scale_up_fabric_manager_responses: self
                .queued_configure_scale_up_fabric_manager_responses
                .clone(),
            submitted_configure_scale_up_fabric_manager_v2_requests: self
                .submitted_configure_scale_up_fabric_manager_v2_requests
                .clone(),
            queued_configure_scale_up_fabric_manager_v2_responses: self
                .queued_configure_scale_up_fabric_manager_v2_responses
                .clone(),
            submitted_get_job_status_requests: self.submitted_get_job_status_requests.clone(),
            queued_get_job_status_responses: self.queued_get_job_status_responses.clone(),
            submitted_get_scale_up_fabric_status_requests: self
                .submitted_get_scale_up_fabric_status_requests
                .clone(),
            queued_get_scale_up_fabric_status_responses: self
                .queued_get_scale_up_fabric_status_responses
                .clone(),
            submitted_configure_switch_certificate_requests: self
                .submitted_configure_switch_certificate_requests
                .clone(),
            queued_configure_switch_certificate_responses: self
                .queued_configure_switch_certificate_responses
                .clone(),
            submitted_get_configure_switch_certificate_job_status_requests: self
                .submitted_get_configure_switch_certificate_job_status_requests
                .clone(),
            queued_get_configure_switch_certificate_job_status_responses: self
                .queued_get_configure_switch_certificate_job_status_responses
                .clone(),
            submitted_batch_set_scale_up_fabric_state_requests: self
                .submitted_batch_set_scale_up_fabric_state_requests
                .clone(),
            queued_batch_set_scale_up_fabric_state_responses: self
                .queued_batch_set_scale_up_fabric_state_responses
                .clone(),
            submitted_batch_get_scale_up_fabric_service_status_requests: self
                .submitted_batch_get_scale_up_fabric_service_status_requests
                .clone(),
            queued_batch_get_scale_up_fabric_service_status_responses: self
                .queued_batch_get_scale_up_fabric_service_status_responses
                .clone(),
            submitted_batch_set_power_state_requests: self
                .submitted_batch_set_power_state_requests
                .clone(),
            queued_batch_set_power_state_responses: self
                .queued_batch_set_power_state_responses
                .clone(),
        }
    }

    /// Set whether `create_nodes` should return an error for testing
    /// if registration attempts are failing (and should retry).
    pub fn set_fail_create_nodes(&self, fail: bool) {
        self.fail_create_nodes.store(fail, Ordering::Relaxed);
    }

    /// Set whether `inventory_get` should return an error for
    /// testing things like whether RMS membership verification
    /// should retry, or going back to re-registration (or moving
    /// forward thanks to successful registration verification).
    pub fn set_fail_inventory_get(&self, fail: bool) {
        self.fail_inventory_get.store(fail, Ordering::Relaxed);
    }

    pub async fn queue_update_firmware_response(&self, response: rms::BatchUpdateFirmwareResponse) {
        self.queued_firmware_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn set_firmware_job_status(&self, response: rms::GetFirmwareJobStatusResponse) {
        self.firmware_job_statuses
            .lock()
            .await
            .insert(response.job_id.clone(), response);
    }

    pub async fn set_firmware_job_error(
        &self,
        job_id: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.firmware_job_errors
            .lock()
            .await
            .insert(job_id.into(), message.into());
    }

    pub async fn submitted_firmware_requests(&self) -> Vec<rms::BatchUpdateFirmwareRequest> {
        self.submitted_firmware_requests.lock().await.clone()
    }

    pub async fn queue_apply_firmware_object_response(
        &self,
        response: rms::ApplyFirmwareObjectResponse,
    ) {
        self.queued_firmware_object_apply_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn insert_firmware_object(&self, object: rms::FirmwareObject) {
        self.firmware_objects
            .lock()
            .await
            .insert(object.id.clone(), object);
    }

    pub async fn submitted_apply_firmware_object_requests(
        &self,
    ) -> Vec<rms::ApplyFirmwareObjectRequest> {
        self.submitted_apply_firmware_object_requests
            .lock()
            .await
            .clone()
    }

    pub async fn submitted_apply_stored_firmware_object_requests(
        &self,
    ) -> Vec<rms::ApplyStoredFirmwareObjectRequest> {
        self.submitted_apply_stored_firmware_object_requests
            .lock()
            .await
            .clone()
    }

    pub async fn set_switch_system_image_job_status(
        &self,
        response: rms::GetSwitchSystemImageJobStatusResponse,
    ) {
        self.switch_system_image_job_statuses
            .lock()
            .await
            .insert(response.job_id.clone(), response);
    }

    pub async fn set_switch_system_image_job_error(
        &self,
        job_id: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.switch_system_image_job_errors
            .lock()
            .await
            .insert(job_id.into(), message.into());
    }

    pub async fn queue_apply_switch_system_image_response(
        &self,
        response: rms::ApplySwitchSystemImageResponse,
    ) {
        self.queued_apply_switch_system_image_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_apply_switch_system_image_requests(
        &self,
    ) -> Vec<rms::ApplySwitchSystemImageRequest> {
        self.submitted_apply_switch_system_image_requests
            .lock()
            .await
            .clone()
    }

    pub async fn queue_batch_get_node_device_info_response(
        &self,
        response: Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError>,
    ) {
        self.queued_batch_get_node_device_info_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_batch_get_node_device_info_requests(
        &self,
    ) -> Vec<rms::BatchGetNodeDeviceInfoRequest> {
        self.submitted_batch_get_node_device_info_requests
            .lock()
            .await
            .clone()
    }

    /// Queue a `Result` to be returned on the next call to
    /// `batch_get_power_state`.
    pub async fn queue_batch_get_power_state_response(
        &self,
        response: Result<rms::BatchGetPowerStateResponse, RackManagerError>,
    ) {
        self.queued_batch_get_power_state_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Snapshot recorded `BatchGetPowerState` requests in call order.
    pub async fn submitted_batch_get_power_state_requests(
        &self,
    ) -> Vec<rms::BatchGetPowerStateRequest> {
        self.submitted_batch_get_power_state_requests
            .lock()
            .await
            .clone()
    }

    pub async fn queue_configure_scale_up_fabric_manager_response(
        &self,
        response: Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError>,
    ) {
        self.queued_configure_scale_up_fabric_manager_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_configure_scale_up_fabric_manager_requests(
        &self,
    ) -> Vec<rms::ConfigureScaleUpFabricManagerRequest> {
        self.submitted_configure_scale_up_fabric_manager_requests
            .lock()
            .await
            .clone()
    }

    /// Queues one V2 scale-up fabric response for FIFO consumption.
    ///
    /// The fake returns `Ok(Default::default())` after the queue is
    /// exhausted.
    pub async fn queue_configure_scale_up_fabric_manager_v2_response(
        &self,
        response: Result<rms_v2::ConfigureScaleUpFabricManagerResponse, RackManagerError>,
    ) {
        self.queued_configure_scale_up_fabric_manager_v2_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Returns recorded V2 scale-up fabric requests in submission order.
    pub async fn submitted_configure_scale_up_fabric_manager_v2_requests(
        &self,
    ) -> Vec<rms_v2::ConfigureScaleUpFabricManagerRequest> {
        self.submitted_configure_scale_up_fabric_manager_v2_requests
            .lock()
            .await
            .clone()
    }

    /// Queues one generic RMS job-status response for FIFO consumption.
    ///
    /// The fake returns `Ok(Default::default())` after the queue is
    /// exhausted.
    pub async fn queue_get_job_status_response(
        &self,
        response: Result<rms::GetJobStatusResponse, RackManagerError>,
    ) {
        self.queued_get_job_status_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Returns recorded generic RMS job-status requests in submission order.
    pub async fn submitted_get_job_status_requests(&self) -> Vec<rms::GetJobStatusRequest> {
        self.submitted_get_job_status_requests.lock().await.clone()
    }

    /// Queues one scale-up fabric status response for FIFO consumption.
    ///
    /// The fake returns `Ok(Default::default())` after the queue is
    /// exhausted.
    pub async fn queue_get_scale_up_fabric_status_response(
        &self,
        response: Result<rms::GetScaleUpFabricStatusResponse, RackManagerError>,
    ) {
        self.queued_get_scale_up_fabric_status_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Returns recorded scale-up fabric status requests in submission order.
    pub async fn submitted_get_scale_up_fabric_status_requests(
        &self,
    ) -> Vec<rms::GetScaleUpFabricStatusRequest> {
        self.submitted_get_scale_up_fabric_status_requests
            .lock()
            .await
            .clone()
    }

    pub async fn queue_configure_switch_certificate_response(
        &self,
        response: Result<rms::ConfigureSwitchCertificateResponse, RackManagerError>,
    ) {
        self.queued_configure_switch_certificate_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_configure_switch_certificate_requests(
        &self,
    ) -> Vec<rms::ConfigureSwitchCertificateRequest> {
        self.submitted_configure_switch_certificate_requests
            .lock()
            .await
            .clone()
    }

    pub async fn queue_get_configure_switch_certificate_job_status_response(
        &self,
        response: Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError>,
    ) {
        self.queued_get_configure_switch_certificate_job_status_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_get_configure_switch_certificate_job_status_requests(
        &self,
    ) -> Vec<rms::GetConfigureSwitchCertificateJobStatusRequest> {
        self.submitted_get_configure_switch_certificate_job_status_requests
            .lock()
            .await
            .clone()
    }

    pub async fn queue_batch_set_scale_up_fabric_state_response(
        &self,
        response: Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError>,
    ) {
        self.queued_batch_set_scale_up_fabric_state_responses
            .lock()
            .await
            .push_back(response);
    }

    pub async fn submitted_batch_set_scale_up_fabric_state_requests(
        &self,
    ) -> Vec<rms::BatchSetScaleUpFabricStateRequest> {
        self.submitted_batch_set_scale_up_fabric_state_requests
            .lock()
            .await
            .clone()
    }

    /// Queue a response for the next `batch_get_scale_up_fabric_service_status` call.
    pub async fn queue_batch_get_scale_up_fabric_service_status_response(
        &self,
        response: Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError>,
    ) {
        self.queued_batch_get_scale_up_fabric_service_status_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Snapshot recorded `BatchGetScaleUpFabricServiceStatus` requests in call order.
    pub async fn submitted_batch_get_scale_up_fabric_service_status_requests(
        &self,
    ) -> Vec<rms::BatchGetScaleUpFabricServiceStatusRequest> {
        self.submitted_batch_get_scale_up_fabric_service_status_requests
            .lock()
            .await
            .clone()
    }

    /// Queue a `Result` to be returned on the next call to
    /// `batch_set_power_state`. Used by power-shelf maintenance
    /// tests to drive both the success and failure paths of the
    /// caller-supplied `BatchSetPowerState` RPC.
    pub async fn queue_batch_set_power_state_response(
        &self,
        response: Result<rms::BatchSetPowerStateResponse, RackManagerError>,
    ) {
        self.queued_batch_set_power_state_responses
            .lock()
            .await
            .push_back(response);
    }

    /// Snapshot the recorded `BatchSetPowerState` requests, in
    /// the order they were received.
    pub async fn submitted_batch_set_power_state_requests(
        &self,
    ) -> Vec<rms::BatchSetPowerStateRequest> {
        self.submitted_batch_set_power_state_requests
            .lock()
            .await
            .clone()
    }
}

#[derive(Debug, Clone)]
pub struct MockRmsClient {
    fail_create_nodes: Arc<AtomicBool>,
    fail_inventory_get: Arc<AtomicBool>,
    registered_nodes: Arc<Mutex<Vec<rms::NodeInventoryInfo>>>,
    firmware_objects: Arc<Mutex<HashMap<String, rms::FirmwareObject>>>,
    submitted_firmware_requests: Arc<Mutex<Vec<rms::BatchUpdateFirmwareRequest>>>,
    queued_firmware_responses: Arc<Mutex<VecDeque<rms::BatchUpdateFirmwareResponse>>>,
    submitted_apply_stored_firmware_object_requests:
        Arc<Mutex<Vec<rms::ApplyStoredFirmwareObjectRequest>>>,
    queued_firmware_object_apply_responses: Arc<Mutex<VecDeque<rms::ApplyFirmwareObjectResponse>>>,
    submitted_apply_firmware_object_requests: Arc<Mutex<Vec<rms::ApplyFirmwareObjectRequest>>>,
    firmware_job_statuses: Arc<Mutex<HashMap<String, rms::GetFirmwareJobStatusResponse>>>,
    firmware_job_errors: Arc<Mutex<HashMap<String, String>>>,
    submitted_apply_switch_system_image_requests:
        Arc<Mutex<Vec<rms::ApplySwitchSystemImageRequest>>>,
    queued_apply_switch_system_image_responses:
        Arc<Mutex<VecDeque<rms::ApplySwitchSystemImageResponse>>>,
    switch_system_image_job_statuses:
        Arc<Mutex<HashMap<String, rms::GetSwitchSystemImageJobStatusResponse>>>,
    switch_system_image_job_errors: Arc<Mutex<HashMap<String, String>>>,
    submitted_batch_get_node_device_info_requests:
        Arc<Mutex<Vec<rms::BatchGetNodeDeviceInfoRequest>>>,
    queued_batch_get_node_device_info_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError>>>>,
    submitted_batch_get_power_state_requests: Arc<Mutex<Vec<rms::BatchGetPowerStateRequest>>>,
    queued_batch_get_power_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchGetPowerStateResponse, RackManagerError>>>>,
    submitted_configure_scale_up_fabric_manager_requests:
        Arc<Mutex<Vec<rms::ConfigureScaleUpFabricManagerRequest>>>,
    queued_configure_scale_up_fabric_manager_responses:
        Arc<Mutex<VecDeque<Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError>>>>,
    submitted_configure_scale_up_fabric_manager_v2_requests:
        Arc<Mutex<Vec<rms_v2::ConfigureScaleUpFabricManagerRequest>>>,
    queued_configure_scale_up_fabric_manager_v2_responses: Arc<
        Mutex<VecDeque<Result<rms_v2::ConfigureScaleUpFabricManagerResponse, RackManagerError>>>,
    >,
    submitted_get_job_status_requests: Arc<Mutex<Vec<rms::GetJobStatusRequest>>>,
    queued_get_job_status_responses:
        Arc<Mutex<VecDeque<Result<rms::GetJobStatusResponse, RackManagerError>>>>,
    submitted_get_scale_up_fabric_status_requests:
        Arc<Mutex<Vec<rms::GetScaleUpFabricStatusRequest>>>,
    queued_get_scale_up_fabric_status_responses:
        Arc<Mutex<VecDeque<Result<rms::GetScaleUpFabricStatusResponse, RackManagerError>>>>,
    submitted_configure_switch_certificate_requests:
        Arc<Mutex<Vec<rms::ConfigureSwitchCertificateRequest>>>,
    queued_configure_switch_certificate_responses:
        Arc<Mutex<VecDeque<Result<rms::ConfigureSwitchCertificateResponse, RackManagerError>>>>,
    submitted_get_configure_switch_certificate_job_status_requests:
        Arc<Mutex<Vec<rms::GetConfigureSwitchCertificateJobStatusRequest>>>,
    queued_get_configure_switch_certificate_job_status_responses: Arc<
        Mutex<
            VecDeque<Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError>>,
        >,
    >,
    submitted_batch_set_scale_up_fabric_state_requests:
        Arc<Mutex<Vec<rms::BatchSetScaleUpFabricStateRequest>>>,
    queued_batch_set_scale_up_fabric_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError>>>>,
    submitted_batch_get_scale_up_fabric_service_status_requests:
        Arc<Mutex<Vec<rms::BatchGetScaleUpFabricServiceStatusRequest>>>,
    queued_batch_get_scale_up_fabric_service_status_responses: Arc<
        Mutex<VecDeque<Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError>>>,
    >,
    submitted_batch_set_power_state_requests: Arc<Mutex<Vec<rms::BatchSetPowerStateRequest>>>,
    queued_batch_set_power_state_responses:
        Arc<Mutex<VecDeque<Result<rms::BatchSetPowerStateResponse, RackManagerError>>>>,
}

#[async_trait::async_trait]
impl RmsApi for MockRmsClient {
    async fn batch_get_node_device_info(
        &self,
        cmd: rms::BatchGetNodeDeviceInfoRequest,
    ) -> Result<rms::BatchGetNodeDeviceInfoResponse, RackManagerError> {
        self.submitted_batch_get_node_device_info_requests
            .lock()
            .await
            .push(cmd);

        self.queued_batch_get_node_device_info_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::BatchGetNodeDeviceInfoResponse::default()))
    }

    async fn get_node_device_info(
        &self,
        _cmd: rms::GetNodeDeviceInfoRequest,
    ) -> Result<rms::GetNodeDeviceInfoResponse, RackManagerError> {
        Ok(rms::GetNodeDeviceInfoResponse::default())
    }

    async fn list_node_device_info_by_node_type(
        &self,
        _cmd: rms::ListNodeDeviceInfoByNodeTypeRequest,
    ) -> Result<rms::ListNodeDeviceInfoByNodeTypeResponse, RackManagerError> {
        Ok(rms::ListNodeDeviceInfoByNodeTypeResponse::default())
    }

    async fn batch_update_firmware(
        &self,
        cmd: rms::BatchUpdateFirmwareRequest,
    ) -> Result<rms::BatchUpdateFirmwareResponse, RackManagerError> {
        self.submitted_firmware_requests.lock().await.push(cmd);
        Ok(self
            .queued_firmware_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or_default())
    }

    async fn update_switch_system_password(
        &self,
        _cmd: rms::UpdateSwitchSystemPasswordRequest,
    ) -> Result<rms::UpdateSwitchSystemPasswordResponse, RackManagerError> {
        Ok(rms::UpdateSwitchSystemPasswordResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                job_id: "rms-sim-password-update".into(),
                ..Default::default()
            }),
        })
    }

    async fn batch_reset_switch_sdn_factory_default(
        &self,
        _cmd: rms::BatchResetSwitchSdnFactoryDefaultRequest,
    ) -> Result<rms::BatchResetSwitchSdnFactoryDefaultResponse, RackManagerError> {
        Ok(rms::BatchResetSwitchSdnFactoryDefaultResponse::default())
    }

    async fn get_job_status(
        &self,
        cmd: rms::GetJobStatusRequest,
    ) -> Result<rms::GetJobStatusResponse, RackManagerError> {
        self.submitted_get_job_status_requests
            .lock()
            .await
            .push(cmd);

        self.queued_get_job_status_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::GetJobStatusResponse::default()))
    }

    async fn get_scale_up_fabric_status(
        &self,
        cmd: rms::GetScaleUpFabricStatusRequest,
    ) -> Result<rms::GetScaleUpFabricStatusResponse, RackManagerError> {
        self.submitted_get_scale_up_fabric_status_requests
            .lock()
            .await
            .push(cmd);

        self.queued_get_scale_up_fabric_status_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::GetScaleUpFabricStatusResponse::default()))
    }

    async fn set_power_state(
        &self,
        _cmd: rms::SetPowerStateRequest,
    ) -> Result<rms::SetPowerStateResponse, RackManagerError> {
        Ok(rms::SetPowerStateResponse::default())
    }

    async fn batch_set_power_state(
        &self,
        cmd: rms::BatchSetPowerStateRequest,
    ) -> Result<rms::BatchSetPowerStateResponse, RackManagerError> {
        self.submitted_batch_set_power_state_requests
            .lock()
            .await
            .push(cmd);

        self.queued_batch_set_power_state_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::BatchSetPowerStateResponse::default()))
    }

    async fn get_power_state(
        &self,
        _cmd: rms::GetPowerStateRequest,
    ) -> Result<rms::GetPowerStateResponse, RackManagerError> {
        Ok(rms::GetPowerStateResponse::default())
    }

    async fn batch_get_power_state(
        &self,
        cmd: rms::BatchGetPowerStateRequest,
    ) -> Result<rms::BatchGetPowerStateResponse, RackManagerError> {
        self.submitted_batch_get_power_state_requests
            .lock()
            .await
            .push(cmd);

        self.queued_batch_get_power_state_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::BatchGetPowerStateResponse::default()))
    }

    async fn sequence_rack_power(
        &self,
        _cmd: rms::SequenceRackPowerRequest,
    ) -> Result<rms::SequenceRackPowerResponse, RackManagerError> {
        Ok(rms::SequenceRackPowerResponse::default())
    }

    async fn list_node_inventory(
        &self,
    ) -> Result<rms::ListNodeInventoryResponse, RackManagerError> {
        if self.fail_inventory_get.load(Ordering::Relaxed) {
            return Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable("mock RMS inventory_get failure"),
            ));
        }

        let nodes = self.registered_nodes.lock().await.clone();
        Ok(rms::ListNodeInventoryResponse { nodes })
    }

    async fn create_nodes(
        &self,
        cmd: rms::CreateNodesRequest,
    ) -> Result<rms::CreateNodesResponse, RackManagerError> {
        if self.fail_create_nodes.load(Ordering::Relaxed) {
            return Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable("mock RMS create_nodes failure"),
            ));
        }

        // Track registered nodes so inventory_get can find them,
        // just like a real RMS would.
        let mut registered = self.registered_nodes.lock().await;

        if let Some(nodes) = cmd.nodes {
            for node in nodes.nodes {
                registered.push(librms::protos::rack_manager::NodeInventoryInfo {
                    node_id: node.node_id.clone(),
                    rack_id: node.rack_id.clone(),
                    r#type: node.r#type.unwrap_or(0),
                    node_descriptor: node.node_descriptor.clone(),
                    ..Default::default()
                });
            }
        }

        Ok(rms::CreateNodesResponse::default())
    }

    async fn update_node(
        &self,
        _cmd: rms::UpdateNodeRequest,
    ) -> Result<rms::UpdateNodeResponse, RackManagerError> {
        Ok(rms::UpdateNodeResponse::default())
    }

    async fn delete_node(
        &self,
        _cmd: rms::DeleteNodeRequest,
    ) -> Result<rms::DeleteNodeResponse, RackManagerError> {
        Ok(rms::DeleteNodeResponse::default())
    }

    async fn get_rack_power_on_sequence(
        &self,
        _cmd: rms::GetRackPowerOnSequenceRequest,
    ) -> Result<rms::GetRackPowerOnSequenceResponse, RackManagerError> {
        Ok(rms::GetRackPowerOnSequenceResponse::default())
    }

    async fn set_rack_power_on_sequence(
        &self,
        _cmd: rms::SetRackPowerOnSequenceRequest,
    ) -> Result<rms::SetRackPowerOnSequenceResponse, RackManagerError> {
        Ok(rms::SetRackPowerOnSequenceResponse::default())
    }

    async fn list_racks(&self) -> Result<rms::ListRacksResponse, RackManagerError> {
        Ok(rms::ListRacksResponse::default())
    }

    async fn get_node_firmware_inventory(
        &self,
        _cmd: rms::GetNodeFirmwareInventoryRequest,
    ) -> Result<rms::GetNodeFirmwareInventoryResponse, RackManagerError> {
        Ok(rms::GetNodeFirmwareInventoryResponse::default())
    }

    async fn get_rack_firmware_inventory(
        &self,
        _cmd: rms::GetRackFirmwareInventoryRequest,
    ) -> Result<rms::GetRackFirmwareInventoryResponse, RackManagerError> {
        Ok(rms::GetRackFirmwareInventoryResponse::default())
    }

    async fn add_firmware_object(
        &self,
        cmd: rms::AddFirmwareObjectRequest,
    ) -> Result<rms::AddFirmwareObjectResponse, RackManagerError> {
        #[derive(serde::Deserialize)]
        struct FirmwareObjectConfig {
            #[serde(rename = "Id")]
            id: String,
        }

        let config =
            serde_json::from_str::<FirmwareObjectConfig>(&cmd.config_json).map_err(|e| {
                RackManagerError::ApiInvocationError(tonic::Status::invalid_argument(format!(
                    "invalid config_json: {e}"
                )))
            })?;

        if config.id.is_empty() {
            return Err(RackManagerError::ApiInvocationError(
                tonic::Status::invalid_argument("config_json must contain non-empty id"),
            ));
        }

        let id = config.id;

        let mut objects = self.firmware_objects.lock().await;

        let is_default = cmd.set_default
            || !objects
                .values()
                .any(|existing| existing.hardware_type == cmd.hardware_type && existing.is_default);

        let object = rms::FirmwareObject {
            id: id.clone(),
            config_json: cmd.config_json,
            available: false,
            hardware_type: cmd.hardware_type,
            is_default,
            ..Default::default()
        };

        if is_default {
            for existing in objects.values_mut() {
                if existing.hardware_type == object.hardware_type {
                    existing.is_default = false;
                }
            }
        }

        objects.insert(id, object.clone());

        Ok(rms::AddFirmwareObjectResponse {
            object: Some(object),
        })
    }

    async fn get_firmware_object(
        &self,
        cmd: rms::GetFirmwareObjectRequest,
    ) -> Result<rms::GetFirmwareObjectResponse, RackManagerError> {
        let object = self
            .firmware_objects
            .lock()
            .await
            .get(&cmd.id)
            .cloned()
            .ok_or_else(|| {
                RackManagerError::ApiInvocationError(tonic::Status::not_found(format!(
                    "firmware object {} not found",
                    cmd.id
                )))
            })?;

        Ok(rms::GetFirmwareObjectResponse {
            object: Some(object),
        })
    }

    async fn list_firmware_objects(
        &self,
        cmd: rms::ListFirmwareObjectsRequest,
    ) -> Result<rms::ListFirmwareObjectsResponse, RackManagerError> {
        let objects = self
            .firmware_objects
            .lock()
            .await
            .values()
            .filter(|object| !cmd.only_available || object.available)
            .filter(|object| {
                cmd.hardware_type.is_empty() || object.hardware_type == cmd.hardware_type
            })
            .cloned()
            .collect();

        Ok(rms::ListFirmwareObjectsResponse { objects })
    }

    async fn delete_firmware_object(
        &self,
        cmd: rms::DeleteFirmwareObjectRequest,
    ) -> Result<rms::DeleteFirmwareObjectResponse, RackManagerError> {
        self.firmware_objects.lock().await.remove(&cmd.id);

        Ok(rms::DeleteFirmwareObjectResponse {
            response: Some(rms::OperationResponse {
                status: rms::ReturnCode::Success as i32,
                ..Default::default()
            }),
        })
    }

    async fn set_default_firmware_object(
        &self,
        cmd: rms::SetDefaultFirmwareObjectRequest,
    ) -> Result<rms::SetDefaultFirmwareObjectResponse, RackManagerError> {
        let mut objects = self.firmware_objects.lock().await;

        let hardware_type = objects
            .get(&cmd.object_id)
            .map(|object| object.hardware_type.clone())
            .ok_or_else(|| {
                RackManagerError::ApiInvocationError(tonic::Status::not_found(format!(
                    "firmware object {} not found",
                    cmd.object_id
                )))
            })?;

        for object in objects.values_mut() {
            if object.hardware_type == hardware_type {
                object.is_default = object.id == cmd.object_id;
            }
        }

        let Some(object) = objects.get(&cmd.object_id).cloned() else {
            return Err(RackManagerError::ApiInvocationError(
                tonic::Status::not_found(format!("firmware object {} not found", cmd.object_id)),
            ));
        };

        Ok(rms::SetDefaultFirmwareObjectResponse {
            object: Some(object),
        })
    }

    async fn apply_stored_firmware_object(
        &self,
        cmd: rms::ApplyStoredFirmwareObjectRequest,
    ) -> Result<rms::ApplyStoredFirmwareObjectResponse, RackManagerError> {
        self.submitted_apply_stored_firmware_object_requests
            .lock()
            .await
            .push(cmd);

        Ok(rms::ApplyStoredFirmwareObjectResponse::default())
    }

    async fn apply_firmware_object(
        &self,
        cmd: rms::ApplyFirmwareObjectRequest,
    ) -> Result<rms::ApplyFirmwareObjectResponse, RackManagerError> {
        self.submitted_apply_firmware_object_requests
            .lock()
            .await
            .push(cmd);

        Ok(self
            .queued_firmware_object_apply_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or_default())
    }

    async fn update_switch_system_image(
        &self,
        _cmd: rms::UpdateSwitchSystemImageRequest,
    ) -> Result<rms::UpdateSwitchSystemImageResponse, RackManagerError> {
        Ok(rms::UpdateSwitchSystemImageResponse::default())
    }

    async fn apply_stored_switch_system_image(
        &self,
        _cmd: rms::ApplyStoredSwitchSystemImageRequest,
    ) -> Result<rms::ApplyStoredSwitchSystemImageResponse, RackManagerError> {
        Ok(rms::ApplyStoredSwitchSystemImageResponse::default())
    }

    async fn apply_switch_system_image(
        &self,
        cmd: rms::ApplySwitchSystemImageRequest,
    ) -> Result<rms::ApplySwitchSystemImageResponse, RackManagerError> {
        self.submitted_apply_switch_system_image_requests
            .lock()
            .await
            .push(cmd);

        Ok(self
            .queued_apply_switch_system_image_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or_default())
    }

    async fn get_firmware_object_history(
        &self,
        _cmd: rms::GetFirmwareObjectHistoryRequest,
    ) -> Result<rms::GetFirmwareObjectHistoryResponse, RackManagerError> {
        Ok(rms::GetFirmwareObjectHistoryResponse::default())
    }

    async fn list_switch_firmware(
        &self,
        _cmd: rms::ListSwitchFirmwareRequest,
    ) -> Result<rms::ListSwitchFirmwareResponse, RackManagerError> {
        Ok(rms::ListSwitchFirmwareResponse::default())
    }

    async fn push_switch_firmware(
        &self,
        _cmd: rms::PushSwitchFirmwareRequest,
    ) -> Result<rms::PushSwitchFirmwareResponse, RackManagerError> {
        Ok(rms::PushSwitchFirmwareResponse::default())
    }

    async fn configure_scale_up_fabric_manager(
        &self,
        cmd: rms::ConfigureScaleUpFabricManagerRequest,
    ) -> Result<rms::ConfigureScaleUpFabricManagerResponse, RackManagerError> {
        self.submitted_configure_scale_up_fabric_manager_requests
            .lock()
            .await
            .push(cmd);

        self.queued_configure_scale_up_fabric_manager_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::ConfigureScaleUpFabricManagerResponse::default()))
    }

    async fn configure_scale_up_fabric_manager_v2(
        &self,
        cmd: rms_v2::ConfigureScaleUpFabricManagerRequest,
    ) -> Result<rms_v2::ConfigureScaleUpFabricManagerResponse, RackManagerError> {
        self.submitted_configure_scale_up_fabric_manager_v2_requests
            .lock()
            .await
            .push(cmd);

        self.queued_configure_scale_up_fabric_manager_v2_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms_v2::ConfigureScaleUpFabricManagerResponse::default()))
    }

    async fn configure_switch_certificate(
        &self,
        cmd: rms::ConfigureSwitchCertificateRequest,
    ) -> Result<rms::ConfigureSwitchCertificateResponse, RackManagerError> {
        self.submitted_configure_switch_certificate_requests
            .lock()
            .await
            .push(cmd);

        self.queued_configure_switch_certificate_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::ConfigureSwitchCertificateResponse {
                response: Some(rms::NodeBatchResponse {
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                }),
                ..Default::default()
            }))
    }

    async fn batch_disable_switch_mtls(
        &self,
        _cmd: rms::BatchDisableSwitchMtlsRequest,
    ) -> Result<rms::BatchDisableSwitchMtlsResponse, RackManagerError> {
        Ok(rms::BatchDisableSwitchMtlsResponse::default())
    }

    async fn get_configure_switch_certificate_job_status(
        &self,
        cmd: rms::GetConfigureSwitchCertificateJobStatusRequest,
    ) -> Result<rms::GetConfigureSwitchCertificateJobStatusResponse, RackManagerError> {
        self.submitted_get_configure_switch_certificate_job_status_requests
            .lock()
            .await
            .push(cmd);

        self.queued_get_configure_switch_certificate_job_status_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::GetConfigureSwitchCertificateJobStatusResponse {
                status: rms::ReturnCode::Success as i32,
                state: "running".to_string(),
                ..Default::default()
            }))
    }

    async fn batch_set_scale_up_fabric_state(
        &self,
        cmd: rms::BatchSetScaleUpFabricStateRequest,
    ) -> Result<rms::BatchSetScaleUpFabricStateResponse, RackManagerError> {
        self.submitted_batch_set_scale_up_fabric_state_requests
            .lock()
            .await
            .push(cmd);

        self.queued_batch_set_scale_up_fabric_state_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(rms::BatchSetScaleUpFabricStateResponse::default()))
    }

    async fn batch_get_scale_up_fabric_service_status(
        &self,
        cmd: rms::BatchGetScaleUpFabricServiceStatusRequest,
    ) -> Result<rms::BatchGetScaleUpFabricServiceStatusResponse, RackManagerError> {
        self.submitted_batch_get_scale_up_fabric_service_status_requests
            .lock()
            .await
            .push(cmd);

        self.queued_batch_get_scale_up_fabric_service_status_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(Ok(
                rms::BatchGetScaleUpFabricServiceStatusResponse::default(),
            ))
    }

    async fn get_scale_up_fabric_state(
        &self,
        _cmd: rms::GetScaleUpFabricStateRequest,
    ) -> Result<rms::GetScaleUpFabricStateResponse, RackManagerError> {
        Ok(rms::GetScaleUpFabricStateResponse::default())
    }

    async fn list_switch_system_images(
        &self,
        _cmd: rms::ListSwitchSystemImagesRequest,
    ) -> Result<rms::ListSwitchSystemImagesResponse, RackManagerError> {
        Ok(rms::ListSwitchSystemImagesResponse::default())
    }

    async fn set_scale_up_fabric_telemetry_interface_state(
        &self,
        _cmd: rms::SetScaleUpFabricTelemetryInterfaceStateRequest,
    ) -> Result<rms::SetScaleUpFabricTelemetryInterfaceStateResponse, RackManagerError> {
        Ok(rms::SetScaleUpFabricTelemetryInterfaceStateResponse::default())
    }

    async fn get_switch_system_image_job_status(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, RackManagerError> {
        self.switch_system_image_job_status(cmd)
            .await
            .map_err(RackManagerError::ApiInvocationError)
    }

    async fn get_version(&self) -> Result<rms::GetVersionResponse, RackManagerError> {
        Ok(rms::GetVersionResponse::default())
    }

    async fn update_firmware(
        &self,
        _cmd: rms::UpdateFirmwareRequest,
    ) -> Result<rms::UpdateFirmwareResponse, RackManagerError> {
        Ok(rms::UpdateFirmwareResponse::default())
    }

    async fn batch_update_firmware_by_node_type(
        &self,
        _cmd: rms::BatchUpdateFirmwareByNodeTypeRequest,
    ) -> Result<rms::BatchUpdateFirmwareByNodeTypeResponse, RackManagerError> {
        Ok(rms::BatchUpdateFirmwareByNodeTypeResponse::default())
    }

    async fn get_firmware_job_status(
        &self,
        cmd: rms::GetFirmwareJobStatusRequest,
    ) -> Result<rms::GetFirmwareJobStatusResponse, RackManagerError> {
        if let Some(message) = self
            .firmware_job_errors
            .lock()
            .await
            .get(&cmd.job_id)
            .cloned()
        {
            return Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable(message),
            ));
        }

        Ok(self
            .firmware_job_statuses
            .lock()
            .await
            .get(&cmd.job_id)
            .cloned()
            .unwrap_or(rms::GetFirmwareJobStatusResponse {
                status: rms::ReturnCode::Failure as i32,
                job_id: cmd.job_id,
                state_description: "mock firmware job not found".to_string(),
                error_message: "mock firmware job not found".to_string(),
                ..Default::default()
            }))
    }
}

impl MockRmsClient {
    async fn switch_system_image_job_status(
        &self,
        cmd: rms::GetSwitchSystemImageJobStatusRequest,
    ) -> Result<rms::GetSwitchSystemImageJobStatusResponse, tonic::Status> {
        if let Some(message) = self
            .switch_system_image_job_errors
            .lock()
            .await
            .get(&cmd.job_id)
            .cloned()
        {
            return Err(tonic::Status::unavailable(message));
        }

        Ok(self
            .switch_system_image_job_statuses
            .lock()
            .await
            .get(&cmd.job_id)
            .cloned()
            .unwrap_or(rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Failure as i32,
                job_id: cmd.job_id,
                message: "mock switch system image job not found".to_string(),
                error_message: "mock switch system image job not found".to_string(),
                ..Default::default()
            }))
    }
}
