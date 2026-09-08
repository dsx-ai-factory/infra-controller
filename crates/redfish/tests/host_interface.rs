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

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use axum_server::tls_rustls::RustlsConfig;
use carbide_redfish::libredfish::host_interface::{
    HostInterfaceStaticIpv4Repair, ensure_gb200_host_interface_static_ipv4,
};
use libredfish::{Endpoint, Redfish, RedfishClientPool, RedfishError};
use serde_json::{Value, json};

const INTERFACE_PATH: &str = "/redfish/v1/Managers/BMC/EthernetInterfaces/hostusb0";
const RESOURCE_ETAG: &str = "W/\"revision-7\"";

#[derive(Debug)]
struct PatchRequest {
    path: String,
    if_match: Option<String>,
    body: Value,
}

#[derive(Clone)]
struct AppState {
    etag: Option<&'static str>,
    initial_static_addresses: Option<Value>,
    get_hits: Arc<AtomicUsize>,
    patch_requests: Arc<Mutex<Vec<PatchRequest>>>,
    patch_status: StatusCode,
    apply_patch: bool,
    patched: Arc<AtomicBool>,
}

impl AppState {
    fn new(etag: Option<&'static str>, patch_status: StatusCode) -> Self {
        Self {
            etag,
            initial_static_addresses: Some(json!([])),
            get_hits: Arc::new(AtomicUsize::new(0)),
            patch_requests: Arc::new(Mutex::new(Vec::new())),
            patch_status,
            apply_patch: true,
            patched: Arc::new(AtomicBool::new(false)),
        }
    }

    fn with_static_addresses(mut self, addresses: Value) -> Self {
        self.initial_static_addresses = Some(addresses);
        self
    }

    fn without_static_addresses(mut self) -> Self {
        self.initial_static_addresses = None;
        self
    }

    fn without_applying_patch(mut self) -> Self {
        self.apply_patch = false;
        self
    }
}

async fn get_interface(State(state): State<AppState>) -> Response {
    state.get_hits.fetch_add(1, Ordering::SeqCst);
    let static_addresses = if state.patched.load(Ordering::SeqCst) {
        Some(json!([{
            "Address": "10.0.1.1",
            "SubnetMask": "255.255.255.0",
            "Gateway": "0.0.0.0"
        }]))
    } else {
        state.initial_static_addresses.clone()
    };
    let mut resource = json!({
        "@odata.id": INTERFACE_PATH,
        "@odata.type": "#EthernetInterface.v1_9_0.EthernetInterface",
        "Id": "hostusb0"
    });
    if let Some(static_addresses) = static_addresses {
        resource["IPv4StaticAddresses"] = static_addresses;
    }

    let mut response = Json(resource).into_response();
    if let Some(etag) = state.etag {
        response
            .headers_mut()
            .insert(ETAG, HeaderValue::from_static(etag));
    }
    response
}

async fn patch_interface(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = PatchRequest {
        path: uri.path().to_string(),
        if_match: headers
            .get(IF_MATCH)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: serde_json::from_slice(&body).unwrap(),
    };
    state.patch_requests.lock().unwrap().push(request);
    if state.patch_status.is_success() && state.apply_patch {
        state.patched.store(true, Ordering::SeqCst);
    }

    let mut response = Json(json!({})).into_response();
    *response.status_mut() = state.patch_status;
    response
}

fn spawn_mock_bmc(state: AppState) -> SocketAddr {
    let app = Router::new()
        .route(INTERFACE_PATH, get(get_interface).patch(patch_interface))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    let tls = bmc_mock::tls::server_config(None::<&str>).unwrap();
    let config = RustlsConfig::from_config(Arc::new(tls));
    tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, config)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    addr
}

fn redfish_client(addr: SocketAddr) -> Box<dyn Redfish> {
    let pool = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()
        .unwrap();
    let mut client = pool
        .create_standard_client(Endpoint {
            host: addr.ip().to_string(),
            port: Some(addr.port()),
            user: Some("root".to_string()),
            password: Some("placeholder".to_string()),
        })
        .unwrap();
    client.set_manager_id("BMC").unwrap();
    client
}

fn install_crypto_provider() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();
}

#[tokio::test]
async fn repair_uses_resource_etag_and_accepts_success_responses() {
    install_crypto_provider();
    for patch_status in [StatusCode::OK, StatusCode::NO_CONTENT] {
        let state = AppState::new(Some(RESOURCE_ETAG), patch_status);
        let addr = spawn_mock_bmc(state.clone());
        let client = redfish_client(addr);

        let repair = ensure_gb200_host_interface_static_ipv4(client.as_ref())
            .await
            .unwrap();

        assert_eq!(repair, HostInterfaceStaticIpv4Repair::Patched);
        assert_eq!(state.get_hits.load(Ordering::SeqCst), 2);
        let requests = state.patch_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, INTERFACE_PATH);
        assert_eq!(requests[0].if_match.as_deref(), Some(RESOURCE_ETAG));
        assert_eq!(
            requests[0].body,
            json!({
                "IPv4StaticAddresses": [{
                    "Address": "10.0.1.1",
                    "SubnetMask": "255.255.255.0",
                    "Gateway": "0.0.0.0"
                }]
            })
        );
    }
}

#[tokio::test]
async fn repair_does_not_patch_an_already_configured_address() {
    install_crypto_provider();
    let state = AppState::new(Some(RESOURCE_ETAG), StatusCode::OK).with_static_addresses(json!([{
        "Address": "10.0.1.1",
        "SubnetMask": "255.255.255.0",
        "Gateway": "0.0.0.0"
    }]));
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let repair = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap();

    assert_eq!(repair, HostInterfaceStaticIpv4Repair::AlreadyConfigured);
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 1);
    assert!(state.patch_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repair_does_not_patch_without_an_etag() {
    install_crypto_provider();
    let state = AppState::new(None, StatusCode::OK);
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let error = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("missing a usable ETag"));
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 1);
    assert!(state.patch_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repair_does_not_patch_when_static_addresses_are_not_reported() {
    install_crypto_provider();
    let state = AppState::new(Some(RESOURCE_ETAG), StatusCode::OK).without_static_addresses();
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let error = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("did not report IPv4StaticAddresses")
    );
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 1);
    assert!(state.patch_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repair_preserves_a_conflicting_static_address() {
    install_crypto_provider();
    let state = AppState::new(Some(RESOURCE_ETAG), StatusCode::OK).with_static_addresses(json!([{
        "Address": "192.0.2.1",
        "SubnetMask": "255.255.255.0",
        "Gateway": "0.0.0.0"
    }]));
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let repair = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap();

    assert_eq!(repair, HostInterfaceStaticIpv4Repair::Conflicting);
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 1);
    assert!(state.patch_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repair_propagates_a_stale_etag_response() {
    install_crypto_provider();
    let state = AppState::new(Some(RESOURCE_ETAG), StatusCode::PRECONDITION_FAILED);
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let error = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        RedfishError::HTTPErrorCode { status_code, .. }
            if status_code == StatusCode::PRECONDITION_FAILED
    ));
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 1);
    assert_eq!(state.patch_requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn repair_verifies_that_an_accepted_patch_changed_the_address() {
    install_crypto_provider();
    let state = AppState::new(Some(RESOURCE_ETAG), StatusCode::OK).without_applying_patch();
    let addr = spawn_mock_bmc(state.clone());
    let client = redfish_client(addr);

    let error = ensure_gb200_host_interface_static_ipv4(client.as_ref())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("address is still missing"));
    assert_eq!(state.get_hits.load(Ordering::SeqCst), 2);
    assert_eq!(state.patch_requests.lock().unwrap().len(), 1);
}
