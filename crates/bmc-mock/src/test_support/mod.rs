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

use axum::Json;
use axum::routing::get;
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
    BmcState, Callbacks, DpuMachineInfo, HostHardwareType, HostMachineInfo, MachineInfo,
    MockPowerState, SetSystemPowerError, SystemPowerControl, machine_router,
};

pub mod axum_http_client;

#[derive(Debug)]
pub struct NoopCallbacks;

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
    pub static ref TEST_HW_MAC_POOL_CONFIG: MacAddressPoolConfig =
        MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).unwrap();

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
    ))
    .await
}

fn host_info(hw_type: HostHardwareType) -> MachineInfo {
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
        &host_info(HostHardwareType::WiwynnGB200Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn lenovo_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::LenovoGB300Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn dgx_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::NvidiaDgxGb300),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn supermicro_gb300_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::SupermicroGb300Nvl),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn generic_supermicro_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::GenericSupermicro),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn liteon_powershelf_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::LiteOnPowerShelf),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn delta_powershelf_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::DeltaPowerShelf),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

/// Delta power shelf whose PSUs report the given per-bay on/off states under
/// `Oem.deltaenergysystems.Power`. Lets tests exercise off and mixed shelves
/// (the default [`delta_powershelf_bmc`] is an all-on six-bay shelf).
pub async fn delta_powershelf_bmc_with_psu_power(states: Vec<bool>) -> TestBmcHandle {
    let machine_info = match host_info(HostHardwareType::DeltaPowerShelf) {
        MachineInfo::Host(host) => MachineInfo::Host(host.with_delta_psu_power(states)),
        MachineInfo::Dpu(_) => unreachable!("Delta power shelf must be a host"),
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn nvidia_switch_nd5200_ld_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::NvidiaSwitchNd5200Ld),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

pub async fn dell_poweredge_r750_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::DellPowerEdgeR750),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

const TEST_DELL_STORAGE: &str = "/redfish/v1/Systems/System.Embedded.1/Storage";

/// Builds a Dell PowerEdge R750 whose Storage collection lists `storages`.
/// Every Storage and drive resource is served at its own `@odata.id`, so a
/// Storage can link a drive that is not served to model a partly failing walk.
pub async fn dell_poweredge_r750_bmc_with_storage(
    storages: Vec<serde_json::Value>,
    drives: Vec<serde_json::Value>,
) -> TestBmcHandle {
    let (router, state) = machine_router(
        &host_info(HostHardwareType::DellPowerEdgeR750),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    );
    state.injection.upsert(Rule {
        id: RuleId::from("storage-collection"),
        selector: Selector::Path {
            method: Some("GET".to_string()),
            glob: TEST_DELL_STORAGE.to_string(),
        },
        action: Action::Replace(serde_json::json!({
            "@odata.id": TEST_DELL_STORAGE,
            "@odata.type": "#StorageCollection.StorageCollection",
            "Name": "Storage Collection",
            "Members": storages
                .iter()
                .map(|storage| serde_json::json!({ "@odata.id": storage["@odata.id"] }))
                .collect::<Vec<_>>()
        })),
        remaining: None,
    });
    let router = storages
        .into_iter()
        .chain(drives)
        .fold(router, |router, resource| {
            let path = resource["@odata.id"].as_str().unwrap().to_string();
            let resource = Arc::new(resource);
            router.route(
                &path,
                get(move || {
                    let resource = Arc::clone(&resource);
                    async move { Json((*resource).clone()) }
                }),
            )
        });
    test_bmc((router, state)).await
}

pub async fn dell_poweredge_r750_bluefield3_bmc(settings: DpuSettings) -> TestBmcHandle {
    let machine_info = {
        let mut mac_pool = TEST_MAC_POOL.lock().unwrap();
        MachineInfo::Dpu(DpuMachineInfo::new(
            HostHardwareType::DellPowerEdgeR750,
            &mut mac_pool,
            settings,
        ))
    };
    test_bmc(machine_router(
        &machine_info,
        Arc::new(NoopCallbacks),
        "test-dpu-id".to_string(),
        false,
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
    ))
    .await
}

pub async fn generic_ami_bmc() -> TestBmcHandle {
    test_bmc(machine_router(
        &host_info(HostHardwareType::GenericAmi),
        Arc::new(NoopCallbacks),
        "test-host-id".to_string(),
        false,
    ))
    .await
}

#[cfg(test)]
mod test {

    use axum::Router;
    use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
    use url::Url;

    use super::*;
    use crate::test_support::axum_http_client::Error;
    use crate::test_support::host_info;

    #[tokio::test]
    async fn transport_supports_expand_query_through_mock_expander() {
        let client = AxumRouterHttpClient::new(
            machine_router(
                &host_info(HostHardwareType::DellPowerEdgeR750),
                Arc::new(NoopCallbacks),
                "test-host-id".to_string(),
                false,
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
