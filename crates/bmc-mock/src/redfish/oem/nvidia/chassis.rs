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

//! NVIDIA OEM chassis actions.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::post;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{ActionError, Callbacks, DeviceEffect, http};

/// The only `ResetType` the AC power cycle action accepts.
const AUX_POWER_CYCLE: &str = "AuxPowerCycle";

pub(crate) fn aux_power_reset_target(chassis_id: &str) -> String {
    format!("/redfish/v1/Chassis/{chassis_id}/Actions/Oem/NvidiaChassis.AuxPowerReset")
}

/// The `Actions.Oem` entry a chassis advertises when it supports `AuxPowerReset`.
pub(crate) fn aux_power_reset_action(chassis_id: &str) -> serde_json::Value {
    json!({"Actions": {"Oem": {"#NvidiaChassis.AuxPowerReset": {
        "target": aux_power_reset_target(chassis_id),
        "ResetType@Redfish.AllowableValues": [AUX_POWER_CYCLE],
    }}}})
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        &aux_power_reset_target("{chassis_id}"),
        post(post_aux_power_reset::<C>),
    )
}

/// `NvidiaChassis.AuxPowerReset`: an AC power cycle of the whole chassis. The
/// effect on the host and its DPUs belongs to the backend, see
/// [`DeviceEffect::AuxPowerCycle`].
async fn post_aux_power_reset<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(chassis_id): Path<String>,
    body: Result<axum::Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let supported = state
        .chassis_state
        .find(&chassis_id)
        .is_some_and(|chassis| chassis.config.aux_power_reset);
    let Some(callbacks) = state.callbacks.as_ref().filter(|_| supported) else {
        return http::not_found();
    };
    let axum::Json(body) = match body {
        Ok(body) => body,
        Err(_) => return http::bad_request("AuxPowerReset requires a JSON body"),
    };
    if body.get("ResetType").and_then(serde_json::Value::as_str) != Some(AUX_POWER_CYCLE) {
        return http::bad_request("AuxPowerReset requires a `ResetType` of `AuxPowerCycle`");
    }
    match callbacks.device_effect(DeviceEffect::AuxPowerCycle).await {
        Ok(()) => json!({}).into_ok_response(),
        Err(ActionError::BadRequest(error)) => http::bad_request(&error.to_string()),
        Err(ActionError::Internal(error)) => {
            tracing::error!(%error, chassis_id, "AuxPowerReset failed");
            http::redfish_error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "AuxPowerReset failed",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::{TestBmcConfig, TestCallbacks, create_test_bmc, host_info};
    use crate::{BmcState, HardwareType, MachineRouterOptions};

    fn gb200_bmc() -> (Router, BmcState<TestCallbacks>) {
        create_test_bmc(
            &host_info(HardwareType::WiwynnGB200Nvl),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        )
    }

    async fn send(router: &Router, method: Method, path: &str, body: &str) -> (StatusCode, String) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// Only a chassis configured for the action serves it, and only with the body
    /// libredfish sends. Only the chassis that serves it advertises it.
    #[tokio::test]
    async fn action_is_served_only_by_the_chassis_that_advertises_it() {
        struct Case {
            scenario: &'static str,
            chassis_id: &'static str,
            body: &'static str,
            status: StatusCode,
            effects: usize,
        }
        let cases = [
            Case {
                scenario: "libredfish body on the BMC chassis",
                chassis_id: "BMC_0",
                body: r#"{"ResetType":"AuxPowerCycle"}"#,
                status: StatusCode::OK,
                effects: 1,
            },
            Case {
                scenario: "other reset type",
                chassis_id: "BMC_0",
                body: r#"{"ResetType":"ForceOff"}"#,
                status: StatusCode::BAD_REQUEST,
                effects: 0,
            },
            Case {
                scenario: "missing reset type",
                chassis_id: "BMC_0",
                body: "{}",
                status: StatusCode::BAD_REQUEST,
                effects: 0,
            },
            Case {
                scenario: "chassis that does not advertise the action",
                chassis_id: "Chassis_0",
                body: r#"{"ResetType":"AuxPowerCycle"}"#,
                status: StatusCode::NOT_FOUND,
                effects: 0,
            },
        ];
        for case in cases {
            let (router, state) = gb200_bmc();
            let (status, body) = send(
                &router,
                Method::POST,
                &aux_power_reset_target(case.chassis_id),
                case.body,
            )
            .await;
            assert_eq!(status, case.status, "{}: {body}", case.scenario);
            let callbacks = state.callbacks.as_ref().unwrap();
            assert_eq!(
                callbacks.effects.lock().unwrap().len(),
                case.effects,
                "{}",
                case.scenario
            );
        }

        let (router, _) = gb200_bmc();
        for (chassis_id, advertised) in [("BMC_0", true), ("Chassis_0", false)] {
            let (status, body) = send(
                &router,
                Method::GET,
                &format!("/redfish/v1/Chassis/{chassis_id}"),
                "",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let chassis: serde_json::Value = serde_json::from_str(&body).unwrap();
            let action = &chassis["Actions"]["Oem"]["#NvidiaChassis.AuxPowerReset"];
            assert_eq!(
                action["target"].as_str(),
                advertised
                    .then(|| aux_power_reset_target(chassis_id))
                    .as_deref(),
                "{chassis_id}"
            );
        }
    }

    #[tokio::test]
    async fn action_is_not_served_by_a_host_without_the_oem_chassis() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::DellPowerEdgeR750),
            TestBmcConfig::default(),
            String::new(),
            false,
            MachineRouterOptions::default(),
        );
        let (status, _) = send(
            &router,
            Method::POST,
            &aux_power_reset_target("BMC_0"),
            r#"{"ResetType":"AuxPowerCycle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_json_returns_a_redfish_error_without_an_effect() {
        for content_type in ["application/json", "text/plain"] {
            let (router, state) = gb200_bmc();
            let response = router
                .oneshot(
                    Request::post(aux_power_reset_target("BMC_0"))
                        .header(CONTENT_TYPE, content_type)
                        .body(Body::from("{"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let json: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert!(json["error"].is_object(), "{content_type}");
            assert!(
                state
                    .callbacks
                    .as_ref()
                    .unwrap()
                    .effects
                    .lock()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    /// A backend that does not model the device must fail the action, not answer
    /// success without an effect.
    #[tokio::test]
    async fn backend_without_device_effects_rejects_the_action() {
        struct NoEffects;
        impl Callbacks for NoEffects {
            async fn computer_system_reset(
                &self,
                _: crate::ResourceResetType,
            ) -> Result<(), ActionError> {
                Ok(())
            }
            fn state_refresh_indication(&self) {}
        }
        let (router, _) = crate::machine_router(
            &host_info(HardwareType::WiwynnGB200Nvl),
            std::sync::Arc::new(NoEffects),
            "test-host-id".into(),
            false,
            MachineRouterOptions::default(),
        );
        let (status, document) = send(&router, Method::GET, "/redfish/v1/Chassis/BMC_0", "").await;
        assert_eq!(status, StatusCode::OK);
        let document: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert!(!document["Actions"]["Oem"]["#NvidiaChassis.AuxPowerReset"].is_null());
        let (status, body) = send(
            &router,
            Method::POST,
            &aux_power_reset_target("BMC_0"),
            r#"{"ResetType":"AuxPowerCycle"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}
