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

//! Loopback fakes shared by the gateway wire tests: machine-a-tron instances whose inventory can
//! be restarted or taken down and which serve the RMS mock for the racks they declare, a
//! `mat-k8s-controller` source list that can be edited while the gateway watches it, and helpers
//! that query the UFM API the gateway serves.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager::{
    Endpoint, NetworkInterface, NodeInfo, NodeOperationStats, NodeSet,
};
use mac_address::MacAddress;
use mat_protocol_gateway::{ControllerConfig, ExitReason, GatewayConfig, SourceListClient, run};
use reqwest::header::AUTHORIZATION;
use rms_mock::{RmsMock, RmsMockConfig, SimNode, SimNodeKind, StaticInventory};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tower::ServiceExt;
use ufm_mock::UfmAuthToken;

/// UFM HTTP Basic credential every fake gateway is started with.
pub(crate) const TOKEN: &str = "wire-test-token";

// GUIDs as machine-a-tron reports them and as the UFM API prints them (no `0x`).
pub(crate) const GUID_A: &str = "0x000000000000000a";
pub(crate) const GUID_B: &str = "0x000000000000000b";
pub(crate) const GUID_B2: &str = "0x00000000000000b2";
pub(crate) const GUID_C: &str = "0x000000000000000c";
pub(crate) const PORT_A: &str = "000000000000000a";
pub(crate) const PORT_B: &str = "000000000000000b";
pub(crate) const PORT_B2: &str = "00000000000000b2";
pub(crate) const PORT_C: &str = "000000000000000c";

/// Long enough for several 20ms watch ticks on a loaded host, short enough to keep the suite
/// fast; assertions that something does NOT happen wait this long.
pub(crate) const QUIET_PERIOD: Duration = Duration::from_millis(300);

/// Upper bound for anything that must eventually happen.
pub(crate) const WAIT: Duration = Duration::from_secs(10);

/// Serves `router` on an ephemeral loopback port for the rest of the test.
pub(crate) async fn serve(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    address
}

/// Reserves a loopback port and releases it so that [`run`] can bind it.
///
/// `run` reports no bound address, so a test that must reach a gateway started through `run`, or
/// restart it on the same port, picks the port up front.
pub(crate) async fn free_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

/// Polls `condition` every 25ms until it holds or [`WAIT`] elapses.
pub(crate) async fn wait_until<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(WAIT, async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// Inventory of one fake machine-a-tron as `/machines/status` reports it.
struct MachineATronState {
    /// Changes when the fake "pod" restarts, like `machine_a_tron::status` does.
    epoch: u64,
    /// Resets to 1 with every epoch.
    generation: u64,
    /// InfiniBand port GUIDs advertised for the single machine.
    guids: Vec<String>,
    /// Rack ids reported by `/racks/status`.
    racks: Vec<String>,
    /// False makes the status routes answer 503, as a Service without endpoints would fail.
    available: bool,
}

/// The RMS mock behind a fake's listener and what it was built from, so that a restart can put a
/// fresh one, with an empty job table, behind the same listener.
struct RmsState {
    devices: Arc<[SimNode]>,
    config: RmsMockConfig,
    mock: Arc<RmsMock>,
}

impl RmsState {
    fn new(devices: Arc<[SimNode]>, config: RmsMockConfig) -> Self {
        let mock = Self::build(&devices, &config);
        Self {
            devices,
            config,
            mock,
        }
    }

    fn build(devices: &Arc<[SimNode]>, config: &RmsMockConfig) -> Arc<RmsMock> {
        Arc::new(RmsMock::new(
            Arc::new(StaticInventory::new(devices.clone())),
            config.clone(),
        ))
    }

    fn restart(&mut self) {
        self.mock = Self::build(&self.devices, &self.config);
    }
}

/// One fake machine-a-tron exposing `/machines/status` in the shape of
/// `machine_a_tron::status::DevicesStatusResponse`, including fields UFM ignores, `/racks/status`
/// with the racks it declares, and the RMS mock over the devices it simulates, all on one
/// listener like the real one.
#[derive(Clone)]
pub(crate) struct FakeMachineATron {
    name: String,
    state: Arc<Mutex<MachineATronState>>,
    rms: Arc<Mutex<RmsState>>,
    address: SocketAddr,
}

impl FakeMachineATron {
    /// Starts an instance called `name` advertising `guids` on one machine and simulating no
    /// racks.
    pub(crate) async fn start(name: &str, guids: &[&str]) -> Self {
        Self::start_with_racks(name, guids, &[], Vec::new()).await
    }

    /// Starts an instance that also reports `racks` on `/racks/status` and answers RMS for
    /// `devices`, which carry their own rack ids and BMC MACs.
    pub(crate) async fn start_with_racks(
        name: &str,
        guids: &[&str],
        racks: &[&str],
        devices: Vec<SimNode>,
    ) -> Self {
        Self::start_with_rms(name, guids, racks, devices, RmsMockConfig::default()).await
    }

    /// Like [`Self::start_with_racks`] with the RMS mock configured by `rms`.
    pub(crate) async fn start_with_rms(
        name: &str,
        guids: &[&str],
        racks: &[&str],
        devices: Vec<SimNode>,
        rms: RmsMockConfig,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mat = Self {
            name: name.to_string(),
            state: Arc::new(Mutex::new(MachineATronState {
                epoch: 1,
                generation: 1,
                guids: guids.iter().map(ToString::to_string).collect(),
                racks: racks.iter().map(ToString::to_string).collect(),
                available: true,
            })),
            rms: Arc::new(Mutex::new(RmsState::new(devices.into(), rms))),
            address,
        };
        let rms = mat.rms.clone();
        let router = Router::new()
            .route("/machines/status", get(machines_status))
            .route("/racks/status", get(racks_status))
            .route("/", get(|| async { "machine-a-tron" }))
            .with_state(mat.clone())
            // Built per request so that a restart can swap the mock behind the listener.
            .fallback_service(tower::service_fn(move |request: Request| {
                let mock = rms.lock().unwrap().mock.clone();
                async move { rms_mock::router(mock).oneshot(request).await }
            }));
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        mat
    }

    /// Changes the racks `/racks/status` reports from the next poll on.
    pub(crate) fn set_racks(&self, racks: &[&str]) {
        self.state.lock().unwrap().racks = racks.iter().map(ToString::to_string).collect();
    }

    /// `(name, address)` pair in the form [`FakeController`] publishes.
    pub(crate) fn source(&self) -> (String, SocketAddr) {
        (self.name.clone(), self.address)
    }

    /// Makes the status routes fail (503) or succeed again.
    pub(crate) fn set_available(&self, available: bool) {
        self.state.lock().unwrap().available = available;
    }

    /// Replays a machine-a-tron pod restart behind the same Service: a new `epoch_id`, the
    /// generation back at 1, whatever ports the new process advertises, and an RMS mock that
    /// knows nothing of the jobs the previous one issued.
    pub(crate) fn restart(&self, guids: &[&str]) {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.generation = 1;
        state.guids = guids.iter().map(ToString::to_string).collect();
        state.available = true;
        self.rms.lock().unwrap().restart();
    }
}

async fn machines_status(State(mat): State<FakeMachineATron>) -> Response {
    let state = mat.state.lock().unwrap();
    if !state.available {
        return (StatusCode::SERVICE_UNAVAILABLE, "no endpoints").into_response();
    }
    let ports = state
        .guids
        .iter()
        .map(|guid| json!({ "guid": guid, "state": "active" }))
        .collect::<Vec<_>>();
    Json(json!({
        "inventory_id": format!("{}-inventory", mat.name),
        "epoch_id": format!("{}-epoch-{}", mat.name, state.epoch),
        "generation": state.generation,
        "machines": [{
            "mat_id": mat.name,
            "device_kind": "machine",
            "device_id": format!("{}-device", mat.name),
            "machine_id": format!("{}-machine", mat.name),
            "api_state": "Ready",
            "power_state": "On",
            "infiniband_ports": ports,
            "bmc": { "redfish": { "reachable_port": 443, "listen_port": 1266 } },
            "dpus": []
        }]
    }))
    .into_response()
}

async fn racks_status(State(mat): State<FakeMachineATron>) -> Response {
    let state = mat.state.lock().unwrap();
    if !state.available {
        return (StatusCode::SERVICE_UNAVAILABLE, "no endpoints").into_response();
    }
    let racks = state
        .racks
        .iter()
        .map(|rack_id| {
            json!({
                "rack_id": rack_id,
                "rack_type": "wiwynn_gb200_nvl72",
                "version": 1,
                "members": [],
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "racks": racks })).into_response()
}

/// Fake `mat-k8s-controller` source list whose generation, readiness and sources can be changed
/// while the gateway watches it, to replay controller restarts and fleet changes.
#[derive(Clone)]
pub(crate) struct FakeController {
    pub(crate) generation: Arc<AtomicU64>,
    pub(crate) ready: Arc<AtomicBool>,
    sources: Arc<Mutex<Vec<(String, SocketAddr)>>>,
    /// Number of `GET /v1/sources` requests served so far.
    pub(crate) fetches: Arc<AtomicU64>,
}

impl FakeController {
    pub(crate) fn new(sources: Vec<(String, SocketAddr)>, generation: u64) -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(generation)),
            ready: Arc::new(AtomicBool::new(true)),
            sources: Arc::new(Mutex::new(sources)),
            fetches: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) async fn serve(&self) -> SocketAddr {
        serve(
            Router::new()
                .route("/v1/sources", get(sources))
                .with_state(self.clone()),
        )
        .await
    }

    pub(crate) fn set_sources(&self, sources: Vec<(String, SocketAddr)>) {
        *self.sources.lock().unwrap() = sources;
    }

    /// Publishes `sources` under the next generation, as the Go registry does after its
    /// debounce window when the set changed.
    pub(crate) fn publish(&self, sources: Vec<(String, SocketAddr)>) {
        self.set_sources(sources);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Replays what the Go registry publishes while the controller container restarts: first
    /// generation 0 with `ready = false` and no sources, then the rediscovered `sources` as
    /// generation 1.
    pub(crate) async fn restart_with(
        &self,
        sources: Vec<(String, SocketAddr)>,
        downtime: Duration,
    ) {
        self.ready.store(false, Ordering::SeqCst);
        self.generation.store(0, Ordering::SeqCst);
        self.set_sources(Vec::new());
        tokio::time::sleep(downtime).await;
        self.set_sources(sources);
        self.generation.store(1, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
    }

    pub(crate) fn fetches(&self) -> u64 {
        self.fetches.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait_for_fetches(&self, at_least: u64) {
        wait_until("the gateway fetches the source list", || async move {
            self.fetches() >= at_least
        })
        .await;
    }
}

async fn sources(State(controller): State<FakeController>) -> Json<Value> {
    controller.fetches.fetch_add(1, Ordering::SeqCst);
    let sources = controller
        .sources
        .lock()
        .unwrap()
        .iter()
        .map(|(name, address)| {
            json!({
                "name": name,
                "base_url": format!("http://{address}"),
                "pod": format!("{name}-pod"),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "generation": controller.generation.load(Ordering::SeqCst),
        "ready": controller.ready.load(Ordering::SeqCst),
        "sources": sources,
    }))
}

/// Gateway configuration with millisecond intervals, listening on `listen`.
///
/// `startup_attempts` x `startup_retry_interval` is 10s so that a controller replayed as
/// not-ready for a while does not exhaust the bootstrap bound on a loaded host.
pub(crate) fn gateway_config(controller: SocketAddr, listen: SocketAddr) -> GatewayConfig {
    let mut config = GatewayConfig {
        listen_address: listen,
        controller: ControllerConfig {
            sources_url: format!("http://{controller}/v1/sources").parse().unwrap(),
            poll_interval: Duration::from_millis(20),
            request_timeout: Duration::from_secs(2),
            startup_attempts: 500,
            startup_retry_interval: Duration::from_millis(20),
        },
        ..GatewayConfig::default()
    };
    config.ufm.inventory.poll_interval = Duration::from_millis(50);
    config.ufm.inventory.request_timeout = Duration::from_secs(2);
    config.ownership.poll_interval = Duration::from_millis(50);
    config.ownership.request_timeout = Duration::from_secs(2);
    config.ownership.stale_after = Duration::from_millis(400);
    config.validate().unwrap();
    config
}

/// Like [`gateway_config`] with a port only `run` will bind; for tests that serve the router
/// themselves or never reach the listener.
pub(crate) fn unbound_gateway_config(controller: SocketAddr) -> GatewayConfig {
    gateway_config(controller, "127.0.0.1:0".parse().unwrap())
}

pub(crate) fn source_list_client(config: &GatewayConfig) -> SourceListClient {
    SourceListClient::new(
        config.controller.sources_url.clone(),
        config.controller.request_timeout,
    )
    .unwrap()
}

pub(crate) fn auth_token() -> UfmAuthToken {
    UfmAuthToken::new(TOKEN.to_string()).unwrap()
}

/// Runs the gateway process loop on a task, as `main` does, until `shutdown` or a source set
/// change.
pub(crate) fn spawn_run(
    config: GatewayConfig,
    shutdown: CancellationToken,
) -> JoinHandle<eyre::Result<ExitReason>> {
    tokio::spawn(async move { run(config, &auth_token(), shutdown).await })
}

/// Waits for the spawned `run` to return and unwraps its result.
pub(crate) async fn finished(running: JoinHandle<eyre::Result<ExitReason>>) -> ExitReason {
    tokio::time::timeout(WAIT, running)
        .await
        .expect("run must return")
        .expect("run task must not panic")
        .expect("run must not fail")
}

/// GUID and link states of one UFM port as `/ufmRestV3/resources/ports` reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UfmPort {
    pub(crate) guid: String,
    pub(crate) logical_state: String,
    pub(crate) physical_state: String,
}

/// Ports the gateway at `gateway` currently serves, sorted by GUID; `None` while nothing answers,
/// which is the case until `run` has bound its listener.
pub(crate) async fn try_ufm_ports(
    client: &reqwest::Client,
    gateway: SocketAddr,
) -> Option<Vec<UfmPort>> {
    let ports: Vec<Value> = client
        .get(format!("http://{gateway}/ufmRestV3/resources/ports"))
        .header(AUTHORIZATION, format!("Basic {TOKEN}"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut ports = ports
        .iter()
        .map(|port| UfmPort {
            guid: port["guid"].as_str().unwrap().to_string(),
            logical_state: port["logical_state"].as_str().unwrap().to_string(),
            physical_state: port["physical_state"].as_str().unwrap().to_string(),
        })
        .collect::<Vec<_>>();
    ports.sort_by(|left, right| left.guid.cmp(&right.guid));
    Some(ports)
}

/// Like [`try_ufm_ports`] for a gateway that is known to be listening.
pub(crate) async fn ufm_ports(client: &reqwest::Client, gateway: SocketAddr) -> Vec<UfmPort> {
    try_ufm_ports(client, gateway)
        .await
        .expect("the gateway must answer the UFM ports request")
}

/// Waits until the gateway serves exactly `expected` as `(guid, logical_state)` pairs.
pub(crate) async fn wait_for_ports(
    client: &reqwest::Client,
    gateway: SocketAddr,
    expected: &[(&str, &str)],
) -> Vec<UfmPort> {
    let expected = expected
        .iter()
        .map(|(guid, state)| (guid.to_string(), state.to_string()))
        .collect::<Vec<_>>();
    tokio::time::timeout(WAIT, async {
        loop {
            let Some(ports) = try_ufm_ports(client, gateway).await else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            let observed = ports
                .iter()
                .map(|port| (port.guid.clone(), port.logical_state.clone()))
                .collect::<Vec<_>>();
            if observed == expected {
                return ports;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until UFM ports are {expected:?}"))
}

/// Status of an unauthenticated GET against the gateway; `None` when nothing answers.
pub(crate) async fn probe(
    client: &reqwest::Client,
    gateway: SocketAddr,
    path: &str,
) -> Option<StatusCode> {
    client
        .get(format!("http://{gateway}{path}"))
        .send()
        .await
        .ok()
        .map(|response| response.status())
}

// BMC MACs of the simulated devices the RMS routing tests name.
pub(crate) const SWITCH_A: [u8; 6] = [0x02, 0x00, 0x11, 0x11, 0x22, 0x22];
pub(crate) const TRAY_A: [u8; 6] = [0x02, 0x00, 0xab, 0xcd, 0x12, 0x34];
pub(crate) const SWITCH_B: [u8; 6] = [0x02, 0x00, 0x22, 0x22, 0x33, 0x33];

fn device(kind: SimNodeKind, mac: [u8; 6], rack_id: &str, slot: u32) -> SimNode {
    SimNode {
        kind: Some(kind),
        bmc_mac: Some(MacAddress::new(mac)),
        rack_id: Some(rack_id.to_string()),
        slot_number: Some(slot),
        tray_index: (kind == SimNodeKind::Compute).then_some(2),
        ..SimNode::default()
    }
}

/// The devices of the instance owning `rack-001`: a switch and a compute tray.
pub(crate) fn devices_a() -> Vec<SimNode> {
    vec![
        device(SimNodeKind::Switch, SWITCH_A, "rack-001", 30),
        device(SimNodeKind::Compute, TRAY_A, "rack-001", 12),
    ]
}

/// The devices of the instance owning `rack-002`: one switch.
pub(crate) fn devices_b() -> Vec<SimNode> {
    vec![device(SimNodeKind::Switch, SWITCH_B, "rack-002", 30)]
}

/// A node the way NICo names it: an opaque id, its rack, and the BMC MAC the mock matches on.
pub(crate) fn node(node_id: &str, rack_id: &str, mac: [u8; 6]) -> NodeInfo {
    NodeInfo {
        node_id: node_id.to_string(),
        rack_id: rack_id.to_string(),
        bmc_endpoint: Some(Endpoint {
            interface: Some(NetworkInterface {
                ip_address: String::new(),
                mac_address: MacAddress::new(mac).to_string(),
                host_name: None,
            }),
            port: 443,
            credentials: None,
        }),
        ..NodeInfo::default()
    }
}

pub(crate) fn nodes(nodes: Vec<NodeInfo>) -> Option<NodeSet> {
    Some(NodeSet { nodes })
}

pub(crate) async fn rms_client(gateway: SocketAddr) -> RackManagerClient<Channel> {
    RackManagerClient::connect(format!("http://{gateway}"))
        .await
        .expect("the gateway listener speaks gRPC")
}

pub(crate) async fn wait_until_ready(http: &reqwest::Client, gateway: SocketAddr) {
    wait_until("the gateway reports ready", || async {
        probe(http, gateway, "/readyz").await == Some(StatusCode::OK)
    })
    .await;
}

/// `(total, successful, failed)` of a batch's stats.
pub(crate) fn counts(stats: Option<&NodeOperationStats>) -> (u32, u32, u32) {
    let stats = stats.expect("a batch carries stats");
    (
        stats.total_nodes,
        stats.successful_nodes,
        stats.failed_nodes,
    )
}
