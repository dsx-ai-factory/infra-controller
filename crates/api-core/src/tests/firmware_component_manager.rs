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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use component_manager::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayManager,
    ComputeTrayResult,
};
use component_manager::error::ComponentManagerError;
use component_manager::mock::MockComputeTrayManager;
use component_manager::types::FirmwareUpdateOptions;
use model::component_manager::{ComputeTrayComponent, PowerAction};
use model::test_support::HardwareInfoTemplate;
use rpc::forge as rpc;
use tonic::Request;

use crate::tests::common::api_fixtures::host::GB200_COMPUTE_TRAY_1_INFO_JSON;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host_with_hardware_info_template,
    create_test_env_with_overrides,
};

#[derive(Debug, Default)]
struct RecordingComputeTrayManager {
    inner: MockComputeTrayManager,
    firmware_update_options: Mutex<Vec<FirmwareUpdateOptions>>,
}

impl RecordingComputeTrayManager {
    fn clear_firmware_update_options(&self) {
        self.firmware_update_options.lock().unwrap().clear();
    }

    fn firmware_update_options(&self) -> Vec<FirmwareUpdateOptions> {
        self.firmware_update_options.lock().unwrap().clone()
    }
}

#[async_trait]
impl ComputeTrayManager for RecordingComputeTrayManager {
    fn name(&self) -> &str {
        "recording-compute-tray-manager"
    }

    fn backend(&self) -> Backend {
        self.inner.backend()
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        self.inner.power_control(endpoints, action).await
    }

    async fn update_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        target_version: &str,
        components: &[ComputeTrayComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        self.firmware_update_options
            .lock()
            .unwrap()
            .push(options.clone());
        self.inner
            .update_firmware(endpoints, target_version, components, options)
            .await
    }

    async fn get_firmware_status(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        self.inner.get_firmware_status(endpoints).await
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        self.inner.list_firmware_bundles().await
    }
}

#[crate::sqlx_test]
async fn compute_tray_direct_dispatch_forwards_force_update(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let compute_tray_manager = Arc::new(RecordingComputeTrayManager::default());
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            compute_tray_manager: Some(compute_tray_manager.clone()),
            ..Default::default()
        },
    )
    .await;
    let managed_host = create_managed_host_with_hardware_info_template(
        &env,
        HardwareInfoTemplate::Custom(GB200_COMPUTE_TRAY_1_INFO_JSON),
    )
    .await;
    compute_tray_manager.clear_firmware_update_options();

    for force_update in [false, true] {
        let response = crate::handlers::component_manager::update_component_firmware(
            &env.api,
            Request::new(rpc::UpdateComponentFirmwareRequest {
                target_version: r#"{"Id":"test-firmware"}"#.to_string(),
                access_token: None,
                force_update,
                bypass_state_controller: true,
                target: Some(
                    rpc::update_component_firmware_request::Target::ComputeTrays(
                        rpc::UpdateComputeTrayFirmwareTarget {
                            machine_ids: Some(::rpc::common::MachineIdList {
                                machine_ids: vec![*managed_host.id],
                            }),
                            bmc_macs: None,
                            components: vec![],
                        },
                    ),
                ),
            }),
        )
        .await?
        .into_inner();

        assert_eq!(response.results.len(), 1);
        assert_eq!(
            response.results[0].status,
            rpc::ComponentManagerStatusCode::Success as i32,
        );
    }

    let options = compute_tray_manager.firmware_update_options();
    assert_eq!(
        options
            .iter()
            .map(|options| options.force_update)
            .collect::<Vec<_>>(),
        vec![false, true],
    );
    assert!(
        options.iter().all(|options| options.access_token.is_none()),
        "the force-update fix must not change access-token handling",
    );

    Ok(())
}
