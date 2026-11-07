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

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use carbide_axum_utils::injection::{InjectionStore, injection_router, management_router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::UfmAuthToken;
use crate::state::{BindRequest, Fabric, FabricError, PartitionKey, QosRequest, UnbindRequest};

#[derive(Clone)]
pub(crate) struct HttpState {
    fabric: Fabric,
    auth_header: HeaderValue,
}

impl HttpState {
    pub(crate) fn new(fabric: Fabric, auth_token: &UfmAuthToken) -> eyre::Result<Self> {
        let auth_header = HeaderValue::from_str(&format!("Basic {}", auth_token.expose_secret()))?;
        Ok(Self {
            fabric,
            auth_header,
        })
    }

    pub(crate) fn router(self, injection: Arc<InjectionStore>) -> Router {
        let routes = Router::new()
            .route("/app/ufm_version", get(version))
            .route("/app/smconf", get(sm_config))
            .route("/resources/ports", get(ports))
            .route("/resources/pkeys", get(partitions).post(bind))
            .route("/resources/pkeys/{pkey}", get(partition))
            .route("/actions/remove_guids_from_pkey", post(unbind))
            .route("/resources/pkeys/qos_conf", put(update_qos))
            .with_state(self.clone());
        let routes = injection_router(routes, Arc::clone(&injection))
            .route_layer(middleware::from_fn_with_state(self, authorize));
        Router::new()
            .merge(management_router(injection))
            .nest("/ufmRestV3", routes)
    }
}

#[derive(Debug, Default, Deserialize)]
struct PartitionQuery {
    #[serde(default)]
    guids_data: bool,
    #[serde(default)]
    qos_conf: bool,
}

#[derive(Debug, Serialize)]
struct VersionResponse<'a> {
    ufm_release_version: &'a str,
}

async fn version(State(state): State<HttpState>) -> Response {
    json_response(
        StatusCode::OK,
        &VersionResponse {
            ufm_release_version: state.fabric.version(),
        },
    )
}

async fn sm_config(State(state): State<HttpState>) -> Response {
    json_response(StatusCode::OK, state.fabric.sm_config())
}

async fn ports(State(state): State<HttpState>) -> Response {
    let fabric = state.fabric.read();
    json_response(StatusCode::OK, &fabric.ports())
}

async fn partitions(
    State(state): State<HttpState>,
    Query(query): Query<PartitionQuery>,
) -> Response {
    let fabric = state.fabric.read();
    json_response(
        StatusCode::OK,
        &fabric.partitions(query.guids_data, query.qos_conf),
    )
}

async fn partition(
    State(state): State<HttpState>,
    Path(pkey): Path<PartitionKey>,
    Query(query): Query<PartitionQuery>,
) -> Response {
    let fabric = state.fabric.read();
    match fabric.partition(pkey, query.guids_data, query.qos_conf) {
        Ok(Some(partition)) => json_response(StatusCode::OK, &partition),
        Ok(None) => json_response(StatusCode::OK, &json!({})),
        Err(error) => error_response(error),
    }
}

async fn bind(State(state): State<HttpState>, Json(request): Json<BindRequest>) -> Response {
    match state.fabric.bind(request) {
        Ok(()) => json_response(StatusCode::OK, &json!({})),
        Err(error) => error_response(error),
    }
}

async fn unbind(State(state): State<HttpState>, Json(request): Json<UnbindRequest>) -> Response {
    state.fabric.unbind(request);
    json_response(StatusCode::OK, &json!({}))
}

async fn update_qos(State(state): State<HttpState>, Json(request): Json<QosRequest>) -> Response {
    match state.fabric.update_qos(request) {
        Ok(()) => json_response(StatusCode::OK, &json!({})),
        Err(error) => error_response(error),
    }
}

async fn authorize(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    let mut response = if request.headers().get(AUTHORIZATION) != Some(&state.auth_header) {
        (StatusCode::UNAUTHORIZED, "invalid UFM authorization token").into_response()
    } else {
        next.run(request).await
    };

    response
        .headers_mut()
        .insert("x-ufm-mock", HeaderValue::from_static("carbide-ufm-mock"));
    response
}

fn error_response(error: FabricError) -> Response {
    let status = match error {
        FabricError::PartitionNotFound(_) | FabricError::PortNotFound(_) => StatusCode::NOT_FOUND,
        FabricError::InvalidInventoryIdentity => StatusCode::BAD_REQUEST,
    };
    json_response(status, &json!({ "error": error.to_string() }))
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .expect("static response headers must be valid"),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize UFM response: {error}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::config::FabricConfig;
    use crate::inventory::{
        InfinibandPortState, InventoryMachine, InventoryPort, InventorySnapshot,
    };

    fn test_router() -> Router {
        let fabric = Fabric::new(FabricConfig::default());
        fabric
            .reconcile(
                InventorySnapshot {
                    inventory_id: "inventory-a".into(),
                    epoch_id: "epoch-a".into(),
                    generation: 1.into(),
                    machines: vec![InventoryMachine {
                        mat_id: "mat-a".into(),
                        machine_id: Some("machine-a".into()),
                        infiniband_ports: Some(vec![InventoryPort {
                            guid: "0x1".parse().unwrap(),
                            state: InfinibandPortState::Active,
                        }]),
                    }],
                },
                crate::inventory::InventoryLease::FIRST_REMOTE,
            )
            .unwrap();
        let auth_token = UfmAuthToken::new("test-token".to_string()).unwrap();
        HttpState::new(fabric, &auth_token)
            .unwrap()
            .router(Arc::new(InjectionStore::new()))
    }

    fn authorized_request(method: &str, uri: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, "Basic test-token")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn requires_the_basic_token_used_by_rest_ib_fabric() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/ufmRestV3/app/ufm_version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn shared_injection_rules_apply_to_ufm_routes_and_can_be_cleared() {
        let router = test_router();
        let rule = json!({
            "id": "version-unavailable",
            "selector": {
                "Path": {
                    "method": "GET",
                    "glob": "/ufmRestV3/app/ufm_version"
                }
            },
            "action": { "Status": 503 }
        });

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/Injection/rules")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(rule.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .clone()
            .oneshot(authorized_request(
                "GET",
                "/ufmRestV3/app/ufm_version",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get("x-ufm-mock"),
            Some(&HeaderValue::from_static("carbide-ufm-mock"))
        );

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/Injection/rules")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("[]"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .oneshot(authorized_request(
                "GET",
                "/ufmRestV3/app/ufm_version",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn exposes_ports_and_mutable_partition_membership_in_ufm_v3_shape() {
        let router = test_router();
        let response = router
            .clone()
            .oneshot(authorized_request(
                "GET",
                "/ufmRestV3/resources/ports?sys_type=Computer",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body[0]["guid"], "0000000000000001");
        assert_eq!(body[0]["systemID"], "0000000000000001");
        assert_eq!(body[0]["logical_state"], "Active");

        let response = router
            .clone()
            .oneshot(authorized_request(
                "GET",
                "/ufmRestV3/resources/pkeys?guids_data=true",
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["0x7fff"]["partition"], "management");
        assert_eq!(body["0x7fff"]["guids"], json!([]));

        let response = router
            .clone()
            .oneshot(authorized_request(
                "POST",
                "/ufmRestV3/resources/pkeys",
                Body::from(
                    serde_json::to_vec(&json!({
                        "pkey": "1",
                        "ip_over_ib": false,
                        "membership": "full",
                        "index0": true,
                        "guids": ["0000000000000001"]
                    }))
                    .unwrap(),
                ),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = router
            .oneshot(authorized_request(
                "GET",
                "/ufmRestV3/resources/pkeys/1?guids_data=true&qos_conf=true",
                Body::empty(),
            ))
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["partition"], "api_pkey_0x1");
        assert_eq!(body["guids"][0]["membership"], "full");
        assert_eq!(body["qos_conf"]["rate_limit"], 2.5);
    }
}
