/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! A BMC serving fixed Redfish documents, for driver tests shaped after
//! recorded hardware rather than a whole simulated machine.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bmc_mock::test_support::TestBmc;
use bmc_mock::test_support::axum_http_client::{AxumRouterHttpClient, RecordedRequest};
use bmc_platform::{ManagerIdentity, OpCx, PlatformIdentity, SystemIdentity};
use nv_redfish::ServiceRoot;
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use serde_json::{Value, json};
use url::Url;

type Responses = Arc<HashMap<(Method, String), (StatusCode, Option<Value>)>>;

pub(crate) struct FixtureBmc {
    bmc: Arc<TestBmc>,
    root: ServiceRoot<TestBmc>,
    client: AxumRouterHttpClient,
    identity: PlatformIdentity,
}

/// Builds a BMC that answers `GET` with its documents and any other method
/// with the configured response, defaulting to `204 No Content`.
pub(crate) struct Fixture {
    responses: HashMap<(Method, String), (StatusCode, Option<Value>)>,
    identity: PlatformIdentity,
}

impl Fixture {
    /// A service root from `vendor`/`product` with one system and one
    /// manager, both selected by exploration; later documents replace these.
    pub(crate) fn new(vendor: &str, product: &str, system: &str, manager: &str) -> Self {
        let system_id = format!("/redfish/v1/Systems/{system}");
        let manager_id = format!("/redfish/v1/Managers/{manager}");
        let fixture = Self {
            responses: HashMap::new(),
            identity: PlatformIdentity {
                system: Some(SystemIdentity {
                    id: system.to_string(),
                    ..SystemIdentity::default()
                }),
                manager: Some(ManagerIdentity {
                    id: manager.to_string(),
                    model: None,
                    firmware: None,
                }),
                ..PlatformIdentity::default()
            },
        };
        fixture
            .document(
                "/redfish/v1",
                json!({
                    "@odata.id": "/redfish/v1",
                    "Id": "RootService",
                    "Name": "Root Service",
                    "Vendor": vendor,
                    "Product": product,
                    "Systems": {"@odata.id": "/redfish/v1/Systems"},
                    "Managers": {"@odata.id": "/redfish/v1/Managers"},
                    "Chassis": {"@odata.id": "/redfish/v1/Chassis"},
                    "UpdateService": {"@odata.id": "/redfish/v1/UpdateService"},
                    "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                }),
            )
            .document(
                "/redfish/v1/Systems",
                json!({
                    "@odata.id": "/redfish/v1/Systems",
                    "@odata.type": "#ComputerSystemCollection.ComputerSystemCollection",
                    "Name": "Systems",
                    "Members": [{"@odata.id": system_id}],
                }),
            )
            .document(
                "/redfish/v1/Managers",
                json!({
                    "@odata.id": "/redfish/v1/Managers",
                    "@odata.type": "#ManagerCollection.ManagerCollection",
                    "Name": "Managers",
                    "Members": [{"@odata.id": manager_id}],
                }),
            )
            .document(
                &system_id,
                json!({"@odata.id": system_id, "Id": system, "Name": system}),
            )
            .document(
                &manager_id,
                json!({"@odata.id": manager_id, "Id": manager, "Name": manager}),
            )
    }

    pub(crate) fn document(mut self, path: &str, body: Value) -> Self {
        self.responses.insert(
            (Method::GET, path.to_string()),
            (StatusCode::OK, Some(body)),
        );
        self
    }

    pub(crate) fn respond(
        mut self,
        method: Method,
        path: &str,
        status: StatusCode,
        body: Option<Value>,
    ) -> Self {
        self.responses
            .insert((method, path.to_string()), (status, body));
        self
    }

    pub(crate) async fn build(self) -> FixtureBmc {
        let router = Router::new()
            .fallback(respond)
            .with_state(Arc::new(self.responses));
        let client = AxumRouterHttpClient::new(router);
        let bmc = Arc::new(HttpBmc::new(
            client.clone(),
            Url::parse("https://bmc.test").expect("valid URL"),
            BmcCredentials::new("root".to_string(), "password".to_string()),
            CacheSettings::with_capacity(0),
        ));
        let root = ServiceRoot::new(bmc.clone())
            .await
            .expect("fixture serves a service root");
        FixtureBmc {
            bmc,
            root,
            client,
            identity: self.identity,
        }
    }
}

impl FixtureBmc {
    /// An operation context, with the request log cleared.
    pub(crate) async fn cx(&self) -> OpCx<'_, TestBmc> {
        let cx = OpCx::new(self.bmc.as_ref(), &self.root, &self.identity);
        if self.identity.system.is_some() {
            cx.system()
                .await
                .expect("fixture resolves the selected system");
        }
        if self.identity.manager.is_some() {
            cx.manager()
                .await
                .expect("fixture resolves the selected manager");
        }
        self.client.take_requests();
        cx
    }

    /// An operation context that has fetched nothing, as the runtime builds it.
    pub(crate) fn lazy_cx(&self) -> OpCx<'_, TestBmc> {
        self.client.take_requests();
        OpCx::new(self.bmc.as_ref(), &self.root, &self.identity)
    }

    /// Every request other than `GET` issued since the last call.
    pub(crate) fn writes(&self) -> Vec<RecordedRequest> {
        self.client
            .take_requests()
            .into_iter()
            .filter(|request| request.method != Method::GET)
            .collect()
    }
}

/// The JSON body of `request`.
pub(crate) fn body(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).expect("request body is JSON")
}

/// The request path, without scheme and host.
pub(crate) fn path(request: &RecordedRequest) -> &str {
    request
        .uri
        .strip_prefix("https://bmc.test")
        .unwrap_or(&request.uri)
}

async fn respond(State(responses): State<Responses>, method: Method, uri: Uri) -> Response {
    let key = (method.clone(), uri.path().to_string());
    match responses.get(&key) {
        Some((status, Some(body))) => (*status, axum::Json(body.clone())).into_response(),
        Some((status, None)) => status.into_response(),
        None if method == Method::GET => StatusCode::NOT_FOUND.into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
