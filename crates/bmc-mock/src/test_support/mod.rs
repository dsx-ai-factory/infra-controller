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

use axum::routing::get;
use axum::{Json, Router};
use axum_http_client::AxumRouterHttpClient;
use mac_address::MacAddress;
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use url::Url;

use crate::injection::{Action, Rule, RuleId, Selector};
use crate::mac_address_pool::{
    Config as MacAddressConfig, MacAddressPool, PoolConfig as MacAddressPoolConfig,
    RangesConfig as MacAddressRangesConfig,
};
use crate::machine_info::DpuSettings;
use crate::{
    BmcState, Callbacks, DpuMachineInfo, HardwareType, HostMachineInfo, MachineInfo,
    MachineRouterOptions, MockPowerState, SetSystemPowerError, SystemPowerControl, machine_router,
};

pub mod axum_http_client;

#[derive(Debug)]
pub(super) struct NoopCallbacks;

impl Callbacks for NoopCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        MockPowerState::On
    }

    fn send_power_command(
        &self,
        _reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        Ok(())
    }

    fn state_refresh_indication(&self) {}
}

pub type TestBmc = HttpBmc<AxumRouterHttpClient>;

lazy_static::lazy_static! {
    pub static ref TEST_MAC_POOL: Arc<Mutex<MacAddressPool>> =
        Arc::new(Mutex::new(MacAddressPool::new(MacAddressConfig {
            pool: Some(MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 32).unwrap()),
            ranges: Some(MacAddressRangesConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 32, 8).unwrap()),
        })));
}

#[derive(Clone)]
pub struct TestBmcHandle {
    pub service_root: Arc<nv_redfish::ServiceRoot<TestBmc>>,
    pub state: BmcState,
}

async fn test_bmc((router, state): (axum::Router, BmcState)) -> TestBmcHandle {
    let client = AxumRouterHttpClient::new(router);
    let endpoint = Url::parse("https://bmc-mock.local").expect("valid URL");
    let credentials = BmcCredentials::new("root".to_string(), "password".to_string());
    let bmc = Arc::new(HttpBmc::new(
        client,
        endpoint,
        credentials,
        CacheSettings::with_capacity(32),
    ));
    TestBmcHandle {
        service_root: nv_redfish::ServiceRoot::new(bmc).await.unwrap().into(),
        state,
    }
}

pub async fn bmc_for_machine(machine_info: MachineInfo) -> TestBmcHandle {
    let machine_id = match &machine_info {
        MachineInfo::Host(_) => "test-host-id",
        MachineInfo::Dpu(_) => "test-dpu-id",
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        machine_id.to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub(super) fn host_info(hw_type: HardwareType) -> MachineInfo {
    let ndpu = hw_type.fixed_number_of_dpu().unwrap_or(0);
    let mut pool = TEST_MAC_POOL.lock().unwrap();
    let ranges_config = pool.allocate_range_config().unwrap();
    MachineInfo::Host(HostMachineInfo::new(
        hw_type,
        (0..ndpu)
            .map(|_| DpuMachineInfo::new(hw_type, &mut pool, DpuSettings::default()))
            .collect(),
        &mut pool,
        ranges_config,
    ))
}

pub async fn wiwynn_gb200_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::WiwynnGB200Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn lenovo_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::LenovoGB300Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn nvidia_dgx_h100_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::NvidiaDgxH100),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn dgx_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::NvidiaDgxGb300),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

/// Host-mode mock for the NvidiaDgxVr hardware type ("vr-tray" in machine-a-tron
/// configs). Unlike the other GB300-family types (Lenovo, Nvidia DGX GB300,
/// Supermicro), this one previously only had a DPU-mode helper
/// (`nvidia_dgx_vr_bluefield4_dpu_bmc`), so there was no way to test exploring
/// it as a host tray at all. Added while investigating #3159.
pub async fn nvidia_dgx_vr_host_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::NvidiaDgxVr),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn supermicro_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::SupermicroGb300Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn generic_supermicro_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::GenericSupermicro),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn liteon_powershelf_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::LiteOnPowerShelf),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn delta_powershelf_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::DeltaPowerShelf),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

/// Delta power shelf whose PSUs report the given per-bay on/off states under
/// `Oem.deltaenergysystems.Power`. Lets tests exercise off and mixed shelves
/// (the default [`delta_powershelf_bmc`] is an all-on six-bay shelf).
pub async fn delta_powershelf_bmc_with_psu_power(states: Vec<bool>) -> TestBmcHandle {
    let machine_info = match host_info(HardwareType::DeltaPowerShelf) {
        MachineInfo::Host(host) => MachineInfo::Host(host.with_delta_psu_power(states)),
        MachineInfo::Dpu(_) => unreachable!("Delta power shelf must be a host"),
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn nvidia_switch_nd5200_ld_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::NvidiaSwitchNd5200Ld),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn nvidia_switch_n5700_ld_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::NvidiaSwitchN5700Ld),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn dell_poweredge_r750_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::DellPowerEdgeR750),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn dell_poweredge_r750_bluefield3_bmc(settings: DpuSettings) -> TestBmcHandle {
    let machine_info = {
        let mut mac_pool = TEST_MAC_POOL.lock().unwrap();
        MachineInfo::Dpu(DpuMachineInfo::new(
            HardwareType::DellPowerEdgeR750,
            &mut mac_pool,
            settings,
        ))
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-dpu-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn dell_poweredge_r760_bluefield4_bmc(dpu: DpuMachineInfo) -> TestBmcHandle {
    let machine_info = MachineInfo::Dpu(dpu);
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-dpu-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn nvidia_dgx_vr_bluefield4_dpu_bmc(settings: DpuSettings) -> TestBmcHandle {
    let machine_info = {
        let mut mac_pool = TEST_MAC_POOL.lock().unwrap();
        MachineInfo::Dpu(DpuMachineInfo::new(
            HardwareType::NvidiaDgxVr,
            &mut mac_pool,
            settings,
        ))
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-dpu-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn hpe_proliant_dl380a_gen11_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::HpeProliantDl380aGen11),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

pub async fn generic_ami_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HardwareType::GenericAmi),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    ))
    .await
}

/// Builds a router from the recorded Lenovo ThinkSystem SR670 Redfish tree.
pub fn lenovo_thinksystem_sr670_router() -> Router {
    let archive_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("lenovo_thinksystem_sr670.tar.gz");
    crate::tar_router::tar_router(crate::tar_router::TarGzOption::Disk(&archive_path), None)
        .expect("Lenovo ThinkSystem SR670 archive must be readable")
}

/// Builds the recorded Lenovo XCC router with five usable System interfaces
/// and the ConnectX-7 MAC reported only through its linked adapter Port.
pub fn lenovo_xcc_router_with_partial_system_network_inventory() -> Router {
    const SYSTEM_INTERFACES: &str = "/redfish/v1/Systems/1/EthernetInterfaces";
    const ADAPTERS: &str = "/redfish/v1/Chassis/1/NetworkAdapters";
    const ADAPTER: &str = "/redfish/v1/Chassis/1/NetworkAdapters/slot-15";
    const PORTS: &str = "/redfish/v1/Chassis/1/NetworkAdapters/slot-15/Ports";
    const PORT: &str = "/redfish/v1/Chassis/1/NetworkAdapters/slot-15/Ports/2";

    let interfaces = [
        ("ToManager", "0a:8f:c3:a5:8a:41"),
        ("NIC1", "00:62:0b:4c:28:a8"),
        ("NIC2", "00:62:0b:4c:28:a9"),
        ("NIC3", "00:62:0b:4c:28:aa"),
        ("NIC4", "00:62:0b:4c:28:ab"),
    ];
    let interface_members = interfaces
        .iter()
        .map(|(id, _)| serde_json::json!({ "@odata.id": format!("{SYSTEM_INTERFACES}/{id}") }))
        .collect::<Vec<_>>();
    let mut router = Router::new().route(
        SYSTEM_INTERFACES,
        get(move || {
            let interface_members = interface_members.clone();
            async move {
                Json(serde_json::json!({
                    "@odata.id": SYSTEM_INTERFACES,
                    "@odata.type": "#EthernetInterfaceCollection.EthernetInterfaceCollection",
                    "Name": "Ethernet Interface Collection",
                    "Members": interface_members,
                }))
            }
        }),
    );
    for (id, mac_address) in interfaces {
        let path = format!("{SYSTEM_INTERFACES}/{id}");
        let interface_odata_id = path.clone();
        router = router.route(
            &path,
            get(move || {
                let interface_odata_id = interface_odata_id.clone();
                async move {
                    Json(serde_json::json!({
                        "@odata.id": interface_odata_id,
                        "@odata.type": "#EthernetInterface.v1_12_0.EthernetInterface",
                        "Id": id,
                        "Name": id,
                        "InterfaceEnabled": true,
                        "MACAddress": mac_address,
                        "Status": { "State": "Enabled" },
                    }))
                }
            }),
        );
    }

    router
        .route(
            ADAPTERS,
            get(|| async {
                Json(serde_json::json!({
                    "@odata.id": ADAPTERS,
                    "@odata.type": "#NetworkAdapterCollection.NetworkAdapterCollection",
                    "Name": "Network Adapter Collection",
                    "Members": [{ "@odata.id": ADAPTER }],
                }))
            }),
        )
        .route(
            ADAPTER,
            get(|| async {
                Json(serde_json::json!({
                    "@odata.id": ADAPTER,
                    "@odata.type": "#NetworkAdapter.v1_7_0.NetworkAdapter",
                    "Id": "slot-15",
                    "Name": "ConnectX-7",
                    "Manufacturer": "NVIDIA",
                    "Model": "ConnectX-7",
                    "Ports": { "@odata.id": PORTS },
                }))
            }),
        )
        .route(
            PORTS,
            get(|| async {
                Json(serde_json::json!({
                    "@odata.id": PORTS,
                    "@odata.type": "#PortCollection.PortCollection",
                    "Name": "Port Collection",
                    "Members": [{ "@odata.id": PORT }],
                }))
            }),
        )
        .route(
            PORT,
            get(|| async {
                Json(serde_json::json!({
                    "@odata.id": PORT,
                    "@odata.type": "#Port.v1_6_0.Port",
                    "Id": "2",
                    "Name": "Port 2",
                    "Oem": {
                        "Lenovo": { "PhysicalPortMacAddress": "94:6d:ae:53:cb:9b" },
                    },
                }))
            }),
        )
        .fallback_service(lenovo_thinksystem_sr670_router())
}

const TEST_ADAPTERS: &str = "/redfish/v1/Chassis/Self/NetworkAdapters";
const TEST_ADAPTER: &str = "/redfish/v1/Chassis/Self/NetworkAdapters/1";
const TEST_PORTS: &str = "/redfish/v1/Chassis/Self/NetworkAdapters/1/Ports";
const TEST_SYSTEM_INTERFACES: &str = "/redfish/v1/Systems/Self/EthernetInterfaces";
const TEST_DISABLED_INTERFACE: &str = "/redfish/v1/Systems/Self/EthernetInterfaces/disabled";

/// Builds a generic host router with supplemental network adapter ports.
pub fn generic_ami_router_with_network_adapter_ports(
    ports: Vec<serde_json::Value>,
) -> (axum::Router, BmcState) {
    let (router, state) = machine_router(
        &host_info(HardwareType::GenericAmi),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
        MachineRouterOptions::default(),
    );
    state.injection.put(vec![Rule {
        id: RuleId::from("network-adapters-link"),
        selector: Selector::Path {
            method: Some("GET".to_string()),
            glob: "/redfish/v1/Chassis/Self".to_string(),
        },
        action: Action::JsonMerge(serde_json::json!({
            "NetworkAdapters": { "@odata.id": TEST_ADAPTERS }
        })),
        remaining: None,
    }]);

    let port_ids = ports
        .iter()
        .enumerate()
        .map(|(index, _)| format!("{TEST_PORTS}/{}", index + 1))
        .collect::<Vec<_>>();
    let collection_port_ids = port_ids.clone();
    let mut router = router
        .route(
            TEST_ADAPTERS,
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "@odata.id": TEST_ADAPTERS,
                    "@odata.type": "#NetworkAdapterCollection.NetworkAdapterCollection",
                    "Name": "Network Adapter Collection",
                    "Members": [{ "@odata.id": TEST_ADAPTER }]
                }))
            }),
        )
        .route(
            TEST_ADAPTER,
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "@odata.id": TEST_ADAPTER,
                    "@odata.type": "#NetworkAdapter.v1_7_0.NetworkAdapter",
                    "Id": "1",
                    "Name": "Network Adapter",
                    "Ports": { "@odata.id": TEST_PORTS }
                }))
            }),
        )
        .route(
            TEST_PORTS,
            axum::routing::get(move || {
                let port_ids = collection_port_ids.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "@odata.id": TEST_PORTS,
                        "@odata.type": "#PortCollection.PortCollection",
                        "Name": "Port Collection",
                        "Members": port_ids
                            .into_iter()
                            .map(|id| serde_json::json!({ "@odata.id": id }))
                            .collect::<Vec<_>>()
                    }))
                }
            }),
        );
    for (port_id, port) in port_ids.into_iter().zip(ports) {
        let port = Arc::new(port);
        router = router.route(
            &port_id,
            axum::routing::get(move || {
                let port = Arc::clone(&port);
                async move { axum::Json((*port).clone()) }
            }),
        );
    }

    (router, state)
}

/// Builds a generic host router with one supplemental network adapter port.
pub fn generic_ami_router_with_network_adapter_port(
    port: serde_json::Value,
) -> (axum::Router, BmcState) {
    generic_ami_router_with_network_adapter_ports(vec![port])
}

/// Adds a disabled System EthernetInterface containing an invalid MAC to the
/// adapter-port test router.
pub fn generic_ami_router_with_network_adapter_port_and_disabled_system_mac(
    port: serde_json::Value,
) -> (axum::Router, BmcState) {
    let (router, state) = generic_ami_router_with_network_adapter_port(port);
    state.injection.upsert(Rule {
        id: RuleId::from("disabled-system-interface"),
        selector: Selector::Path {
            method: Some("GET".to_string()),
            glob: TEST_SYSTEM_INTERFACES.to_string(),
        },
        action: Action::Replace(serde_json::json!({
            "@odata.id": TEST_SYSTEM_INTERFACES,
            "@odata.type": "#EthernetInterfaceCollection.EthernetInterfaceCollection",
            "Name": "Ethernet Interface Collection",
            "Members": [{ "@odata.id": TEST_DISABLED_INTERFACE }]
        })),
        remaining: None,
    });
    let router = router.route(
        TEST_DISABLED_INTERFACE,
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "@odata.id": TEST_DISABLED_INTERFACE,
                "@odata.type": "#EthernetInterface.v1_12_0.EthernetInterface",
                "Id": "disabled",
                "Name": "Disabled Ethernet Interface",
                "InterfaceEnabled": false,
                "MACAddress": "not-a-mac"
            }))
        }),
    );
    (router, state)
}

pub async fn generic_ami_bmc_with_network_adapter_port(port: serde_json::Value) -> TestBmcHandle {
    test_bmc(generic_ami_router_with_network_adapter_port(port)).await
}

pub async fn generic_ami_bmc_with_network_adapter_ports(
    ports: Vec<serde_json::Value>,
) -> TestBmcHandle {
    test_bmc(generic_ami_router_with_network_adapter_ports(ports)).await
}

#[cfg(test)]
mod test {

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::injection::{Action, InjectionStore, Rule, RuleId, Selector};
    use crate::test_support::axum_http_client::Error;
    use crate::test_support::host_info;

    #[tokio::test]
    async fn caller_provided_injection_store_is_active() {
        let injection = Arc::new(InjectionStore::new());
        injection.upsert(Rule {
            id: RuleId::from("unavailable"),
            selector: Selector::Path {
                method: Some("GET".to_string()),
                glob: "/redfish/v1".to_string(),
            },
            action: Action::Status(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
            remaining: None,
        });
        let (router, state) = crate::machine_router_with_injection_store(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "test-host-id".to_string(),
            false,
            injection.clone(),
            crate::MachineRouterOptions::default(),
        );

        assert!(Arc::ptr_eq(&injection, &state.injection));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// End-to-end BMC self-reset window (epic #3796 issue 4): the manager
    /// reset is acknowledged, then EVERY request gets 503 until the window
    /// expires, recovery is lazy, and the server's power is never touched.
    #[tokio::test]
    async fn manager_reset_takes_bmc_offline_then_recovers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        #[derive(Debug, Default)]
        struct PowerCommandCounter(AtomicUsize);
        impl Callbacks for PowerCommandCounter {
            fn get_power_state(&self) -> MockPowerState {
                MockPowerState::On
            }
            fn send_power_command(
                &self,
                _reset_type: SystemPowerControl,
            ) -> Result<(), SetSystemPowerError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            fn state_refresh_indication(&self) {}
        }

        let callbacks = Arc::new(PowerCommandCounter::default());
        let (router, _state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            callbacks.clone(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::from_millis(100)),
                ..Default::default()
            },
        );

        let get = |path: &str| {
            router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        };

        // discover the manager's reset action target
        let response = get("/redfish/v1/Managers").await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let managers: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let manager_path = managers["Members"][0]["@odata.id"]
            .as_str()
            .expect("at least one manager")
            .to_string();
        let reset_target = format!("{manager_path}/Actions/Manager.Reset");

        // trigger the self-reset: acknowledged before going dark
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&reset_target)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // inside the window: EVERYTHING answers 503
        for path in ["/redfish/v1", &manager_path, "/redfish/v1/Systems"] {
            let response = get(path).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path} should be offline during the reset window"
            );
        }

        // after the window: silently back online
        tokio::time::sleep(Duration::from_millis(150)).await;
        let response = get("/redfish/v1").await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // a BMC reset must not touch the server's power
        assert_eq!(callbacks.0.load(Ordering::SeqCst), 0);
    }

    /// Regression guard: a ZERO `bmc_reset_duration` (e.g. from a profile
    /// or override with a zero timing) must behave exactly like `None` —
    /// the pre-existing no-op reset, with no offline window at all.
    #[tokio::test]
    async fn zero_reset_duration_keeps_the_old_noop_behavior() {
        use std::time::Duration;

        let (router, state) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::ZERO),
                ..Default::default()
            },
        );
        assert!(state.availability.is_none(), "zero must disable entirely");

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1/Managers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let managers: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let manager_path = managers["Members"][0]["@odata.id"]
            .as_str()
            .expect("at least one manager");

        // reset is acknowledged and the BMC stays online — old behavior
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{manager_path}/Actions/Manager.Reset"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
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
    }

    #[tokio::test]
    async fn transport_supports_expand_query_through_mock_expander() {
        let client = AxumRouterHttpClient::new(
            machine_router(
                &host_info(HardwareType::DellPowerEdgeR750),
                Arc::new(NoopCallbacks),
                "test-host-id".to_string(),
                false,
                MachineRouterOptions::default(),
            )
            .0,
        );
        let url =
            Url::parse("https://bmc-mock.local/redfish/v1/Chassis?$expand=.($levels=1)").unwrap();

        let response: serde_json::Value = client
            .get(
                url,
                &BmcCredentials::new("root".to_string(), "password".to_string()),
                None,
                &axum::http::HeaderMap::new(),
            )
            .await
            .expect("expanded GET should succeed");

        let members = response
            .get("Members")
            .and_then(|m| m.as_array())
            .expect("expanded response should contain Members array");
        assert!(!members.is_empty(), "expanded Members must not be empty");
        assert!(
            members[0].get("@odata.id").is_some() && members[0].get("Name").is_some(),
            "expanded member should contain entity fields from expander router"
        );
    }

    #[tokio::test]
    async fn unroutable_request_returns_404_from_transport() {
        let client = AxumRouterHttpClient::new(Router::new());
        let url = Url::parse("https://bmc-mock.local/redfish/v1").unwrap();
        let err = client
            .get::<serde_json::Value>(
                url,
                &BmcCredentials::new("root".to_string(), "password".to_string()),
                None,
                &axum::http::HeaderMap::new(),
            )
            .await
            .expect_err("empty router should return transport error");

        match err {
            Error::InvalidResponse { status, .. } => {
                assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            }
            other => panic!("expected invalid response error, got: {other}"),
        }
    }
}
