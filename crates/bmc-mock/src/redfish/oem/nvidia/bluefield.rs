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

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, patch, post};
use mac_address::MacAddress;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{ActionError, Callbacks, ResourceResetType, http, redfish};

#[derive(Clone)]
pub(crate) enum BluefieldState {
    Bluefield3 {
        mode: Arc<Mutex<ModeState>>,
        base_mac: MacAddress,
    },
    Bluefield4,
}

pub(crate) struct ModeState {
    nic_mode: bool,
    /// A `Mode.Set` queues the requested mode here. A real BlueField applies it
    /// only after the host power-cycles, so it lands on `nic_mode` on the next
    /// `PowerOn` event (see `BmcState::on_event`), not immediately.
    pending_nic_mode: Option<bool>,
}

impl BluefieldState {
    pub(crate) fn new_bf3(nic_mode: bool, base_mac: MacAddress) -> Self {
        Self::Bluefield3 {
            mode: Arc::new(Mutex::new(ModeState {
                nic_mode,
                pending_nic_mode: None,
            })),
            base_mac,
        }
    }

    pub(crate) fn new_bf4() -> Self {
        Self::Bluefield4
    }

    /// Whether the BlueField currently reports NIC mode.
    pub(crate) fn nic_mode(&self) -> bool {
        match self {
            Self::Bluefield3 { mode, .. } => mode.lock().unwrap().nic_mode,
            Self::Bluefield4 => false,
        }
    }

    /// Queue a `Mode.Set`; it takes effect on the next power cycle.
    fn stage_mode(&self, nic_mode: bool) {
        match self {
            Self::Bluefield3 { mode, .. } => {
                mode.lock().unwrap().pending_nic_mode = Some(nic_mode);
            }
            Self::Bluefield4 => (),
        }
    }

    /// Apply a queued `Mode.Set`, if any -- called on power-on, the point at
    /// which a real BlueField picks up a staged mode change.
    pub(crate) fn apply_pending_mode(&self) {
        match self {
            Self::Bluefield3 { mode, .. } => {
                let mut mode = mode.lock().unwrap();
                if let Some(pending) = mode.pending_nic_mode.take() {
                    mode.nic_mode = pending;
                }
            }
            Self::Bluefield4 => (),
        }
    }
}

pub(in crate::redfish) fn resource(system_id: &str) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("/redfish/v1/Systems/{system_id}/Oem/Nvidia")),
        odata_type: Cow::Borrowed("#NvidiaComputerSystem.v1_0_0.NvidiaComputerSystem"),
        // Neither BF2 nor BF-3 provide Id & Name in the resource We
        // simulate this behavior by removing these fields from final answer.
        id: Cow::Borrowed(""),
        name: Cow::Borrowed(""),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        "/redfish/v1/Systems/{system_id}/Oem/Nvidia",
        get(get_oem_nvidia::<C>),
    )
    .route(
        // TODO: This is BF-3 only.
        "/redfish/v1/Systems/{system_id}/Oem/Nvidia/Actions/HostRshim.Set",
        post(hostrshim_set),
    )
    .route(
        // BF-3 OEM mode flip. Staged here and applied on the next power
        // cycle, the same as real hardware.
        "/redfish/v1/Systems/{system_id}/Oem/Nvidia/Actions/Mode.Set",
        post(mode_set::<C>),
    )
    .route(
        // BF-4 OEM DPU OS restart. Firmware serves it without advertising it.
        "/redfish/v1/Chassis/{chassis_id}/Actions/Oem/NvidiaChassis.Reset",
        post(nvidia_chassis_reset::<C>),
    )
    .route(
        "/redfish/v1/Managers/{manager_id}/Oem/Nvidia",
        patch(patch_managers_oem_nvidia::<C>),
    )
}

async fn hostrshim_set() -> Response {
    json!({}).into_ok_response()
}

async fn get_oem_nvidia<C: Callbacks>(
    State(state): State<BmcState<C>>,
    axum::extract::Path(system_id): axum::extract::Path<String>,
) -> Response {
    let redfish::oem::State::NvidiaBluefield(state) = state.oem_state else {
        return http::not_found();
    };
    use crate::redfish::schema::nvidia_computer_system::{Mode, NvidiaComputerSystem};
    let model = match &state {
        BluefieldState::Bluefield3 { base_mac, .. } => NvidiaComputerSystem::new()
            .mode(if state.nic_mode() {
                Mode::NicMode
            } else {
                Mode::DpuMode
            })
            .base_mac(base_mac.to_string().replace(":", "")),
        BluefieldState::Bluefield4 => NvidiaComputerSystem::new(),
    };
    // This complex OEM resource carries an URI but no Id or Name on BlueField.
    redfish::oem::object("#NvidiaComputerSystem.v1_0_0.NvidiaComputerSystem", model)
        .patch(json!({"@odata.id": resource(&system_id).odata_id}))
        .into_ok_response()
}

async fn patch_managers_oem_nvidia<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(manager_id): Path<String>,
) -> Response {
    if state.manager.find(&manager_id).is_none() {
        return http::not_found();
    }

    // This is used by enable_rshim_bmc() of libredfish client.
    json!({}).into_ok_response()
}

/// BF-3 OEM `Mode.Set`: queue a DPU/NIC mode flip. Like real hardware, the
/// change is staged and only takes effect on the next power cycle (applied in
/// `BmcState::on_event` on `PowerOn`), so a read-back before then still shows
/// the old mode.
async fn mode_set<C: Callbacks>(
    State(state): State<BmcState<C>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    let redfish::oem::State::NvidiaBluefield(bluefield) = state.oem_state else {
        return http::not_found();
    };
    let Some(nic_mode) = parse_requested_mode(&body) else {
        return http::bad_request("Mode.Set requires a `Mode` of `NicMode` or `DpuMode`");
    };
    bluefield.stage_mode(nic_mode);
    // No response payload -- 204, per Redfish for an action with nothing to return.
    http::ok_no_content()
}

/// Parse a `Mode.Set` body into the requested NIC-mode flag, validating
/// strictly: `Some(true)` for `NicMode`, `Some(false)` for `DpuMode`, `None`
/// for a missing or unrecognized value. A real BF-3 rejects those, and a strict
/// mock turns a drifted client payload into a loud failure rather than a
/// silently wrong flip.
fn parse_requested_mode(body: &serde_json::Value) -> Option<bool> {
    match body.get("Mode").and_then(|mode| mode.as_str()) {
        Some(mode) if mode.eq_ignore_ascii_case("NicMode") => Some(true),
        Some(mode) if mode.eq_ignore_ascii_case("DpuMode") => Some(false),
        _ => None,
    }
}

/// BF-4 OEM `NvidiaChassis.Reset` with `ResetType` `ArmReset`: a graceful
/// restart of the Arm cores that run the DPU OS. The DPU chassis shares its id
/// with the DPU system, so the action restarts that system, simulated and logged
/// like a `ComputerSystem.Reset` with `GracefulRestart`.
async fn nvidia_chassis_reset<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(chassis_id): Path<String>,
    body: Result<axum::Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let supported = matches!(
        state.oem_state,
        redfish::oem::State::NvidiaBluefield(BluefieldState::Bluefield4)
    ) && state.system_state.find(&chassis_id).is_some();
    let Some(callbacks) = state.callbacks.as_ref().filter(|_| supported) else {
        return http::not_found();
    };
    let Ok(axum::Json(body)) = body else {
        return http::bad_request("NvidiaChassis.Reset requires a JSON body");
    };
    if body.get("ResetType").and_then(serde_json::Value::as_str) != Some("ArmReset") {
        return http::bad_request("NvidiaChassis.Reset requires a `ResetType` of `ArmReset`");
    }
    let reset_type = ResourceResetType::GracefulRestart;
    match callbacks.computer_system_reset(reset_type).await {
        Ok(()) => {
            state.record_event(redfish::log_service::LogEntryDraft::reset_requested(
                &redfish::computer_system::resource(&chassis_id).odata_id,
                reset_type,
            ));
            json!({}).into_ok_response()
        }
        Err(ActionError::BadRequest(error)) => http::bad_request(&error.to_string()),
        Err(ActionError::Internal(error)) => {
            tracing::error!(%error, chassis_id, "NvidiaChassis.Reset failed");
            http::redfish_error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "NvidiaChassis.Reset failed",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::machine_info::DpuSettings;
    use crate::test_support::{TEST_MAC_POOL, TestBmcConfig, create_test_bmc};
    use crate::{DpuMachineInfo, HardwareType, MachineInfo, MachineRouterOptions};

    /// Only a BF-4 serves `NvidiaChassis.Reset`, and only for `ArmReset`. An
    /// accepted reset restarts the system and logs it like `ComputerSystem.Reset`.
    #[tokio::test]
    async fn nvidia_chassis_reset_restarts_only_a_bluefield4() {
        struct Case {
            scenario: &'static str,
            hardware: HardwareType,
            chassis_id: &'static str,
            body: &'static str,
            status: StatusCode,
            resets: &'static [ResourceResetType],
        }
        let cases = [
            Case {
                scenario: "BF-4 ArmReset",
                hardware: HardwareType::NvidiaDgxVr,
                chassis_id: "BlueField_0",
                body: r#"{"ResetType":"ArmReset"}"#,
                status: StatusCode::OK,
                resets: &[ResourceResetType::GracefulRestart],
            },
            Case {
                scenario: "BF-4 other reset type",
                hardware: HardwareType::NvidiaDgxVr,
                chassis_id: "BlueField_0",
                body: r#"{"ResetType":"ForceRestart"}"#,
                status: StatusCode::BAD_REQUEST,
                resets: &[],
            },
            Case {
                scenario: "BF-4 chassis that is not the system",
                hardware: HardwareType::NvidiaDgxVr,
                chassis_id: "BlueField_BMC_0",
                body: r#"{"ResetType":"ArmReset"}"#,
                status: StatusCode::NOT_FOUND,
                resets: &[],
            },
            Case {
                // The BF-3's own system id, so only the BF-4 check rejects it.
                scenario: "BF-3",
                hardware: HardwareType::DellPowerEdgeR750,
                chassis_id: "Bluefield",
                body: r#"{"ResetType":"ArmReset"}"#,
                status: StatusCode::NOT_FOUND,
                resets: &[],
            },
        ];
        for case in cases {
            let dpu = DpuMachineInfo::new(
                case.hardware,
                &mut TEST_MAC_POOL.lock().unwrap(),
                DpuSettings::default(),
            );
            let (router, state) = create_test_bmc(
                &MachineInfo::Dpu(dpu),
                TestBmcConfig::default(),
                "test-dpu-id".to_string(),
                false,
                MachineRouterOptions::default(),
            );
            let response = router
                .clone()
                .oneshot(
                    Request::post(format!(
                        "/redfish/v1/Chassis/{}/Actions/Oem/NvidiaChassis.Reset",
                        case.chassis_id
                    ))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(case.body))
                    .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), case.status, "{}", case.scenario);
            assert_eq!(
                *state.callbacks.as_ref().unwrap().commands.lock().unwrap(),
                case.resets,
                "{}",
                case.scenario
            );

            let system = format!("/redfish/v1/Systems/{}", case.chassis_id);
            // Entries land in the DPU system's log, whatever id the request named.
            let dpu_system = state.system_state.primary_system_odata_id().unwrap();
            let response = router
                .oneshot(
                    Request::get(format!("{dpu_system}/LogServices/EventLog/Entries"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let entries: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            let logged: Vec<_> = entries["Members"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["Links"]["OriginOfCondition"]["@odata.id"] == system)
                .map(|entry| entry["Message"].as_str().unwrap())
                .collect();
            let expected: Vec<_> = case
                .resets
                .iter()
                .map(|&reset| {
                    redfish::log_service::LogEntryDraft::reset_requested(&system, reset).message
                })
                .collect();
            assert_eq!(logged, expected, "{}", case.scenario);
        }
    }

    #[test]
    fn mode_set_is_staged_and_applied_on_power_on() {
        // Starts in DPU mode.
        let bf = BluefieldState::new_bf3(false, MacAddress::new([0, 0, 0, 0, 0, 1]));
        assert!(!bf.nic_mode());

        // A `Mode.Set` to NIC mode is staged, not applied immediately -- a
        // read-back still reports DPU mode, like a real BF-3 before its power
        // cycle.
        bf.stage_mode(true);
        assert!(!bf.nic_mode());

        // The next power-on applies the staged mode.
        bf.apply_pending_mode();
        assert!(bf.nic_mode());

        // A power-on with nothing staged leaves the mode untouched.
        bf.apply_pending_mode();
        assert!(bf.nic_mode());
    }

    #[test]
    fn parse_requested_mode_validates_strictly() {
        assert_eq!(
            parse_requested_mode(&serde_json::json!({ "Mode": "NicMode" })),
            Some(true)
        );
        assert_eq!(
            parse_requested_mode(&serde_json::json!({ "Mode": "DpuMode" })),
            Some(false)
        );
        // Case-insensitive, matching the handler.
        assert_eq!(
            parse_requested_mode(&serde_json::json!({ "Mode": "nicmode" })),
            Some(true)
        );
        // Missing or unrecognized -> rejected (the handler returns 400).
        assert_eq!(parse_requested_mode(&serde_json::json!({})), None);
        assert_eq!(
            parse_requested_mode(&serde_json::json!({ "Mode": "bogus" })),
            None
        );
    }
}
