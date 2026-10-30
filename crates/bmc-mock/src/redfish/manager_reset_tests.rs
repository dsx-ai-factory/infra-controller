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
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tower::ServiceExt;

use crate::redfish::account_service::PasswordUpdater;
use crate::test_support::{TestBmcConfig, TestCallbacks, create_test_bmc, host_info};
use crate::{
    ActionError, BmcState, Callbacks, DeviceEffect, HardwareType, MachineInfo,
    MachineRouterOptions, ResourceResetType,
};

fn bmc(auth: bool) -> (Router, BmcState<TestCallbacks>) {
    create_test_bmc(
        &host_info(HardwareType::DellPowerEdgeR750),
        TestBmcConfig::default(),
        "reset-defaults".into(),
        auth,
        MachineRouterOptions::default(),
    )
}

fn target<C: Callbacks>(state: &BmcState<C>) -> String {
    format!(
        "{}/Actions/Manager.ResetToDefaults",
        state.manager.primary_odata_id().unwrap()
    )
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: &str,
    token: Option<&str>,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("x-auth-token", token);
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}

async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn factory_reset_restores_manager_construction_values() {
    for (hardware, ipmi_port) in [
        (HardwareType::DellPowerEdgeR750, None),
        (HardwareType::DellPowerEdgeR750, Some(9623)),
        (HardwareType::HpeProliantDl380aGen11, None),
        (HardwareType::SupermicroGb300Nvl, None),
    ] {
        let (router, state) = create_test_bmc(
            &host_info(hardware),
            TestBmcConfig::default(),
            "reset-manager-config".into(),
            false,
            MachineRouterOptions::default(),
        );
        state.manager.set_ipmi_endpoint(ipmi_port);
        let manager_path = state.manager.primary_odata_id().unwrap();
        let manager_before = body(request(&router, "GET", &manager_path, "", None).await).await;
        let mut patches = vec![
            (
                manager_path.clone(),
                json!({"Oem":{"Hpe":{"VirtualNICEnabled":true}}}),
            ),
            (
                format!("{manager_path}/NetworkProtocol"),
                json!({
                    "IPMI":{"ProtocolEnabled":ipmi_port.is_none()},
                    "NTP":{"ProtocolEnabled":false,"NTPServers":["configured.example"]},
                }),
            ),
        ];
        if let Some(path) = manager_before["HostInterfaces"]["@odata.id"].as_str() {
            let interfaces = body(request(&router, "GET", path, "", None).await).await;
            let path = interfaces["Members"][0]["@odata.id"].as_str().unwrap();
            let before = body(request(&router, "GET", path, "", None).await).await;
            patches.push((
                path.to_owned(),
                json!({"InterfaceEnabled":!before["InterfaceEnabled"].as_bool().unwrap()}),
            ));
        }
        if matches!(hardware, HardwareType::DellPowerEdgeR750) {
            patches.push((
                format!("{manager_path}/Attributes"),
                json!({"Attributes":{"Lockdown.1.SystemLockdown":"Enabled"}}),
            ));
        }
        if matches!(hardware, HardwareType::SupermicroGb300Nvl) {
            patches.push((
                format!("{manager_path}/Oem/Supermicro/KCSInterface"),
                json!({"Privilege":"NoAccess"}),
            ));
            patches.push((
                format!("{manager_path}/Oem/Supermicro/SysLockdown"),
                json!({"SysLockdownEnabled":true}),
            ));
        }
        let mut defaults = Vec::new();
        for (path, patch) in &patches {
            let mut before = body(request(&router, "GET", path, "", None).await).await;
            // DateTime is sampled for each GET of the manager resource.
            before.as_object_mut().unwrap().remove("DateTime");
            let response = request(&router, "PATCH", path, &patch.to_string(), None).await;
            assert!(response.status().is_success(), "PATCH {path}");
            let mut changed = body(request(&router, "GET", path, "", None).await).await;
            changed.as_object_mut().unwrap().remove("DateTime");
            assert_ne!(changed, before, "PATCH must change {path}");
            defaults.push((path, before));
        }
        let response = request(
            &router,
            "POST",
            &target(&state),
            r#"{"ResetType":"ResetAll"}"#,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        for (path, before) in defaults {
            let mut restored = body(request(&router, "GET", path, "", None).await).await;
            restored.as_object_mut().unwrap().remove("DateTime");
            assert_eq!(restored, before, "factory value of {path}");
        }
    }
}

#[tokio::test]
async fn reset_request_contract_and_rejection_leave_state_unchanged() {
    // These are the two shapes sent by the workspace libredfish client: standard.rs
    // uses ResetType; nvidia_dpu.rs and liteon_powershelf.rs use ResetToDefaultsType.
    let cases = [
        (
            "standard client",
            r#"{"ResetType":"ResetAll"}"#,
            StatusCode::OK,
        ),
        (
            "DPU and shelf clients",
            r#"{"ResetToDefaultsType":"ResetAll"}"#,
            StatusCode::OK,
        ),
        ("absent property", "{}", StatusCode::BAD_REQUEST),
        (
            "both properties",
            r#"{"ResetType":"ResetAll","ResetToDefaultsType":"ResetAll"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "both properties including null",
            r#"{"ResetType":null,"ResetToDefaultsType":"ResetAll"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "unsupported operation",
            r#"{"ResetType":"PreserveNetwork"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "wrong JSON type",
            r#"{"ResetType":17}"#,
            StatusCode::BAD_REQUEST,
        ),
        ("malformed JSON", "{", StatusCode::BAD_REQUEST),
        (
            "null property",
            r#"{"ResetType":null}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "unknown property",
            r#"{"ResetType":"ResetAll","Extra":true}"#,
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (scenario, payload, expected) in cases {
        let (router, state) = bmc(false);
        state
            .account_service_state
            .change_factory_default_password("rotated");
        let before = state.persisted();
        let session = state.session_service_state.create("root");
        let response = request(&router, "POST", &target(&state), payload, None).await;
        assert_eq!(response.status(), expected, "{scenario}");
        assert_eq!(
            state.factory_reset_generation(),
            u64::from(expected.is_success()),
            "{scenario}"
        );
        if !expected.is_success() {
            assert!(state.persisted() == before, "{scenario}");
            assert!(
                state.session_service_state.is_token_valid(&session.token),
                "{scenario}"
            );
        }
    }
    let (router, state) = bmc(false);
    let before = state.persisted();
    let response = request(
        &router,
        "POST",
        "/redfish/v1/Managers/absent/Actions/Manager.ResetToDefaults",
        r#"{"ResetType":"ResetAll"}"#,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(state.persisted() == before);

    let manager = state.manager.primary_odata_id().unwrap();
    let advertised = body(request(&router, "GET", &manager, "", None).await).await;
    assert!(!advertised["Actions"]["#Manager.Reset"].is_null());
    assert_eq!(
        advertised["Actions"]["#Manager.ResetToDefaults"]["target"],
        target(&state)
    );
}

struct NoEffects;
impl Callbacks for NoEffects {
    async fn computer_system_reset(&self, _: ResourceResetType) -> Result<(), ActionError> {
        Ok(())
    }
    fn state_refresh_indication(&self) {}
}

#[tokio::test]
async fn unsupported_backend_cannot_reset_accounts_or_sessions() {
    let (router, state) = crate::machine_router(
        &host_info(HardwareType::DellPowerEdgeR750),
        Arc::new(NoEffects),
        "unsupported-reset".into(),
        false,
        MachineRouterOptions::default(),
    );
    state
        .account_service_state
        .change_factory_default_password("rotated");
    let session = state.session_service_state.create("root");
    let before = state.persisted();
    let manager_path = state.manager.primary_odata_id().unwrap();
    let manager = body(request(&router, "GET", &manager_path, "", None).await).await;
    assert!(!manager["Actions"]["#Manager.Reset"].is_null());
    assert!(manager["Actions"]["#Manager.ResetToDefaults"].is_null());
    let response = request(
        &router,
        "POST",
        &target(&state),
        r#"{"ResetType":"ResetAll"}"#,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state.persisted() == before);
    assert!(state.session_service_state.is_token_valid(&session.token));
}

#[tokio::test]
async fn reset_restores_construction_baseline_and_invalidates_old_authentication() {
    let (router, state) = bmc(true);
    let baseline = state.account_service_state.accounts();
    let mut snapshot = serde_json::to_value(state.persisted()).unwrap();
    snapshot["accounts"][0]["id"] = json!("restored-admin-id");
    snapshot["accounts"][0]["username"] = json!("restored-admin");
    snapshot["accounts"][0]["password"] = json!("persisted-password");
    snapshot["accounts"][0]["role_id"] = json!("ReadOnly");
    let mut added = snapshot["accounts"][0].clone();
    added["id"] = json!("extra");
    added["username"] = json!("extra-user");
    snapshot["accounts"].as_array_mut().unwrap().push(added);
    state
        .restore_persisted(&serde_json::from_value(snapshot).unwrap())
        .unwrap();
    let account_id = serde_json::to_value(state.persisted()).unwrap()["accounts"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        state
            .account_service_state
            .update_password(&account_id, "rotated-again")
            .await
            .unwrap()
    );
    let login = json!({"UserName":"restored-admin", "Password":"rotated-again"}).to_string();
    let session = request(
        &router,
        "POST",
        "/redfish/v1/SessionService/Sessions",
        &login,
        None,
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let token = session.headers()["x-auth-token"]
        .to_str()
        .unwrap()
        .to_owned();
    let response = request(
        &router,
        "POST",
        &target(&state),
        r#"{"ResetType":"ResetAll"}"#,
        Some(&token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(state.account_service_state.accounts(), baseline);
    assert_eq!(state.factory_reset_generation(), 1);
    assert_eq!(
        request(
            &router,
            "GET",
            "/redfish/v1/AccountService/Accounts",
            "",
            Some(&token)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/redfish/v1/SessionService/Sessions",
            &login,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let factory_login =
        json!({"UserName":crate::DUMMY_FACTORY_USERNAME,"Password":crate::DUMMY_FACTORY_PASSWORD})
            .to_string();
    assert_eq!(
        request(
            &router,
            "POST",
            "/redfish/v1/SessionService/Sessions",
            &factory_login,
            None
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    let snapshot = state.persisted();
    let (_, restored) = bmc(false);
    restored.restore_persisted(&snapshot).unwrap();
    assert_eq!(restored.factory_reset_generation(), 1);
    assert_eq!(restored.account_service_state.accounts(), baseline);
}

#[tokio::test]
async fn reset_preserves_system_and_completed_firmware_until_normal_bmc_reset() {
    let mut info = host_info(HardwareType::GenericAmi);
    let MachineInfo::Host(host) = &mut info else {
        unreachable!()
    };
    host.initial_host_firmware = Some(crate::HostFirmwareVersions {
        bmc: Some("before".into()),
        uefi: None,
    });
    host.desired_host_firmware = Some(crate::HostFirmwareVersions {
        bmc: Some("staged".into()),
        uefi: None,
    });
    let (router, state) = create_test_bmc(
        &info,
        TestBmcConfig::default(),
        "firmware-reset".into(),
        false,
        MachineRouterOptions::default(),
    );
    let system = state.system_state.primary_system_odata_id().unwrap();
    let before = body(request(&router, "GET", &system, "", None).await).await;
    let upload = request(
        &router,
        "POST",
        "/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate",
        r#"{"Targets":["/redfish/v1/UpdateService/FirmwareInventory/HostBMC_0"]}"#,
        None,
    )
    .await;
    assert_eq!(upload.status(), StatusCode::ACCEPTED);
    state.update_service_state.complete_all_tasks_for_test();
    assert_eq!(
        request(
            &router,
            "POST",
            &target(&state),
            r#"{"ResetType":"ResetAll"}"#,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let after = body(request(&router, "GET", &system, "", None).await).await;
    assert_eq!(after, before);
    assert_eq!(
        state
            .update_service_state
            .find_firmware_inventory("HostBMC_0")
            .unwrap()["Version"],
        "before"
    );
    // Prove the fixture really contained staged, activatable firmware.
    state.reset();
    assert_eq!(
        state
            .update_service_state
            .find_firmware_inventory("HostBMC_0")
            .unwrap()["Version"],
        "staged"
    );
}

#[tokio::test]
async fn reset_restarts_bmc_availability_and_event_stream_generation() {
    let (router, state) = create_test_bmc(
        &host_info(HardwareType::DellPowerEdgeR750),
        TestBmcConfig::default(),
        "outage-reset".into(),
        false,
        MachineRouterOptions {
            bmc_reset_duration: Some(Duration::from_secs(60)),
            ..Default::default()
        },
    );
    let events = state.event_service.as_ref().unwrap();
    state.on_event(&crate::BmcEvent::PowerOn);
    let _subscriber = events.subscribe(None).unwrap();
    let before = events.stats();
    assert!(before.retained_frames > 0);
    assert_eq!(before.subscribers, 1);
    assert_eq!(
        request(
            &router,
            "POST",
            &target(&state),
            r#"{"ResetType":"ResetAll"}"#,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let after = events.stats();
    assert_ne!(after.generation, before.generation);
    assert_eq!(after.retained_frames, 0);
    assert_eq!(after.subscribers, 0);
    assert_eq!(
        request(&router, "GET", "/redfish/v1", "", None)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

struct DelayedPasswords {
    calls: Mutex<Vec<(String, String)>>,
    entered: Notify,
    release: Semaphore,
}
impl Default for DelayedPasswords {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            entered: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}
impl PasswordUpdater for DelayedPasswords {
    fn update_password<'a>(
        &'a self,
        _: &'a str,
        current: &'a str,
        new: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let first = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((current.into(), new.into()));
                calls.len() == 1
            };
            if first {
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn reset_serializes_with_an_in_flight_external_password_change() {
    let (router, state) = bmc(false);
    let updater = Arc::new(DelayedPasswords::default());
    let backend: Arc<dyn PasswordUpdater> = updater.clone();
    state.account_service_state.set_password_updater(&backend);
    let id = serde_json::to_value(state.persisted()).unwrap()["accounts"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accounts = state.account_service_state.clone();
    let rotation =
        tokio::spawn(async move { accounts.update_password(&id, "rotated").await.unwrap() });
    updater.entered.notified().await;
    // Poll the actual reset admission while the external update is blocked.
    // This proves serialization even if the request task would otherwise run
    // only after the password update completes.
    {
        let update = state.factory_reset_update();
        tokio::pin!(update);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(update.as_mut(), &mut context).is_pending());
    }
    let path = target(&state);
    let reset = tokio::spawn(async move {
        request(&router, "POST", &path, r#"{"ResetType":"ResetAll"}"#, None).await
    });
    updater.release.add_permits(1);
    assert!(rotation.await.unwrap());
    assert_eq!(reset.await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        *updater.calls.lock().unwrap(),
        vec![
            (crate::DUMMY_FACTORY_PASSWORD.into(), "rotated".into()),
            ("rotated".into(), crate::DUMMY_FACTORY_PASSWORD.into())
        ]
    );
    assert_eq!(state.factory_reset_generation(), 1);
}

#[tokio::test]
async fn old_password_login_cannot_insert_a_session_across_reset_publication() {
    let (router, state) = bmc(true);
    state
        .account_service_state
        .change_factory_default_password("rotated");
    let updater = Arc::new(DelayedPasswords::default());
    let backend: Arc<dyn PasswordUpdater> = updater.clone();
    state.account_service_state.set_password_updater(&backend);
    let reset_token = state.session_service_state.create("root").token;
    let reset_router = router.clone();
    let path = target(&state);
    let reset = tokio::spawn(async move {
        request(
            &reset_router,
            "POST",
            &path,
            r#"{"ResetType":"ResetAll"}"#,
            Some(&reset_token),
        )
        .await
    });
    updater.entered.notified().await;
    assert_eq!(state.factory_reset_generation(), 0);
    assert!(state.account_service_state.is_authorized("root", "rotated"));
    let login = request(
        &router,
        "POST",
        "/redfish/v1/SessionService/Sessions",
        r#"{"UserName":"root","Password":"rotated"}"#,
        None,
    );
    tokio::pin!(login);
    // Poll through the real route while reset holds the credential lock. A
    // login that validates and inserts outside that lock completes here with
    // the old password and can escape the reset's session invalidation.
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    if let std::task::Poll::Ready(response) =
        std::future::Future::poll(login.as_mut(), &mut context)
    {
        panic!(
            "old-password login completed during reset: {}",
            response.status()
        );
    }
    updater.release.add_permits(1);
    assert_eq!(reset.await.unwrap().status(), StatusCode::OK);
    assert_eq!(login.await.status(), StatusCode::UNAUTHORIZED);
    assert!(state.session_service_state.list().is_empty());
    assert_eq!(state.factory_reset_generation(), 1);
}

struct FailingPasswords;
impl PasswordUpdater for FailingPasswords {
    fn update_password<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("injected external credential synchronization failure".into()) })
    }
}

#[tokio::test]
async fn failed_external_credential_restore_leaves_accounts_and_sessions_unchanged() {
    let (router, state) = bmc(false);
    state
        .account_service_state
        .change_factory_default_password("rotated");
    let session = state.session_service_state.create("root");
    let manager_path = state.manager.primary_odata_id().unwrap();
    let patch = json!({"Oem":{"Hpe":{"VirtualNICEnabled":true}}});
    assert!(
        request(&router, "PATCH", &manager_path, &patch.to_string(), None)
            .await
            .status()
            .is_success()
    );
    let before = state.persisted();
    let updater: Arc<dyn PasswordUpdater> = Arc::new(FailingPasswords);
    state.account_service_state.set_password_updater(&updater);
    let response = request(
        &router,
        "POST",
        &target(&state),
        r#"{"ResetType":"ResetAll"}"#,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(state.persisted() == before);
    assert!(state.session_service_state.is_token_valid(&session.token));
    let manager = body(request(&router, "GET", &manager_path, "", None).await).await;
    assert_eq!(manager["Oem"]["Hpe"]["VirtualNICEnabled"], true);
}

struct QueuedReset {
    sender: mpsc::UnboundedSender<(DeviceEffect, oneshot::Sender<Result<(), ActionError>>)>,
}
impl Callbacks for QueuedReset {
    fn supports_bmc_factory_reset(&self) -> bool {
        true
    }

    async fn computer_system_reset(&self, _: ResourceResetType) -> Result<(), ActionError> {
        Ok(())
    }
    async fn device_effect(&self, effect: DeviceEffect) -> Result<(), ActionError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send((effect, reply))
            .map_err(|error| ActionError::Internal(eyre::eyre!(error.to_string())))?;
        response
            .await
            .map_err(|error| ActionError::Internal(error.into()))?
    }
    fn state_refresh_indication(&self) {}
}

#[tokio::test]
async fn admitted_reset_survives_http_cancellation_and_releases_update_lock() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let (router, state) = crate::machine_router(
        &host_info(HardwareType::DellPowerEdgeR750),
        Arc::new(QueuedReset { sender }),
        "cancel-reset".into(),
        false,
        MachineRouterOptions::default(),
    );
    state
        .account_service_state
        .change_factory_default_password("rotated");
    let path = target(&state);
    let pending = tokio::spawn(async move {
        request(&router, "POST", &path, r#"{"ResetType":"ResetAll"}"#, None).await
    });
    let (effect, reply) = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.factory_reset_generation(), 0);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let DeviceEffect::ResetBmcToDefaults(update) = effect else {
        panic!("wrong effect")
    };
    update.commit().await.unwrap();
    assert!(reply.send(Ok(())).is_err(), "HTTP receiver was cancelled");
    assert_eq!(state.factory_reset_generation(), 1);
    assert!(
        state
            .account_service_state
            .is_authorized(crate::DUMMY_FACTORY_USERNAME, crate::DUMMY_FACTORY_PASSWORD)
    );
    // A consumed update must not strand the serialization guard.
    let next = tokio::time::timeout(Duration::from_secs(5), state.factory_reset_update())
        .await
        .unwrap();
    next.commit().await.unwrap();
    assert_eq!(state.factory_reset_generation(), 2);
}
