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

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::map_response;
use axum::response::Response;
use axum::routing::{patch, post};
use axum::{Json, Router};
use bmc_mock::injection::{Action, InjectionStore, Rule, RuleId, Selector, injection_router};
use bmc_mock::test_support::{lenovo_thinksystem_sr670_router, serve_https};
use carbide_redfish::libredfish::test_support::RedfishSim;
use carbide_redfish::libredfish::{
    RedfishAuth, RedfishClientCreationError, RedfishClientPool, new_pool_with_credential_ops,
};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter as _, Credentials,
};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_utils::HostPortPair;
use libredfish::Redfish;
use libredfish::model::service_root::RedfishVendor;
use model::machine::{MachineState, ManagedHostState};
use model::test_support::ManagedHostConfig;
use serde_json::{Value, json};

use crate::env::Env;

async fn as_json(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Unlocked Lenovo XCC recording resets.
fn lenovo_xcc(resets: Arc<Mutex<Vec<String>>>, injection: Arc<InjectionStore>) -> Router {
    for (path, unlocked) in [
        (
            "/redfish/v1/Managers/1",
            json!({"Oem": {"Lenovo": {"KCSEnabled": true}}}),
        ),
        (
            "/redfish/v1/Managers/1/Oem/Lenovo/Security",
            json!({"Configurator": {"FWRollback": "Enabled"}}),
        ),
        (
            "/redfish/v1/Managers/1/EthernetInterfaces/ToHost",
            json!({"InterfaceEnabled": true}),
        ),
        (
            "/redfish/v1/Systems/1",
            json!({"Oem": {"Lenovo": {"FrontPanelUSB": {"FPMode": "Shared"}}}}),
        ),
    ] {
        injection.upsert(Rule {
            id: RuleId::from(path),
            selector: Selector::Path {
                method: None,
                glob: path.to_string(),
            },
            action: Action::JsonMerge(unlocked),
            remaining: None,
        });
    }
    let accept = || async { Json(json!({})) };
    let router = Router::new()
        .route("/redfish/v1/Systems/1/Bios/Pending", patch(accept))
        .route("/redfish/v1/Systems/1/Pending", patch(accept))
        .route(
            "/redfish/v1/Systems/1/Actions/ComputerSystem.Reset",
            post(move |Json(body): Json<Value>| async move {
                resets
                    .lock()
                    .unwrap()
                    .push(body["ResetType"].as_str().unwrap_or_default().to_string());
                StatusCode::NO_CONTENT
            }),
        )
        .fallback_service(lenovo_thinksystem_sr670_router().layer(map_response(as_json)));
    injection_router(router, injection)
}

/// Host BMC over real HTTP.
struct HostBmcPool {
    host_bmc: Arc<OnceLock<String>>,
    http: Arc<dyn RedfishClientPool>,
    sim: Arc<RedfishSim>,
}

#[async_trait]
impl RedfishClientPool for HostBmcPool {
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        if self.host_bmc.get().is_some_and(|host_bmc| host_bmc == host) {
            self.http.create_client(host, port, auth, vendor).await
        } else {
            self.sim.create_client(host, port, auth, vendor).await
        }
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        self.sim.credential_reader()
    }
}

fn busy_boot_order_write(action: Action) -> Rule {
    Rule {
        id: RuleId::from("busy-boot-order-write"),
        selector: Selector::Path {
            method: Some("PATCH".to_string()),
            glob: "/redfish/v1/Systems/1/Pending".to_string(),
        },
        action,
        remaining: Some(1),
    }
}

fn configuring_bios() -> ManagedHostState {
    ManagedHostState::HostInit {
        machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 0 },
    }
}

#[test]
fn configure_bios_waits_for_a_busy_bmc_instead_of_restarting_the_host() {
    let mut args = sqlx::testing::TestArgs::new(concat!(
        module_path!(),
        "::configure_bios_waits_for_a_busy_bmc_instead_of_restarting_the_host"
    ));
    args.fixtures(Box::leak(Box::new(vec![])));
    let test_fn: fn(PgPool) -> _ = configure_bios_against_a_busy_bmc;
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || sqlx_testing::TestFn::run_test(test_fn, args))
        .unwrap()
        .join()
        .unwrap();
}

async fn configure_bios_against_a_busy_bmc(pool: PgPool) {
    let resets = Arc::new(Mutex::new(Vec::new()));
    let injection = Arc::new(InjectionStore::new());
    let (_bmc, url) = serve_https(
        "busy-lenovo-xcc",
        lenovo_xcc(resets.clone(), injection.clone()),
    );
    let bmc_address = Arc::new(ArcSwap::from_pointee(Some(HostPortPair::HostAndPort(
        url.host_str().unwrap().to_string(),
        url.port().unwrap(),
    ))));
    let host_bmc = Arc::new(OnceLock::new());

    let mut env = Env::builder(pool.clone())
        .with_controller_redfish_pool({
            let host_bmc = host_bmc.clone();
            move |sim, credentials| {
                let http = libredfish::RedfishClientPool::builder()
                    .danger_accept_invalid_certs()
                    .build()
                    .unwrap();
                Arc::new(HostBmcPool {
                    host_bmc,
                    http: new_pool_with_credential_ops(credentials, http, bmc_address).0,
                    sim,
                })
            }
        })
        .build()
        .await;

    let domain = env.test_harness.test_domain().await;
    let network = env.test_harness.network_controller();
    let underlay = network.create_underlay_segment(&domain).await;
    network.create_admin_segment(&domain).await;
    let explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&explorer, underlay)
        .with_config(ManagedHostConfig {
            vendor: Some(bmc_vendor::BMCVendor::Lenovo),
            ..ManagedHostConfig::default()
        })
        .build()
        .await
        .0;
    let host = mh.host.machine().await;
    host_bmc
        .set(host.status.bmc_info.ip.unwrap().to_string())
        .unwrap();
    env.test_harness
        .api()
        .credential_manager()
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: host.status.bmc_info.mac.unwrap(),
                },
            },
            &Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "password".to_string(),
            },
        )
        .await
        .unwrap();
    mh.advance_state(configuring_bios()).await;

    for busy in [
        Action::Latency {
            mean: Duration::from_secs(11),
            jitter: Duration::ZERO,
        },
        Action::Status(503),
    ] {
        injection.upsert(busy_boot_order_write(busy));
        env.run_single_iteration().await;
        assert_eq!(mh.host.machine().await.state.value, configuring_bios());
        assert_eq!(*resets.lock().unwrap(), Vec::<String>::new());
    }

    env.run_single_iteration().await;
    assert_eq!(
        mh.host.machine().await.state.value,
        ManagedHostState::HostInit {
            machine_state: MachineState::PollingBiosSetup { retry_count: 0 },
        }
    );
    assert_eq!(*resets.lock().unwrap(), ["ForceRestart"]);
}
