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

//! Pinned libredfish compatibility for the mock's decommission actions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, Request, StatusCode};
use bmc_mock::test_support::{TEST_MAC_POOL, serve_https};
use bmc_mock::{
    ActionError, BmcEvent, BmcState, Callbacks, CombinedServer, DeviceEffect, DpuMachineInfo,
    DpuOsImage, DpuOsStatus, DpuSettings, HardwareType, HostMachineInfo, MachineInfo,
    MachineRouterOptions, ResourceResetType, machine_router,
};
use libredfish::{Endpoint, RedfishClientPool, RedfishError};
use nv_redfish::schema::resource::PowerState as ResourcePowerState;
use serde_json::{Value, json};
use tower::ServiceExt;

type PublishPower = Box<dyn Fn(ResourcePowerState) + Send + Sync>;

/// Publishes only the power transitions these client tests exercise. The mock's
/// actor implementation and its cancellation/serialization contracts have their
/// own tests; this backend keeps client compatibility independent of those internals.
#[derive(Default)]
struct ClientBackend {
    publish_power: Mutex<Option<PublishPower>>,
    commands: Mutex<Vec<ResourceResetType>>,
    aux_power_cycles: AtomicUsize,
}

impl ClientBackend {
    fn publish(&self, power: ResourcePowerState) {
        self.publish_power.lock().unwrap().as_ref().unwrap()(power);
    }

    async fn wait_for_command_count(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.commands.lock().unwrap().len() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("BFB install must request a DPU restart");
    }
}

impl Callbacks for ClientBackend {
    async fn computer_system_reset(&self, reset: ResourceResetType) -> Result<(), ActionError> {
        let power = match reset {
            ResourceResetType::ForceOff => ResourcePowerState::Off,
            ResourceResetType::On | ResourceResetType::ForceRestart => ResourcePowerState::On,
            _ => panic!("unexpected client reset: {reset:?}"),
        };
        self.publish(power);
        self.commands.lock().unwrap().push(reset);
        Ok(())
    }

    async fn device_effect(&self, effect: DeviceEffect) -> Result<(), ActionError> {
        match effect {
            DeviceEffect::AuxPowerCycle => {
                self.publish(ResourcePowerState::Off);
                self.aux_power_cycles.fetch_add(1, Ordering::Relaxed);
            }
            DeviceEffect::ResetBmcToDefaults(update) => {
                update.commit().await?;
            }
        }
        Ok(())
    }

    fn state_refresh_indication(&self) {}
}

fn host_info(hardware: HardwareType) -> MachineInfo {
    let mut pool = TEST_MAC_POOL.lock().unwrap();
    let ranges = pool.allocate_range_config().unwrap();
    let dpus = (0..hardware.fixed_number_of_dpu().unwrap_or(0))
        .map(|_| DpuMachineInfo::new(hardware, &mut pool, DpuSettings::default()))
        .collect();
    MachineInfo::Host(HostMachineInfo::new(hardware, dpus, &mut pool, ranges))
}

fn test_router(info: &MachineInfo) -> (Router, BmcState<ClientBackend>, Arc<ClientBackend>) {
    let backend = Arc::new(ClientBackend::default());
    let (router, state) = machine_router(
        info,
        backend.clone(),
        "decommission-client".into(),
        false,
        MachineRouterOptions {
            firmware_upgrade_duration: Some(Duration::ZERO),
            ..Default::default()
        },
    );
    let system = Arc::downgrade(&state.system_state);
    *backend.publish_power.lock().unwrap() = Some(Box::new(move |power| {
        system.upgrade().unwrap().set_power_state(power);
    }));
    backend.publish(ResourcePowerState::On);
    (router, state, backend)
}

async fn client(router: &Router, name: &str) -> (CombinedServer, Box<dyn libredfish::Redfish>) {
    let (server, url) = serve_https(name, router.clone());
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
    (server, client)
}

async fn get_json(router: &Router, path: &str) -> Value {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

/// The hosts whose libredfish driver sends each of the BIOS reset and password request shapes:
/// NVIDIA GBx00 and Dell (empty body), Lenovo (`ResetType`), HPE (target from the Bios `Actions`).
const BIOS_ACTION_HOSTS: [HardwareType; 4] = [
    HardwareType::WiwynnGB200Nvl,
    HardwareType::DellPowerEdgeR750,
    HardwareType::LenovoGB300Nvl,
    HardwareType::HpeProliantDl380aGen11,
];

/// The `Bios` resource path of the host's system.
async fn bios_path(router: &Router) -> String {
    let systems = get_json(router, "/redfish/v1/Systems").await;
    for member in systems["Members"].as_array().unwrap() {
        let path = format!("{}/Bios", member["@odata.id"].as_str().unwrap());
        let response = router
            .clone()
            .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            return path;
        }
    }
    panic!("no system serves a Bios resource");
}

#[tokio::test]
async fn reset_bios_restores_the_profile_attributes() {
    for hardware in BIOS_ACTION_HOSTS {
        let (router, _, _) = test_router(&host_info(hardware));
        let bios_path = bios_path(&router).await;
        let defaults = get_json(&router, &bios_path).await["Attributes"].clone();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri(&bios_path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"Attributes": {"MockProbe": "changed"}}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success(), "{hardware}: {response:?}");
        let changed = get_json(&router, &bios_path).await["Attributes"].clone();
        assert_eq!(changed["MockProbe"], "changed", "{hardware}");

        let (_server, client) = client(&router, "bios-reset").await;
        client.reset_bios().await.unwrap_or_else(|e| {
            panic!("{hardware}: reset_bios failed: {e}");
        });
        let reset = get_json(&router, &bios_path).await["Attributes"].clone();
        assert_eq!(reset, defaults, "{hardware}");
    }
}

#[tokio::test]
async fn uefi_password_changes_check_the_old_password_and_clear_really_clears() {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases_async};

    for hardware in BIOS_ACTION_HOSTS {
        let (router, _, _) = test_router(&host_info(hardware));
        let (_server, client) = client(&router, "uefi-password").await;
        let client = client.as_ref();
        let change = |(old, new): (&'static str, &'static str)| async move {
            client
                .change_uefi_password(old, new)
                .await
                .map(drop)
                .map_err(drop)
        };
        // The rows run in order against one device, so each row sees the password the previous
        // ones left behind.
        let case = |scenario, input, expect| Case {
            scenario,
            input,
            expect,
        };
        check_cases_async(
            [
                case("set the first password", ("", "first"), Yields(())),
                case("an empty old password no longer matches", ("", "x"), Fails),
                case("a wrong old password is refused", ("wrong", "x"), Fails),
                case(
                    "the old password changes it",
                    ("first", "second"),
                    Yields(()),
                ),
            ],
            change,
        )
        .await;

        client
            .clear_uefi_password("second")
            .await
            .unwrap_or_else(|e| panic!("{hardware}: clear_uefi_password failed: {e}"));

        case(
            "an old password does not match an unset password",
            ("second", ""),
            Fails,
        )
        .check_async(change)
        .await;
        case("a cleared password is unset", ("", "third"), Yields(()))
            .check_async(change)
            .await;
        client.reset_bios().await.unwrap();
        case("a BIOS reset keeps the password", ("", "x"), Fails)
            .check_async(change)
            .await;
    }
}

/// NICo's machine setup disables the GB200 shell; a BIOS reset must make
/// setup necessary again through the same client used by the controller.
#[tokio::test]
async fn gb200_bios_reset_undoes_machine_setup() {
    use libredfish::BiosProfileType;

    let (router, _, _) = test_router(&host_info(HardwareType::WiwynnGB200Nvl));
    let bios_path = bios_path(&router).await;
    let (_server, client) = client(&router, "gb200-bios-reset").await;
    let uefi_shell =
        || async { get_json(&router, &bios_path).await["Attributes"]["EmbeddedUefiShell"].clone() };
    let machine_setup = || async {
        client
            .machine_setup(
                None,
                &HashMap::new(),
                BiosProfileType::default(),
                &HashMap::new(),
            )
            .await
            .unwrap();
    };
    assert_eq!(uefi_shell().await, "Enabled");
    machine_setup().await;
    assert_eq!(uefi_shell().await, "Disabled");
    client.reset_bios().await.unwrap();
    assert_eq!(uefi_shell().await, "Enabled");
    machine_setup().await;
    assert_eq!(uefi_shell().await, "Disabled");
}

#[tokio::test]
async fn pinned_libredfish_factory_reset_reaches_host_dpu_and_shelf() {
    let MachineInfo::Host(host) = host_info(HardwareType::WiwynnGB200Nvl) else {
        unreachable!()
    };
    for (name, info) in [
        ("host", host_info(HardwareType::DellPowerEdgeR750)),
        ("dpu", MachineInfo::Dpu(host.dpus[0].clone())),
        ("shelf", host_info(HardwareType::LiteOnPowerShelf)),
    ] {
        let (router, state, _) = test_router(&info);
        let accounts = || serde_json::to_value(state.persisted()).unwrap()["accounts"].clone();
        let baseline = accounts();
        let (_server, client) = client(&router, name).await;
        for generation in 1..=2 {
            state
                .account_service_state
                .change_factory_default_password(format!("rotated-{generation}"));
            assert_ne!(
                accounts(),
                baseline,
                "{name}: password must change before reset"
            );
            client.bmc_reset_to_defaults().await.unwrap();
            assert_eq!(state.factory_reset_generation(), generation, "{name}");
            assert_eq!(accounts(), baseline, "{name}");
        }
    }
}

/// Core forces a GB200 off before its NVIDIA AuxPowerReset. The same action
/// must leave both an already-off host and an on host off.
#[tokio::test]
async fn libredfish_ac_powercycle_leaves_the_host_off() {
    use libredfish::{PowerState, SystemPowerControl};

    let (router, _, backend) = test_router(&host_info(HardwareType::WiwynnGB200Nvl));
    let (_server, client) = client(&router, "gb200-aux-power").await;
    assert!(client.ac_powercycle_supported_by_power());
    assert_eq!(client.get_power_state().await.unwrap(), PowerState::On);
    client.power(SystemPowerControl::ForceOff).await.unwrap();
    client
        .power(SystemPowerControl::ACPowercycle)
        .await
        .unwrap();
    assert_eq!(client.get_power_state().await.unwrap(), PowerState::Off);
    client.power(SystemPowerControl::On).await.unwrap();
    assert_eq!(client.get_power_state().await.unwrap(), PowerState::On);
    client
        .power(SystemPowerControl::ACPowercycle)
        .await
        .unwrap();
    assert_eq!(client.get_power_state().await.unwrap(), PowerState::Off);
    assert_eq!(backend.aux_power_cycles.load(Ordering::Relaxed), 2);
}

/// NICo's firmware poller must see unknown task IDs as missing, including an
/// ID shaped like a task from an earlier mock instance.
#[tokio::test]
async fn unknown_task_ids_are_not_found() {
    let (router, _, _) = test_router(&host_info(HardwareType::WiwynnGB200Nvl));
    let (_server, client) = client(&router, "unknown-task").await;
    for task_id in ["1", "00000000-1", "JID_510613515077"] {
        assert!(
            matches!(client.get_task(task_id).await,
                Err(RedfishError::HTTPErrorCode { status_code, .. }) if status_code == 404),
            "{task_id}"
        );
    }
}

/// The pinned client observes BFB completion after restart and OSRunning only
/// after boot. A subsequent managed install restores the managed image.
#[tokio::test]
async fn bfb_install_lands_on_restart_and_boot_progress_follows() {
    use libredfish::model::task::TaskState;
    use libredfish::model::update_service::TransferProtocolType;

    let dpu = {
        let mut pool = TEST_MAC_POOL.lock().unwrap();
        MachineInfo::Dpu(DpuMachineInfo::new(
            HardwareType::DellPowerEdgeR750,
            &mut pool,
            DpuSettings::default(),
        ))
    };
    let (router, state, backend) = test_router(&dpu);
    let (_server, client) = client(&router, "bf3-bfb-install").await;
    let boot_state = || async {
        client
            .get_system()
            .await
            .unwrap()
            .boot_progress
            .and_then(|progress| progress.last_state)
            .map(|state| state.to_string())
    };
    state
        .update_service_state
        .record_dpu_os_image(Some(DpuOsImage::Managed));
    state.on_event(&BmcEvent::BootCompleted);
    assert_eq!(boot_state().await.as_deref(), Some("OSRunning"));
    for (round, (image_uri, image)) in [
        (
            "carbide-pxe.forge//public/blobs/internal/aarch64/preingestion.bfb",
            DpuOsImage::PreIngestion,
        ),
        (
            "carbide-pxe.forge//public/blobs/internal/aarch64/forge.bfb",
            DpuOsImage::Managed,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let generation = state
            .update_service_state
            .dpu_os_status()
            .unwrap()
            .generation;
        let task = client
            .update_firmware_simple_update(
                image_uri,
                vec!["redfish/v1/UpdateService/FirmwareInventory/DPU_OS".into()],
                TransferProtocolType::HTTP,
            )
            .await
            .unwrap();
        assert!(matches!(task.task_state, Some(TaskState::Running)));
        backend.wait_for_command_count(round + 1).await;
        assert_eq!(
            backend.commands.lock().unwrap()[round],
            ResourceResetType::ForceRestart
        );
        assert!(matches!(
            client.get_task(&task.id).await.unwrap().task_state,
            Some(TaskState::Running)
        ));
        state.on_event(&BmcEvent::PowerOn);
        assert!(matches!(
            client.get_task(&task.id).await.unwrap().task_state,
            Some(TaskState::Completed)
        ));
        assert_eq!(
            state.update_service_state.dpu_os_status(),
            Some(DpuOsStatus {
                image: Some(image),
                generation: generation + 1,
            })
        );
        assert_eq!(boot_state().await.as_deref(), Some("OSBootStarted"));
        state.on_event(&BmcEvent::BootCompleted);
        assert_eq!(boot_state().await.as_deref(), Some("OSRunning"));
        let completed = serde_json::to_value(client.get_task(&task.id).await.unwrap()).unwrap();
        assert_eq!(
            completed["Messages"][0]["Message"],
            "BFB installed; the DPU restarted into the new image."
        );
    }
}

/// Both NVSwitch profiles advertise the route the switch controller invokes.
#[tokio::test]
async fn switch_system_reset_reaches_the_backend() {
    for hardware in [
        HardwareType::NvidiaSwitchNd5200Ld,
        HardwareType::NvidiaSwitchN5700Ld,
    ] {
        let (router, _state, backend) = test_router(&host_info(hardware));
        let system = get_json(&router, "/redfish/v1/Systems/System_0").await;
        let target = system["Actions"]["#ComputerSystem.Reset"]["target"]
            .as_str()
            .unwrap();
        assert_eq!(
            target,
            "/redfish/v1/Systems/System_0/Actions/ComputerSystem.Reset"
        );
        let response = router
            .clone()
            .oneshot(
                Request::post(target)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"ResetType":"ForceRestart"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success(), "{hardware:?}");
        assert_eq!(
            *backend.commands.lock().unwrap(),
            [ResourceResetType::ForceRestart]
        );
        assert_eq!(
            get_json(&router, "/redfish/v1/Systems/System_0").await["PowerState"],
            "On"
        );
    }
}
