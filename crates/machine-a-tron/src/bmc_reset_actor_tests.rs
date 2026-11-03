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
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use bmc_mock::mac_address_pool::{MacAddressPool, PoolConfig};
use bmc_mock::{HardwareType, ResourceResetType};
use dhcproto::v4::relay::{RelayCode, RelayInfo};
use dhcproto::v4::{DhcpOption, HType, Message, MessageType, Opcode, OptionCode};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use mac_address::MacAddress;
use nv_redfish::schema::resource::PowerState;
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::dhcp_wrapper_udp::UdpDhcpClient;
use crate::host_machine::HostMachine;
use crate::power_shelf_simulator::PowerShelfActor;
use crate::switch_simulator::SwitchActor;
use crate::{BmcMockRegistry, DeviceHandle, DhcpClient, DpuMachineHandle, MachineATronContext};

#[derive(Clone)]
struct ObservedRequest {
    mac: MacAddress,
    kind: MessageType,
    vendor: Option<Vec<u8>>,
    link: Ipv4Addr,
    rejected: bool,
}

#[derive(Default)]
struct Leases {
    epoch: u8,
    indexes: HashMap<MacAddress, u8>,
    suppressed: HashSet<MacAddress>,
    requests: Vec<ObservedRequest>,
}

struct DhcpFixture {
    leases: Arc<Mutex<Leases>>,
    changed: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DhcpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DhcpFixture {
    async fn start() -> (Self, UdpDhcpClient, crate::UdpDhcpService) {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let SocketAddr::V4(server) = socket.local_addr().unwrap() else {
            unreachable!()
        };
        let (client, service) = UdpDhcpClient::start(
            server,
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
            Ipv4Addr::LOCALHOST,
        )
        .await
        .unwrap();
        let leases = Arc::new(Mutex::new(Leases {
            epoch: 10,
            ..Default::default()
        }));
        let changed = Arc::new(Notify::new());
        let server_leases = leases.clone();
        let server_changed = changed.clone();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            loop {
                let (length, destination) = socket.recv_from(&mut buffer).await.unwrap();
                let request = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                assert_eq!(request.giaddr(), Ipv4Addr::LOCALHOST);
                let mac = MacAddress::new(request.chaddr()[..6].try_into().unwrap());
                let kind = request.opts().msg_type().unwrap();
                let vendor = match request.opts().get(OptionCode::ClassIdentifier) {
                    Some(DhcpOption::ClassIdentifier(value)) => Some(value.clone()),
                    None => None,
                    other => panic!("unexpected vendor class: {other:?}"),
                };
                let Some(DhcpOption::RelayAgentInformation(relay)) =
                    request.opts().get(OptionCode::RelayAgentInformation)
                else {
                    panic!("missing relay agent information")
                };
                let Some(RelayInfo::LinkSelection(link)) = relay.get(RelayCode::LinkSelection)
                else {
                    panic!("missing link selection")
                };
                let (assigned, rejected) = {
                    let mut state = server_leases.lock().unwrap();
                    let next = u8::try_from(state.indexes.len() + 1).unwrap();
                    let index = *state.indexes.entry(mac).or_insert(next);
                    let rejected = state.suppressed.contains(&mac);
                    let address = Ipv4Addr::new(192, 0, state.epoch, index);
                    state.requests.push(ObservedRequest {
                        mac,
                        kind,
                        vendor,
                        link: *link,
                        rejected,
                    });
                    (address, rejected)
                };
                let response_type = if rejected {
                    MessageType::Nak
                } else {
                    match kind {
                        MessageType::Discover => MessageType::Offer,
                        MessageType::Request => MessageType::Ack,
                        other => panic!("unexpected DHCP message: {other:?}"),
                    }
                };
                let mut response = Message::default();
                response
                    .set_opcode(Opcode::BootReply)
                    .set_htype(HType::Eth)
                    .set_xid(request.xid())
                    .set_chaddr(request.chaddr())
                    .set_giaddr(request.giaddr())
                    .set_yiaddr(assigned);
                response
                    .opts_mut()
                    .insert(DhcpOption::MessageType(response_type));
                response
                    .opts_mut()
                    .insert(DhcpOption::ServerIdentifier(Ipv4Addr::LOCALHOST));
                let mut encoded = Vec::new();
                response.encode(&mut Encoder::new(&mut encoded)).unwrap();
                socket.send_to(&encoded, destination).await.unwrap();
                server_changed.notify_waiters();
            }
        });
        (
            Self {
                leases,
                changed,
                task,
            },
            client,
            service,
        )
    }

    fn reject_count(&self, mac: MacAddress) -> usize {
        self.leases
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.mac == mac && request.rejected)
            .count()
    }

    fn suppress(&self, macs: &[MacAddress]) -> HashMap<MacAddress, usize> {
        let counts = macs
            .iter()
            .map(|&mac| (mac, self.reject_count(mac)))
            .collect();
        self.leases.lock().unwrap().suppressed = macs.iter().copied().collect();
        counts
    }

    async fn wait_for_rejections(&self, baseline: &HashMap<MacAddress, usize>, additional: usize) {
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let changed = self.changed.notified();
                if baseline
                    .iter()
                    .all(|(&mac, &count)| self.reject_count(mac) >= count + additional)
                {
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("BMC DHCP attempts did not reach required rejection count {additional}")
        });
    }

    fn assert_rejections(&self, baseline: &HashMap<MacAddress, usize>, additional: usize) {
        for (&mac, &count) in baseline {
            assert_eq!(
                self.reject_count(mac),
                count + additional,
                "unexpected DHCP attempt count for {mac}"
            );
        }
    }

    async fn assert_no_bmc_request(&self, macs: &[MacAddress]) {
        let count = || {
            self.leases
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|request| macs.contains(&request.mac))
                .count()
        };
        let baseline = count();
        // A failed first attempt must retry after 4s +/- 1s. Observing beyond
        // that bound proves pause/shutdown stopped retry advancement.
        assert!(
            tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    let changed = self.changed.notified();
                    if count() != baseline {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .is_err(),
            "BMC DHCP continued while actors were paused or stopped"
        );
    }

    fn lift(&self, epoch: u8) {
        let mut state = self.leases.lock().unwrap();
        state.epoch = epoch;
        state.suppressed.clear();
    }
}

#[derive(Clone)]
enum BmcActor {
    Device(DeviceHandle),
    Dpu(DpuMachineHandle),
}
impl BmcActor {
    fn ip(&self) -> Option<Ipv4Addr> {
        match self {
            Self::Device(handle) => handle.bmc_ip(),
            Self::Dpu(handle) => handle.bmc_ip(),
        }
    }
    fn snapshot(&self) -> Value {
        match self {
            Self::Device(handle) => serde_json::to_value(handle.persisted()).unwrap(),
            Self::Dpu(handle) => serde_json::to_value(handle.persisted()).unwrap(),
        }
    }
    fn mac(&self) -> MacAddress {
        serde_json::from_value(self.snapshot()["bmc_mac_address"].clone()).unwrap()
    }
    fn generation(&self) -> u64 {
        self.snapshot()["bmc_state"]["factory_reset_generation"]
            .as_u64()
            .unwrap()
    }
    fn power(&self) -> PowerState {
        match self {
            Self::Device(handle) => handle.power_state(),
            Self::Dpu(handle) => handle.power_state(),
        }
    }
    fn unchanged_state(&self) -> Value {
        let state = self.snapshot();
        json!({ "mat_id": state["mat_id"], "serial": state["serial"],
            "installed_os": state["installed_os"], "nvos_admin": state["nvos_admin"],
            "power": format!("{:?}", self.power()) })
    }
}

async fn http(
    router: &Router,
    method: &str,
    path: &str,
    payload: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("x-auth-token", token);
    }
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        router
            .clone()
            .oneshot(request.body(Body::from(payload.to_string())).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

async fn router(registry: &BmcMockRegistry, actor: &BmcActor) -> Router {
    registry
        .read()
        .await
        .get(&actor.ip().unwrap().to_string())
        .unwrap()
        .clone()
}

async fn login(router: &Router, actor: &BmcActor) -> String {
    let account = actor.snapshot()["bmc_state"]["accounts"][0].clone();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/redfish/v1/SessionService/Sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"UserName":account["username"],"Password":account["password"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.headers()["x-auth-token"]
        .to_str()
        .unwrap()
        .to_owned()
}

async fn reset(registry: &BmcMockRegistry, actor: &BmcActor, rotate: Option<&str>, repeats: usize) {
    let router = router(registry, actor).await;
    for _ in 0..repeats {
        let token = login(&router, actor).await;
        if let Some(password) = rotate {
            let id = actor.snapshot()["bmc_state"]["accounts"][0]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let (status, _) = http(
                &router,
                "PATCH",
                &format!("/redfish/v1/AccountService/Accounts/{id}"),
                json!({"Password":password}),
                Some(&token),
            )
            .await;
            assert!(status.is_success());
        }
        let (status, managers) = http(
            &router,
            "GET",
            "/redfish/v1/Managers",
            Value::Null,
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let manager = managers["Members"][0]["@odata.id"].as_str().unwrap();
        let payload = if matches!(actor, BmcActor::Dpu(_)) {
            json!({"ResetToDefaultsType":"ResetAll"})
        } else {
            json!({"ResetType":"ResetAll"})
        };
        let generation = actor.generation();
        let (status, _) = http(
            &router,
            "POST",
            &format!("{manager}/Actions/Manager.ResetToDefaults"),
            payload,
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            actor.generation(),
            generation + 1,
            "HTTP success preceded actor publication"
        );
    }
}

async fn wait_for_addresses(actors: &[BmcActor], epoch: u8) {
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            if actors
                .iter()
                .all(|actor| actor.ip().is_some_and(|ip| ip.octets()[2] == epoch))
            {
                return;
            }
            interval.tick().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("BMC actors did not acquire lease epoch {epoch}"));
}

/// Exercise the actual HTTP callback, every physical actor and UDP DHCP path.
/// NAKs model transport-visible suppression; Core's acknowledgement semantics
/// belong to the end-to-end Core fixture.
#[tokio::test]
async fn bmc_reset_actor_recovery_preserves_devices_and_survives_pause_and_repeated_reset() {
    let (dhcp, client, service) = DhcpFixture::start().await;
    let mut context = Arc::try_unwrap(MachineATronContext::for_test())
        .unwrap_or_else(|_| panic!("test context is shared"));
    context.dhcp_client = DhcpClient::UdpRelay(client);
    context.app_config.host_bmc_password = Some("configured-host-password".into());
    context.app_config.dpu_bmc_password = Some("configured-dpu-password".into());
    context.app_config.register_expected_machines = false;
    let mut config = (*context.app_config.machines["config"]).clone();
    config.hw_type = HardwareType::WiwynnGB200Nvl;
    config.run_interval_idle = Duration::from_millis(100);
    config.run_interval_working = Duration::from_millis(100);
    config.acceleration_factor = 0.0;
    let context = Arc::new(context);
    let range = PoolConfig::new(MacAddress::new([2, 57, 50, 0, 0, 0]), 24).unwrap();
    let mut pool = MacAddressPool::new_pool(range);
    let host = HostMachine::new(
        context.clone(),
        "host".into(),
        Arc::new(config.clone()),
        &mut pool,
        range,
    )
    .start(false);
    let mut switch_config = config.clone();
    switch_config.hw_type = HardwareType::NvidiaSwitchNd5200Ld;
    let switch = SwitchActor::new(
        context.clone(),
        "switch".into(),
        Arc::new(switch_config),
        &mut pool,
        range,
    )
    .start(false);
    let mut shelf_config = config;
    shelf_config.hw_type = HardwareType::LiteOnPowerShelf;
    let shelf = PowerShelfActor::new(
        context.clone(),
        "shelf".into(),
        Arc::new(shelf_config),
        &mut pool,
        range,
    )
    .start(false);
    let devices = [
        DeviceHandle::machine(host.clone()),
        DeviceHandle::switch(switch.clone()),
        DeviceHandle::power_shelf(shelf),
    ];
    let actors = std::iter::once(BmcActor::Device(devices[0].clone()))
        .chain(host.dpus().iter().cloned().map(BmcActor::Dpu))
        .chain(devices[1..].iter().cloned().map(BmcActor::Device))
        .collect::<Vec<_>>();
    assert_eq!(actors.len(), 5);
    wait_for_addresses(&actors, 10).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while switch.nvos_ip().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if host.power_state() != PowerState::Off {
        host.set_system_power(ResourceResetType::ForceOff).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while actors[..3]
            .iter()
            .any(|actor| actor.power() != PowerState::Off)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Use the operation RMS invokes after a real NVOS password rotation, so
    // preserving a factory account cannot mask an unintended NVOS reset.
    switch
        .set_nvos_password(bmc_mock::DUMMY_NVOS_USERNAME, "retained-nvos-password")
        .unwrap();
    let frozen = actors
        .iter()
        .map(BmcActor::unchanged_state)
        .collect::<Vec<_>>();
    let nvos_ip = switch.nvos_ip();
    let macs = actors.iter().map(BmcActor::mac).collect::<Vec<_>>();

    for (cycle, epoch, repeats) in [(1, 20, 1), (2, 30, 2)] {
        let old_ips = actors
            .iter()
            .map(|actor| actor.ip().unwrap())
            .collect::<Vec<_>>();
        let baseline = dhcp.suppress(&macs);
        for actor in &actors {
            reset(
                &context.bmc_registry,
                actor,
                (cycle == 2).then_some("second-cycle-password"),
                repeats,
            )
            .await;
        }
        dhcp.wait_for_rejections(&baseline, 1).await;
        if cycle == 1 {
            for device in &devices {
                device.pause().unwrap();
            }
            // This reply acknowledges the host pause and the sends to its DPUs.
            // The absence of UDP retries below witnesses the child actors too.
            host.api_state().await.unwrap();
            // Establish that the next pending deadline is the first 4s +/- 1s
            // retry, rather than a later exponentially backed-off attempt.
            dhcp.assert_rejections(&baseline, 1);
            dhcp.assert_no_bmc_request(&macs).await;
            for device in &devices {
                device.resume().unwrap();
            }
        }
        // Two observations after one reset, or three after two rapid resets,
        // require an actor-owned retry rather than another HTTP trigger.
        dhcp.wait_for_rejections(&baseline, repeats + 1).await;
        dhcp.lift(epoch);
        wait_for_addresses(&actors, epoch).await;
        for (actor, expected) in actors.iter().zip(&frozen) {
            assert_eq!(actor.unchanged_state(), *expected);
            assert_eq!(actor.generation(), if cycle == 1 { 1 } else { 3 });
            let state = actor.snapshot();
            let accounts = state["bmc_state"]["accounts"].as_array().unwrap();
            assert_eq!(accounts.len(), 1);
            assert!(
                accounts[0]["password"] == accounts[0]["factory_default_password"],
                "re-DHCP reapplied a configured password"
            );
            let router = router(&context.bmc_registry, actor).await;
            let _factory_token = login(&router, actor).await;
        }
        assert_eq!(switch.nvos_ip(), nvos_ip, "BMC reset restarted NVOS DHCP");
        let registry = context.bmc_registry.read().await;
        for old in old_ips {
            assert!(
                !registry.contains_key(&old.to_string()),
                "old BMC route survived re-key"
            );
        }
    }

    let baseline = dhcp.suppress(&macs);
    let mut retired_routers = Vec::new();
    for actor in &actors {
        // Retain a private test handle for stopped-actor callback checks, not an authority.
        retired_routers.push(router(&context.bmc_registry, actor).await);
        reset(&context.bmc_registry, actor, None, 1).await;
    }
    dhcp.wait_for_rejections(&baseline, 1).await;
    for actor in &actors {
        assert!(
            !context
                .bmc_registry
                .read()
                .await
                .contains_key(&actor.ip().unwrap().to_string()),
            "suppressed BMC still serves its abandoned authority"
        );
    }
    for device in &devices {
        device.abort_and_wait().await.unwrap();
    }
    dhcp.assert_rejections(&baseline, 1);
    for (actor, router) in actors.iter().zip(retired_routers) {
        let token = login(&router, actor).await;
        let (_, managers) = http(
            &router,
            "GET",
            "/redfish/v1/Managers",
            Value::Null,
            Some(&token),
        )
        .await;
        let manager = managers["Members"][0]["@odata.id"].as_str().unwrap();
        let generation = actor.generation();
        let (status, _) = http(
            &router,
            "POST",
            &format!("{manager}/Actions/Manager.ResetToDefaults"),
            json!({"ResetType":"ResetAll"}),
            Some(&token),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "stopped actor accepted a reset"
        );
        assert_eq!(actor.generation(), generation);
    }
    dhcp.assert_no_bmc_request(&macs).await;
    let requests = dhcp.leases.lock().unwrap().requests.clone();
    for actor in &actors {
        let observed = requests
            .iter()
            .filter(|request| request.mac == actor.mac())
            .collect::<Vec<_>>();
        assert!(
            observed
                .iter()
                .any(|request| !request.rejected && request.kind == MessageType::Request)
        );
        assert!(observed.iter().filter(|request| request.rejected).count() >= 6);
        for request in observed {
            assert_eq!(request.link, Ipv4Addr::new(192, 168, 192, 1));
            assert_eq!(
                request.vendor.as_deref(),
                matches!(actor, BmcActor::Dpu(_)).then_some(b"NVIDIA/BF/BMC".as_slice())
            );
        }
    }
    service.shutdown().await.unwrap();
}

/// Idle actors must wake for renewal rather than their much longer FSM poll.
#[tokio::test]
async fn periodic_renewal_runs_through_host_and_dpu_actors() {
    let (dhcp, client, service) = DhcpFixture::start().await;
    let mut context = Arc::try_unwrap(MachineATronContext::for_test())
        .unwrap_or_else(|_| panic!("shared test context"));
    context.dhcp_client = DhcpClient::UdpRelay(client);
    context.app_config.bmc_dhcp_renewal_interval = Duration::from_millis(250);
    context.app_config.register_expected_machines = false;
    let mut config = (*context.app_config.machines["config"]).clone();
    config.hw_type = HardwareType::WiwynnGB200Nvl;
    config.run_interval_idle = Duration::from_secs(3);
    config.run_interval_working = Duration::from_secs(3);
    config.acceleration_factor = 0.0;
    let context = Arc::new(context);
    let range = PoolConfig::new(MacAddress::new([2, 57, 51, 0, 0, 0]), 24).unwrap();
    let mut pool = MacAddressPool::new_pool(range);
    let host = HostMachine::new(
        context.clone(),
        "renewal".into(),
        Arc::new(config),
        &mut pool,
        range,
    )
    .start(false);
    let device = DeviceHandle::machine(host.clone());
    let actors = std::iter::once(BmcActor::Device(device.clone()))
        .chain(host.dpus().iter().cloned().map(BmcActor::Dpu))
        .collect::<Vec<_>>();
    wait_for_addresses(&actors, 10).await;
    let bmc = router(&context.bmc_registry, &actors[0]).await;
    let token = login(&bmc, &actors[0]).await;
    let (status, _) = http(
        &bmc,
        "POST",
        "/redfish/v1/Chassis/BMC_0/Actions/Oem/NvidiaChassis.AuxPowerReset",
        json!({"ResetType":"AuxPowerCycle"}),
        Some(&token),
    )
    .await;
    assert!(status.is_success());
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.aux_power_cycle_generation() != 1
            || host
                .dpus()
                .iter()
                .any(|dpu| dpu.aux_power_cycle_generation() != 1)
            || actors.iter().any(|actor| actor.power() != PowerState::Off)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let requests = || {
        actors
            .iter()
            .map(|actor| {
                dhcp.leases
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .filter(|request| {
                        request.mac == actor.mac() && request.kind == MessageType::Request
                    })
                    .count()
            })
            .collect::<Vec<_>>()
    };
    let before = requests();
    let ips = actors
        .iter()
        .map(|actor| actor.ip().unwrap())
        .collect::<Vec<_>>();
    tokio::time::timeout(Duration::from_secs(2), async {
        while requests()
            .iter()
            .zip(&before)
            .any(|(now, before)| *now < before + 2)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle host and DPUs must wake for periodic renewal");
    assert_eq!(
        actors
            .iter()
            .map(|actor| actor.ip().unwrap())
            .collect::<Vec<_>>(),
        ips
    );
    dhcp.lift(20);
    tokio::time::timeout(Duration::from_secs(2), async {
        while actors
            .iter()
            .any(|actor| actor.ip().unwrap().octets()[2] != 20)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("renewal must publish changed addresses without an actor nudge");
    let routes = context.bmc_registry.read().await;
    for (actor, old) in actors.iter().zip(ips) {
        assert!(!routes.contains_key(&old.to_string()));
        assert!(routes.contains_key(&actor.ip().unwrap().to_string()));
    }
    drop(routes);
    device.abort_and_wait().await.unwrap();
    service.shutdown().await.unwrap();
}
