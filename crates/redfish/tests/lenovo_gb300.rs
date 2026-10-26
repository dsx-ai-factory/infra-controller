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

//! Legacy libredfish compatibility with the Lenovo GB300 mock BMC.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use bmc_mock::test_support::{TEST_MAC_POOL, TestCallbacks, serve_https};
use bmc_mock::{
    BmcState, DpuMachineInfo, DpuSettings, HardwareType, HostFirmwareVersions, HostMachineInfo,
    MachineInfo, MachineRouterOptions, machine_router,
};
use nv_redfish::schema::resource::PowerState;
use tower::ServiceExt;

fn lenovo_gb300_host() -> HostMachineInfo {
    let hardware = HardwareType::LenovoGB300Nvl;
    let mut pool = TEST_MAC_POOL.lock().unwrap();
    let hardware_macs = pool.allocate_range_config().unwrap();
    let dpus = (0..hardware.fixed_number_of_dpu().unwrap())
        .map(|_| DpuMachineInfo::new(hardware, &mut pool, DpuSettings::default()))
        .collect();
    HostMachineInfo::new(hardware, dpus, &mut pool, hardware_macs)
}

fn lenovo_gb300_router(host: HostMachineInfo) -> (Router, BmcState<TestCallbacks>) {
    let (router, state) = machine_router(
        &MachineInfo::Host(host),
        Arc::new(TestCallbacks::default()),
        "test-host-id".into(),
        false,
        MachineRouterOptions {
            firmware_upgrade_duration: Some(Duration::ZERO),
            ..Default::default()
        },
    );
    state.system_state.set_power_state(PowerState::On);
    (router, state)
}

async fn get_json(router: &Router, path: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

/// Core's Lenovo client must accept both the order and option PATCH responses.
#[tokio::test]
async fn lenovo_gb300_client_restores_dpu_boot_order() {
    use libredfish::{BootInterfaceRef, Endpoint, RedfishClientPool};

    let host = lenovo_gb300_host();
    let dpu_mac = host.dpus[0].host_mac_address;
    let (router, _) = lenovo_gb300_router(host);
    let (_server, url) = serve_https("gb300-boot-order", router.clone());
    let client = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
        .create_client(Endpoint {
            host: url.host_str().unwrap().to_owned(),
            port: url.port(),
            user: None,
            password: None,
        })
        .await
        .unwrap();
    assert!(
        !client
            .is_boot_order_setup(BootInterfaceRef::Mac(dpu_mac))
            .await
            .unwrap()
    );
    client
        .set_boot_order_dpu_first(BootInterfaceRef::Mac(dpu_mac))
        .await
        .unwrap();
    assert!(
        client
            .is_boot_order_setup(BootInterfaceRef::Mac(dpu_mac))
            .await
            .unwrap()
    );
    let target = get_json(&router, "/redfish/v1/Systems/System_0/BootOptions/0004").await;
    assert_eq!(target["BootOptionEnabled"], true);
}

#[tokio::test]
async fn lenovo_gb300_requires_full_lockdown_to_restore_bios_policy() {
    use libredfish::{EnabledDisabled, Endpoint, RedfishClientPool};
    let (router, _) = lenovo_gb300_router(HostMachineInfo {
        initial_host_firmware: Some(HostFirmwareVersions {
            bmc: Some("bmc-old".into()),
            uefi: None,
        }),
        desired_host_firmware: Some(HostFirmwareVersions {
            bmc: Some("bmc-new".into()),
            uefi: None,
        }),
        ..lenovo_gb300_host()
    });
    let (_server, url) = serve_https("gb300-lockdown", router);
    let client = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .create_client(Endpoint {
            host: url.host_str().unwrap().to_owned(),
            port: url.port(),
            user: None,
            password: None,
        })
        .await
        .unwrap();
    client.lockdown_bmc(EnabledDisabled::Enabled).await.unwrap();
    let status = client.lockdown_status().await.unwrap();
    assert!(
        !status.is_fully_enabled(),
        "BMC-only command must leave BIOS USB unlocked: {status:?}"
    );
    client.lockdown(EnabledDisabled::Enabled).await.unwrap();
    let status = client.lockdown_status().await.unwrap();
    assert!(
        status.is_fully_enabled(),
        "full policy must restore BIOS and BMC: {status:?}"
    );
}

/// Exercise the same legacy client used by the Core controller, including
/// its LenovoGB300 multipart rejection. This verifies the mock's raw-push
/// compatibility, not support for this endpoint on physical Lenovo hardware.
#[tokio::test]
async fn lenovo_gb300_legacy_client_raw_push_activates_bmc() {
    use libredfish::model::service_root::RedfishVendor;
    use libredfish::model::update_service::ComponentType;
    use libredfish::{Endpoint, RedfishClientPool, RedfishError};

    let (router, state) = lenovo_gb300_router(HostMachineInfo {
        initial_host_firmware: Some(HostFirmwareVersions {
            bmc: Some("bmc-old".into()),
            uefi: Some("uefi-old".into()),
        }),
        desired_host_firmware: Some(HostFirmwareVersions {
            bmc: Some("bmc-new".into()),
            uefi: Some("uefi-new".into()),
        }),
        ..lenovo_gb300_host()
    });
    let bios_before = get_json(&router, "/redfish/v1/Systems/System_0").await["BiosVersion"]
        .as_str()
        .expect("Lenovo System_0 must report its BIOS version")
        .to_owned();
    let system_router = router.clone();
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = received.clone();
    let router = router.layer(axum::middleware::from_fn(
        move |request: Request<axum::body::Body>, next: axum::middleware::Next| {
            let captured = captured.clone();
            async move {
                if request.method() == Method::POST
                    && request.uri().path() == "/redfish/v1/UpdateService"
                {
                    let (parts, body) = request.into_parts();
                    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
                    *captured.lock().unwrap() = bytes.to_vec();
                    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
                        .await
                } else {
                    next.run(request).await
                }
            }
        },
    ));
    let (_server, url) = serve_https("gb300-http-push", router);
    let client = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .create_client(Endpoint {
            host: url.host_str().unwrap().to_owned(),
            port: url.port(),
            user: None,
            password: None,
        })
        .await
        .unwrap();
    assert_eq!(
        client.std_redfish().vendor,
        Some(RedfishVendor::LenovoGB300)
    );
    assert_eq!(
        client.get_update_service().await.unwrap().http_push_uri,
        "/redfish/v1/UpdateService",
    );
    let artifact = tempfile::NamedTempFile::new().unwrap();
    let payload = b"mock firmware payload, not a physical firmware image";
    std::fs::write(artifact.path(), payload).unwrap();
    assert!(matches!(
        client
            .update_firmware_multipart(
                artifact.path(),
                true,
                Duration::from_secs(5),
                ComponentType::BMC,
            )
            .await,
        Err(RedfishError::NotSupported(_))
    ));
    assert!(received.lock().unwrap().is_empty());
    let task = client
        .update_firmware(tokio::fs::File::open(artifact.path()).await.unwrap())
        .await
        .unwrap();
    assert_eq!(&*received.lock().unwrap(), payload);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let task = serde_json::to_value(client.get_task(&task.id).await.unwrap()).unwrap();
            if task["TaskState"] == "Completed" {
                break;
            }
            assert_eq!(task["TaskState"], "Running");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("firmware task must complete");
    let inventory = || {
        state
            .update_service_state
            .find_firmware_inventory("BMC")
            .unwrap()
    };
    assert_eq!(inventory()["Version"], "bmc-old");
    client.bmc_reset(None).await.unwrap();
    assert_eq!(inventory()["Version"], "bmc-new");
    assert_eq!(
        state.update_service_state.find_firmware_inventory("UEFI"),
        None,
        "Lenovo exposes BIOS on System_0, not a synthetic firmware inventory entry"
    );
    assert_eq!(
        get_json(&system_router, "/redfish/v1/Systems/System_0").await["BiosVersion"],
        bios_before,
        "BMC activation must leave the host BIOS unchanged"
    );
}
