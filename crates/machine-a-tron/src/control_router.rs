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
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use bmc_mock::injection::{InjectionStore, Rule, RuleId};
use bmc_mock::{HardwareType, RackPlacement};
use carbide_uuid::rack::RackId;
use chrono::{SecondsFormat, Utc};
use rms_sim::{RmsInventory, SimNode, SimNodeKind};
use tower::Service;
use ufm_mock::{
    EpochId, Generation, InventoryId, InventoryMachine as UfmInventoryMachine, InventoryPort,
    InventoryProvider, InventorySnapshot, MachineId, MatId,
};

use crate::device_handle::DeviceHandle;
use crate::device_simulator::SimulatorLifecycle;
use crate::simulator_registry::SimulatorRegistry;
use crate::status::{DeviceKind, DeviceStatus, DeviceStatusConfig, DevicesStatusResponse};

pub fn append(router: Option<Router>, control_state: ControlState) -> Router {
    Router::new()
        .route("/", get(get_machines_ui))
        .route("/machines/status", get(get_machines_status))
        .route("/racks/status", get(get_racks_status))
        .route("/racks/{rack_id}/status", get(get_rack_status))
        .route(
            "/machines/{id}/bmc/injection/rules",
            get(list_bmc_injection_rules).post(upsert_bmc_injection_rule),
        )
        .route(
            "/machines/{id}/bmc/injection/rules/{rule_id}",
            axum::routing::delete(delete_bmc_injection_rule),
        )
        .route("/{*all}", any(process))
        .with_state(ControlRouter {
            inner: router,
            control_state,
        })
}

#[derive(Clone)]
pub struct ControlState {
    simulators: SimulatorRegistry,
    status_config: DeviceStatusConfig,
    inventory_version: Arc<Mutex<InventoryVersion>>,
    rms_snapshot: Arc<Mutex<RmsSnapshot>>,
}

#[derive(Debug)]
struct InventoryVersion {
    inventory_id: InventoryId,
    epoch_id: EpochId,
    generation: Generation,
    snapshot: Vec<InventoryMachine>,
}

/// The fleet as last reported to the RMS simulator, and the fingerprint it
/// was built from.
///
/// Same idea as `InventoryVersion`: rebuild only when the result would
/// differ. An RMS client enriches a fleet one request per device, and
/// building every `SimNode` for every request made that pass quadratic in
/// the fleet size. Everything in a `SimNode` is fixed when the device is
/// built except the two DHCP-assigned addresses, so those are the whole
/// fingerprint. The UFM fingerprint above is not reused because it covers
/// machines only and none of their addresses.
#[derive(Debug, Default)]
struct RmsSnapshot {
    /// `(bmc_ip, host_ip)` per device, in registry order.
    addresses: Vec<(Option<Ipv4Addr>, Option<Ipv4Addr>)>,
    nodes: Arc<[SimNode]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InventoryMachine {
    mat_id: MatId,
    machine_id: Option<MachineId>,
    hardware_type: Option<HardwareType>,
    infiniband_ports: Vec<InventoryPort>,
}

impl ControlState {
    pub fn new(
        simulators: SimulatorRegistry,
        status_config: DeviceStatusConfig,
        inventory_id: InventoryId,
    ) -> Self {
        let devices = Self::collect_statuses(&simulators, &status_config);
        Self {
            simulators,
            status_config,
            inventory_version: Arc::new(Mutex::new(InventoryVersion {
                inventory_id,
                epoch_id: Utc::now()
                    .to_rfc3339_opts(SecondsFormat::Nanos, true)
                    .into(),
                generation: Generation::INITIAL,
                snapshot: Self::inventory_snapshot(&devices),
            })),
            rms_snapshot: Arc::default(),
        }
    }

    fn devices_status(&self) -> DevicesStatusResponse {
        let mut version = self
            .inventory_version
            .lock()
            .expect("inventory version lock poisoned");
        let devices = Self::collect_statuses(&self.simulators, &self.status_config);
        let snapshot = Self::inventory_snapshot(&devices);
        if snapshot != version.snapshot {
            version.snapshot = snapshot;
            version.generation = version
                .generation
                .checked_next()
                .expect("inventory generation cannot overflow during one process epoch");
        }

        DevicesStatusResponse {
            inventory_id: version.inventory_id.clone(),
            epoch_id: version.epoch_id.clone(),
            generation: version.generation,
            devices,
        }
    }

    fn collect_statuses(
        simulators: &SimulatorRegistry,
        status_config: &DeviceStatusConfig,
    ) -> Vec<DeviceStatus> {
        simulators
            .devices()
            .iter()
            .map(|simulator| simulator.status(status_config))
            .collect()
    }

    fn inventory_snapshot(devices: &[DeviceStatus]) -> Vec<InventoryMachine> {
        let mut machines = devices
            .iter()
            .filter(|device| device.device_kind == DeviceKind::Machine)
            .map(|device| {
                let mut infiniband_ports = device
                    .infiniband_ports
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|port| InventoryPort {
                        guid: port.guid,
                        state: port.state,
                    })
                    .collect::<Vec<_>>();
                infiniband_ports.sort_by_key(|port| port.guid);
                InventoryMachine {
                    mat_id: device.mat_id.as_str().into(),
                    machine_id: device.machine_id.as_deref().map(MachineId::from),
                    hardware_type: device.hardware_type,
                    infiniband_ports,
                }
            })
            .collect::<Vec<_>>();
        machines.sort_by(|left, right| left.mat_id.cmp(&right.mat_id));
        machines
    }

    fn device(&self, id: &str) -> Option<Arc<InjectionStore>> {
        self.simulators.find_injection_store(id)
    }
}

/// Report simulated hardware to the hosted RMS simulator.
///
/// Placement comes from the same `RackPlacement` the device's Redfish chassis
/// is built from, so RMS and Redfish cannot disagree about where a node sits.
/// Compute trays report their chassis slot and compute tray index; switch
/// trays report the rack unit they occupy and their index among the rack's
/// switch trays. Power shelves carry a placement too, but RMS does not
/// report one for them and NICo does not ask.
///
/// The snapshot handed out is rebuilt only when a device's BMC or host
/// address has changed since the last request (see `RmsSnapshot`); nothing
/// else in it can change, so a cached snapshot is never stale.
impl RmsInventory for ControlState {
    fn nodes(&self) -> Arc<[SimNode]> {
        let mut cached = self
            .rms_snapshot
            .lock()
            .expect("RMS snapshot lock poisoned");
        let devices = self.simulators.devices();
        let unchanged = cached.addresses.len() == devices.len()
            && devices
                .iter()
                .zip(&cached.addresses)
                .all(|(simulator, (bmc_ip, host_ip))| {
                    let handle = simulator.handle();
                    handle.bmc_ip() == *bmc_ip && handle.host_ip() == *host_ip
                });
        if !unchanged {
            // Each address is read once and used for both the fingerprint
            // and the node, so the two cannot disagree about it.
            let (addresses, nodes): (Vec<_>, Vec<_>) = devices
                .iter()
                .map(|simulator| {
                    let handle = simulator.handle();
                    let (bmc_ip, host_ip) = (handle.bmc_ip(), handle.host_ip());
                    ((bmc_ip, host_ip), Self::sim_node(handle, bmc_ip, host_ip))
                })
                .unzip();
            cached.addresses = addresses;
            cached.nodes = nodes.into();
        }
        Arc::clone(&cached.nodes)
    }
}

impl ControlState {
    fn sim_node(
        handle: &DeviceHandle,
        bmc_ip: Option<Ipv4Addr>,
        host_ip: Option<Ipv4Addr>,
    ) -> SimNode {
        let info = handle.host_info();
        let placement = info.rack_placement;
        let kind = match handle.kind() {
            DeviceKind::Switch => SimNodeKind::Switch,
            DeviceKind::PowerShelf => SimNodeKind::PowerShelf,
            // A DPU is never a top-level simulator, so anything else here is
            // a compute tray.
            _ => SimNodeKind::Compute,
        };
        let (slot_number, tray_index) = match kind {
            SimNodeKind::Compute => (
                placement.and_then(RackPlacement::chassis_physical_slot_number),
                placement.and_then(RackPlacement::compute_tray_index),
            ),
            SimNodeKind::Switch => (
                placement.and_then(RackPlacement::switch_slot_number),
                placement.and_then(RackPlacement::switch_tray_index),
            ),
            SimNodeKind::PowerShelf => (None, None),
        };
        SimNode {
            kind: Some(kind),
            bmc_mac: rms_sim::normalize_mac(&info.bmc_mac_address.to_string()),
            bmc_ip: bmc_ip.map(|ip| ip.to_string()),
            host_mac: info
                .nvos_mac_addresses
                .first()
                .and_then(|mac| rms_sim::normalize_mac(&mac.to_string())),
            host_ip: host_ip.map(|ip| ip.to_string()),
            rack_id: None,
            slot_number,
            tray_index: tray_index.map(u32::from),
        }
    }
}

// This adapter connects machine-a-tron's live control state to the hosted UFM mock. It lets the
// mock consume the same in-process inventory when `include_local_inventory` is enabled, without
// polling machine-a-tron over HTTP.
impl InventoryProvider for ControlState {
    fn inventory_snapshot(&self) -> InventorySnapshot {
        let status = self.devices_status();
        InventorySnapshot {
            inventory_id: status.inventory_id,
            epoch_id: status.epoch_id,
            generation: status.generation,
            machines: status
                .devices
                .into_iter()
                .filter(|device| device.device_kind == DeviceKind::Machine)
                .map(|device| UfmInventoryMachine {
                    mat_id: MatId::from(device.mat_id),
                    machine_id: device.machine_id.map(MachineId::from),
                    infiniband_ports: device.infiniband_ports.map(|ports| {
                        ports
                            .into_iter()
                            .map(|port| InventoryPort {
                                guid: port.guid,
                                state: port.state,
                            })
                            .collect()
                    }),
                })
                .collect(),
        }
    }
}

#[derive(Clone)]
struct ControlRouter {
    inner: Option<Router>,
    control_state: ControlState,
}

async fn get_machines_status(State(state): State<ControlRouter>) -> Json<DevicesStatusResponse> {
    Json(state.control_state.devices_status())
}

async fn get_racks_status(State(state): State<ControlRouter>) -> Json<crate::RacksStatusResponse> {
    Json(
        state
            .control_state
            .simulators
            .racks_status(&state.control_state.status_config),
    )
}

async fn get_rack_status(
    State(state): State<ControlRouter>,
    Path(rack_id): Path<String>,
) -> Response {
    state
        .control_state
        .simulators
        .rack_status(&RackId::new(rack_id), &state.control_state.status_config)
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "rack not found").into_response())
}

async fn get_machines_ui() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn list_bmc_injection_rules(
    State(state): State<ControlRouter>,
    Path(id): Path<String>,
) -> Response {
    let Some(device) = state.control_state.device(&id) else {
        return device_not_found();
    };
    Json(list_rules(&device)).into_response()
}

async fn upsert_bmc_injection_rule(
    State(state): State<ControlRouter>,
    Path(id): Path<String>,
    Json(rule): Json<Rule>,
) -> Response {
    let Some(device) = state.control_state.device(&id) else {
        return device_not_found();
    };
    device.upsert(rule);
    Json(list_rules(&device)).into_response()
}

async fn delete_bmc_injection_rule(
    State(state): State<ControlRouter>,
    Path((id, rule_id)): Path<(String, String)>,
) -> Response {
    let Some(device) = state.control_state.device(&id) else {
        return device_not_found();
    };
    let rule_id = RuleId::from(rule_id);
    if device.delete(&rule_id) {
        Json(list_rules(&device)).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            format!("BMC injection rule not found: {rule_id}"),
        )
            .into_response()
    }
}

fn list_rules(device: &InjectionStore) -> Vec<Rule> {
    device
        .list()
        .into_iter()
        .map(|rule| (*rule).clone())
        .collect()
}

fn device_not_found() -> Response {
    (StatusCode::NOT_FOUND, "device not found").into_response()
}

async fn process(State(mut state): State<ControlRouter>, request: Request<Body>) -> Response {
    let Some(inner) = state.inner.as_mut() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    call_inner_router(inner, request).await
}

async fn call_inner_router(router: &mut Router, request: Request<Body>) -> Response {
    let (head, body) = request.into_parts();

    let mut rb = Request::builder().uri(&head.uri).method(&head.method);
    for (key, value) in &head.headers {
        rb = rb.header(key, value);
    }
    let inner_request = rb.body(body).unwrap();

    router.call(inner_request).await.expect("Infallible error")
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use axum::routing::get;
    use bmc_mock::{HardwareType, RackInfo, RackType};
    use carbide_uuid::rack::{RackId, RackProfileId};
    use rms_sim::RmsInventory;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{ControlState, append};
    use crate::DeviceHandle;
    use crate::device_simulator::DeviceSimulator;
    use crate::dpu_machine::DpuMachineHandle;
    use crate::rack::{RackMemberRegistration, RackRegistration};
    use crate::simulator_registry::SimulatorRegistry;
    use crate::status::DeviceStatusConfig;

    fn control_state(handles: Vec<DeviceHandle>) -> ControlState {
        ControlState::new(
            SimulatorRegistry::try_from_handles(handles).unwrap(),
            DeviceStatusConfig::new(1266),
            "mat-06:00:00:00:00:00".into(),
        )
    }

    fn rack_control_state(handle: DeviceHandle) -> ControlState {
        rack_control_state_for(vec![handle], vec![rack_registration("rack-001", "test")])
    }

    fn rack_registration(rack_id: &str, machine_config_section: &str) -> RackRegistration {
        RackRegistration {
            rack_id: RackId::new(rack_id),
            rack_profile_id: RackProfileId::new("test-profile"),
            rack_type: RackType::WiwynnGb200Nvl72,
            version: 1,
            members: vec![RackMemberRegistration {
                placement: RackInfo {
                    rack_type: RackType::WiwynnGb200Nvl72,
                }
                .placement(11),
                hardware_type: HardwareType::WiwynnGB200Nvl,
                machine_config_section: machine_config_section.to_string(),
            }],
        }
    }

    fn rack_control_state_for(
        handles: Vec<DeviceHandle>,
        registrations: Vec<RackRegistration>,
    ) -> ControlState {
        ControlState::new(
            SimulatorRegistry::builder()
                .devices(
                    handles
                        .into_iter()
                        .map(DeviceSimulator::from_handle)
                        .collect(),
                )
                .racks(registrations)
                .build()
                .unwrap(),
            DeviceStatusConfig::new(1266),
            "mat-06:00:00:00:00:00".into(),
        )
    }

    #[tokio::test]
    async fn machines_status_does_not_require_bmc_routes() {
        let router = append(None, control_state(Vec::new()));

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/machines/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["inventory_id"], "mat-06:00:00:00:00:00");
        assert!(
            body["epoch_id"]
                .as_str()
                .is_some_and(|value| { chrono::DateTime::parse_from_rfc3339(value).is_ok() })
        );
        assert_eq!(body["generation"], 1);
        assert_eq!(body["machines"], serde_json::json!([]));
    }

    #[test]
    fn rms_inventory_is_shared_until_a_device_changes() {
        let handle = DeviceHandle::for_control_test(Vec::new(), None);
        let state = control_state(vec![handle.clone()]);

        let first = state.nodes();
        let second = state.nodes();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].bmc_mac.as_deref(), Some("020000000002"));
        assert_eq!(first[0].bmc_ip, None);

        handle.set_control_test_bmc_ip(Some(Ipv4Addr::new(10, 0, 0, 7)));
        let third = state.nodes();
        assert!(!Arc::ptr_eq(&second, &third));
        assert_eq!(third[0].bmc_ip.as_deref(), Some("10.0.0.7"));
        assert!(Arc::ptr_eq(&third, &state.nodes()));
    }

    #[tokio::test]
    async fn machines_ui_returns_html() {
        let router = append(
            Some(Router::new().route("/redfish/v1", get(|| async { "bmc" }))),
            control_state(Vec::new()),
        );

        let response = router
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("machine-a-tron machines"));
    }

    #[tokio::test]
    async fn machines_status_reports_each_bmc_console_endpoint() {
        let first = DeviceHandle::for_control_test(Vec::new(), Some(16_020))
            .with_control_test_ssh_endpoint(22_020);
        let second = DeviceHandle::for_control_test(Vec::new(), Some(16_021));
        let without_ipmi = DeviceHandle::for_control_test(Vec::new(), None);
        let router = append(None, control_state(vec![first, second, without_ipmi]));

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/machines/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let machines = status["machines"].as_array().unwrap();

        assert_eq!(machines[0]["bmc"]["ipmi"]["reachable_port"], 16_020);
        assert_eq!(machines[0]["bmc"]["ipmi"]["listen_port"], 16_020);
        assert_eq!(machines[0]["bmc"]["ssh"]["reachable_port"], 22_020);
        assert_eq!(machines[0]["bmc"]["ssh"]["listen_port"], 22_020);
        assert_eq!(machines[1]["bmc"]["ipmi"]["reachable_port"], 16_021);
        assert_eq!(machines[1]["bmc"]["ipmi"]["listen_port"], 16_021);
        assert!(machines[1]["bmc"].get("ssh").is_none());
        assert!(machines[2]["bmc"].get("ipmi").is_none());
        assert!(machines[2]["bmc"].get("ssh").is_none());
    }

    #[tokio::test]
    async fn rack_status_projects_devices_from_machines_status() {
        let handle = DeviceHandle::for_control_test(Vec::new(), None);
        let mat_id = handle.mat_id().to_string();
        let router = append(None, rack_control_state(handle));

        let machines_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/machines/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let machines_body = to_bytes(machines_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let machines: serde_json::Value = serde_json::from_slice(&machines_body).unwrap();

        let rack_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/racks/rack-001/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rack_response.status(), StatusCode::OK);
        let rack_body = to_bytes(rack_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rack: serde_json::Value = serde_json::from_slice(&rack_body).unwrap();

        assert_eq!(machines["machines"].as_array().unwrap().len(), 1);
        assert_eq!(machines["machines"][0]["mat_id"], mat_id);
        assert_eq!(rack["rack_id"], "rack-001");
        assert_eq!(rack["rack_type"], "wiwynn_gb200_nvl72");
        assert_eq!(rack["members"].as_array().unwrap().len(), 1);
        assert_eq!(rack["members"][0]["mat_id"], mat_id);
        assert_eq!(rack["members"][0]["position"], 11);

        let racks_response = router
            .oneshot(
                Request::builder()
                    .uri("/racks/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let racks_body = to_bytes(racks_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let racks: serde_json::Value = serde_json::from_slice(&racks_body).unwrap();
        assert_eq!(racks["racks"][0], rack);
    }

    #[tokio::test]
    async fn rack_status_requires_known_rack() {
        let router = append(None, control_state(Vec::new()));

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/racks/unknown/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"rack not found");
    }

    #[tokio::test]
    async fn rack_status_is_isolated_by_rack_id() {
        let first = DeviceHandle::for_control_test_in_section("rack-one");
        let second = DeviceHandle::for_control_test_in_section("rack-two");
        let first_id = first.mat_id().to_string();
        let second_id = second.mat_id().to_string();
        let router = append(
            None,
            rack_control_state_for(
                vec![first, second],
                vec![
                    rack_registration("rack-001", "rack-one"),
                    rack_registration("rack-002", "rack-two"),
                ],
            ),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/racks/rack-001/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let rack: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(rack["members"].as_array().unwrap().len(), 1);
        assert_eq!(rack["members"][0]["mat_id"], first_id);
        assert_ne!(rack["members"][0]["mat_id"], second_id);
    }

    #[tokio::test]
    async fn bmc_injection_rules_require_known_device() {
        let router = append(None, control_state(Vec::new()));

        let get_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/machines/unknown/bmc/injection/rules")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"device not found");

        let post_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/machines/unknown/bmc/injection/rules")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":"test","selector":{"Path":{"method":"GET","glob":"/**"}},"action":{"Status":503}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::NOT_FOUND);

        let delete_response = router
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/machines/unknown/bmc/injection/rules/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bmc_injection_rules_accept_dpu_id() {
        let dpu_id = Uuid::new_v4();
        let observed_dpu_id = "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng"
            .parse()
            .unwrap();
        let dpu = DpuMachineHandle::for_control_test(dpu_id, Some(observed_dpu_id));
        let host = DeviceHandle::for_control_test(vec![dpu], None);
        let router = append(None, control_state(vec![host.clone()]));

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/machines/{dpu_id}/bmc/injection/rules"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":"dpu-test","selector":{"Path":{"method":"GET","glob":"/**"}},"action":{"Status":503}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("dpu-test"));

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/machines/{observed_dpu_id}/bmc/injection/rules"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("dpu-test"));
        assert!(host.bmc_injection_store().list().is_empty());
    }

    #[tokio::test]
    async fn unmatched_paths_forward_to_inner_router() {
        let router = append(
            Some(Router::new().route("/redfish/v1", get(|| async { "bmc" }))),
            control_state(Vec::new()),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"bmc");
    }

    #[tokio::test]
    async fn unmatched_paths_return_not_found_without_inner_router() {
        let router = append(None, control_state(Vec::new()));

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
