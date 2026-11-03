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

//! Ready force-delete/rebuild, then two decommission/rebuild cycles against Core and MAT.
//! DPF Kubernetes resource deletion is excluded: this harness has no injectable Kube backend.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use api_test_helper::api_server::TEST_BMC_DHCP_RELAY_ADDRESS;
use api_test_helper::machine_a_tron::{RmsMock, rms_service_router};
use api_test_helper::utils::{LOCALHOST_CERTS, TestApiServerArgs};
use api_test_helper::{IntegrationTestEnvironment, utils};
use bmc_mock::test_support::TEST_MAC_POOL;
use bmc_mock::{HardwareType, ListenerOrAddress, RackInfo, RackType};
use carbide_secrets::create_credential_manager;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_utils::HostPortPair;
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackId, RackProfileId};
use eyre::{ContextCompat, WrapErr};
use forge_tls::client_config::ClientCert;
use futures::FutureExt;
use mac_address::MacAddress;
use machine_a_tron::{
    DeviceHandle, DeviceKind, DeviceStatus, DeviceStatusConfig, MachineATronConfig, MachineConfig,
    SimulatorLifecycle,
};
use model::address_selection_strategy::AddressSelectionStrategy;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem};
use model::expected_rack::ExpectedRack;
use model::machine_interface::InterfaceType;
use model::power_shelf::{PowerShelfControllerState, PowerShelfDecommissioningState};
use model::rack::{RackMaintenanceState, RackState};
use model::site_explorer::EndpointExplorationError;
use model::switch::{SwitchControllerState, SwitchDecommissioningState};
use rpc::forge;
use rpc::forge_tls_client::{ForgeClientConfig, ForgeClientT, ForgeTlsClient};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

const RACK_ID: &str = "decommission-two-cycle-rack";
const PROFILE_ID: &str = "DECOMMISSION_1_1_1";
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(300);
const READY_STABILITY_WINDOW: Duration = Duration::from_secs(2);

#[ctor::ctor(unsafe)]
fn setup() {
    api_test_helper::setup_logging();
}

#[test]
fn preserved_hardware_decommission_and_rebuild_twice() -> eyre::Result<()> {
    utils::api_server_runtime_builder()
        .build()?
        .block_on(async { timeout(Duration::from_secs(2400), run()).await })?
}

async fn rpc<T>(
    future: impl Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> eyre::Result<T> {
    Ok(timeout(RPC_TIMEOUT, future).await??.into_inner())
}

async fn run() -> eyre::Result<()> {
    let Some(mut env) =
        IntegrationTestEnvironment::try_from_environment(1, "decommission_two_cycles").await?
    else {
        println!("SKIPPED decommission: REPO_ROOT or CONTAINER_REPO_ROOT is not set");
        return Ok(());
    };
    let evidence = std::env::var_os("DECOMMISSION_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("decommission-two-cycles-{}", std::process::id()))
        });
    std::fs::create_dir_all(&evidence)?;
    tracing::info!(path = %evidence.display(), "Saving decommission evidence");

    let firmware = temp_dir::TempDir::with_prefix("decommission-firmware")?;
    let registry = machine_a_tron::BmcMockRegistry::default();
    let mut resources = RunResources::default();
    let mut handles = BTreeMap::new();
    let result = AssertUnwindSafe(async {
        let tls = bmc_mock::tls::server_config(Some(env.root_dir.join("crates/bmc-mock")))?;
        resources.bmc = Some(bmc_mock::CombinedServer::run(
            "decommission-bmc",
            registry.clone(),
            Some(ListenerOrAddress::Listener(TcpListener::bind(
                "127.0.0.1:0",
            )?)),
            tls.clone(),
        ));
        let bmc_addr = resources.bmc.as_ref().unwrap().address;
        // Reserve the listener before configuring Core. RMS clients connect lazily;
        // MAT needs Core's desired-firmware RPC before it can create its inventory.
        let rms_listener = TcpListener::bind("127.0.0.1:0")?;
        let rms_addr = rms_listener.local_addr()?;
        resources.core = Some(
            utils::start_api_server_with_config(
                &mut env,
                TestApiServerArgs {
                    bmc_proxy: Some(HostPortPair::HostAndPort(
                        "127.0.0.1".into(),
                        bmc_addr.port(),
                    )),
                    firmware_directory: firmware.path().to_owned(),
                    addr_index: 0,
                    put_dev_bin_in_path: true,
                    insecure_discovery: true,
                },
                resources.cancel.clone(),
                core_config(rms_addr)?,
            )
            .await?,
        );
        let api_url = format!("https://{}", env.carbide_api_addrs[0]);
        let client_config = ForgeClientConfig::new(
            LOCALHOST_CERTS.ca_cert.to_string_lossy().into_owned(),
            Some(ClientCert {
                cert_path: LOCALHOST_CERTS.client_cert.to_string_lossy().into_owned(),
                key_path: LOCALHOST_CERTS.client_key.to_string_lossy().into_owned(),
            }),
        );
        let client = timeout(
            RPC_TIMEOUT,
            ForgeTlsClient::new(&client_config).build(&api_url),
        )
        .await??;
        // Fresh sites have no NVOS rotation target. Publish one before any switch
        // can reach Ready so both ingestion and rebuild must rotate its password.
        rpc(client
            .clone()
            .rotate_credential(forge::RotateCredentialRequest {
                credential_type: forge::RotationCredentialType::RotationNvos.into(),
                password: None,
                reason: Some("decommission integration fixture".into()),
            }))
        .await?;
        let mut txn = env.db_pool.begin().await?;
        db::expected_rack::create(
            &mut txn,
            &ExpectedRack {
                rack_id: RackId::new(RACK_ID),
                rack_profile_id: RackProfileId::new(PROFILE_ID),
                rack_group_id: None,
                metadata: Default::default(),
            },
        )
        .await?;
        txn.commit().await?;
        let (hosts, mat) = api_test_helper::machine_a_tron::run_local(
            mat_config(&api_url)?,
            vec![],
            &env.root_dir,
            registry.clone(),
            TEST_MAC_POOL.clone(),
        )
        .await?;
        resources.mat = Some(mat);
        assert_eq!(hosts.len(), 1);
        let mat = resources.mat.as_ref().unwrap();
        let rms_service = mat.rms_service(Default::default(), bmc_addr.port());
        resources.rms = Some(bmc_mock::CombinedServer::run_router(
            "decommission-rms",
            rms_service_router(rms_service.clone()),
            Some(ListenerOrAddress::Listener(rms_listener)),
            tls,
        ));
        handles = mat
            .registry()
            .devices()
            .iter()
            .map(|device| {
                let handle = device.handle();
                (handle.machine_config_section().to_owned(), handle.clone())
            })
            .collect();
        timeout(
            Duration::from_secs(1800),
            cycles(
                &env,
                &client,
                &handles,
                &rms_service,
                rms_addr,
                bmc_addr,
                &registry,
                &evidence,
            ),
        )
        .await
        .context("decommission cycles timed out")?
    })
    .catch_unwind()
    .await;
    if !matches!(&result, Ok(Ok(())))
        && let Some(bmc) = resources.bmc.as_ref()
    {
        // Diagnostics run after cycle timeout and before cleanup. Setup failures
        // can still record the resources that were acquired before the failure.
        match AssertUnwindSafe(timeout(
            RPC_TIMEOUT,
            save_failure_evidence(&env, &handles, bmc.address.port(), &registry, &evidence),
        ))
        .catch_unwind()
        .await
        {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => tracing::warn!(%error, "Could not save failure evidence"),
            Ok(Err(error)) => tracing::warn!(%error, "Failure evidence capture timed out"),
            Err(_) => tracing::warn!("Failure evidence capture panicked"),
        }
    }
    let cleanup = resources.cleanup(&env).await;
    match result {
        Ok(result) => result?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    cleanup
}

/// Own resources as soon as setup acquires them, including on an early error.
#[derive(Default)]
struct RunResources {
    bmc: Option<bmc_mock::CombinedServer>,
    rms: Option<bmc_mock::CombinedServer>,
    core: Option<utils::ApiServerHandle>,
    mat: Option<api_test_helper::machine_a_tron::MachineATronHandle>,
    cancel: CancellationToken,
}

impl Drop for RunResources {
    fn drop(&mut self) {
        // Also covers the outer setup/cleanup backstop.
        self.cancel.cancel();
    }
}

impl RunResources {
    async fn cleanup(&mut self, env: &IntegrationTestEnvironment) -> eyre::Result<()> {
        let mut errors = Vec::new();
        if let Some(mat) = self.mat.take() {
            cleanup_step("MAT", &mut errors, mat.shutdown_without_api_cleanup()).await;
        }
        self.cancel.cancel();
        if let Some(core) = self.core.take() {
            cleanup_step("Core", &mut errors, core.wait()).await;
        }
        if let Some(mut rms) = self.rms.take() {
            cleanup_step("RMS", &mut errors, async {
                rms.stop().await?;
                Ok(())
            })
            .await;
        }
        if let Some(mut bmc) = self.bmc.take() {
            cleanup_step("BMC", &mut errors, async {
                bmc.stop().await?;
                Ok(())
            })
            .await;
        }
        cleanup_step("database", &mut errors, async {
            env.db_pool.close().await;
            Ok(())
        })
        .await;
        eyre::ensure!(errors.is_empty(), "cleanup failed: {}", errors.join(", "));
        Ok(())
    }
}

async fn cleanup_step(
    resource: &str,
    errors: &mut Vec<String>,
    future: impl Future<Output = eyre::Result<()>>,
) {
    let failure = match AssertUnwindSafe(timeout(RPC_TIMEOUT, future))
        .catch_unwind()
        .await
    {
        Ok(Ok(Ok(()))) => return,
        Ok(Ok(Err(error))) => format!("{resource}: {error}"),
        Ok(Err(_)) => format!("{resource}: timed out"),
        Err(_) => format!("{resource}: panicked"),
    };
    tracing::warn!(%failure, "Cleanup step failed");
    errors.push(failure);
}

async fn save_failure_evidence(
    env: &IntegrationTestEnvironment,
    handles: &BTreeMap<String, DeviceHandle>,
    bmc_port: u16,
    registry: &machine_a_tron::BmcMockRegistry,
    evidence: &Path,
) -> eyre::Result<()> {
    let authorities: BTreeSet<_> = registry.read().await.keys().cloned().collect();
    let device_state = |name: &str, mac: mac_address::MacAddress, status: DeviceStatus| {
        json!({
            "configuration": name,
            "bmc_mac_address": mac,
            "authority": status.bmc.ip,
            "authority_registered": status.bmc.ip.as_ref().map(|ip| authorities.contains(ip)),
            "status": status,
        })
    };
    let mut devices = Vec::new();
    for (name, handle) in handles {
        let mut status = handle.status(&DeviceStatusConfig::new(bmc_port));
        for (dpu, info) in std::mem::take(&mut status.dpus)
            .into_iter()
            .zip(&handle.host_info().dpus)
        {
            devices.push(device_state(name, info.bmc_mac_address, dpu));
        }
        devices.push(device_state(
            name,
            handle.host_info().bmc_mac_address,
            status,
        ));
    }
    // Save MAT independently so a database failure still leaves routing evidence.
    std::fs::write(
        evidence.join("failure-mat.json"),
        serde_json::to_vec_pretty(&json!({"authorities": authorities, "devices": devices}))?,
    )?;

    let (interfaces, endpoints) = tokio::join!(
        async {
            let mut connection = env.db_pool.acquire().await?;
            let mut interfaces = db::machine_interface::find_all(&mut connection).await?;
            interfaces.sort_by_key(|interface| interface.mac_address);
            Ok::<_, eyre::Report>(serde_json::to_value(interfaces)?)
        },
        async {
            let mut connection = env.db_pool.acquire().await?;
            let mut endpoints = db::explored_endpoints::find_all(&mut *connection).await?;
            endpoints.sort_by_key(|endpoint| endpoint.address);
            let mut states = Vec::new();
            for endpoint in endpoints {
                let error = endpoint.report.last_exploration_error.as_ref();
                let mut error_state = serde_json::to_value(error)?;
                if let Some(fields) = error_state.as_object_mut() {
                    // Retain diagnostic fields without opaque HTTP response bodies.
                    fields.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "Type"
                                | "Details"
                                | "details"
                                | "Cause"
                                | "cause"
                                | "Key"
                                | "key"
                                | "ResponseCode"
                                | "ConsecutiveCount"
                        )
                    });
                }
                states.push(json!({
                    "address": endpoint.address,
                    "report_version": endpoint.report_version,
                    "preingestion_state": endpoint.preingestion_state,
                    "waiting_for_explorer_refresh": endpoint.waiting_for_explorer_refresh,
                    "exploration_requested": endpoint.exploration_requested,
                    "pause_ingestion_and_poweron": endpoint.pause_ingestion_and_poweron,
                    "pause_remediation": endpoint.pause_remediation,
                    "last_exploration_error": error_state,
                    "avoid_lockout_reason": matches!(error, Some(EndpointExplorationError::AvoidLockout))
                        .then_some("previous unauthorized exploration; the original failure is not retained in AvoidLockout"),
                }));
            }
            Ok::<_, eyre::Report>(json!(states))
        },
    );
    for (section, result) in [("interfaces", interfaces), ("endpoints", endpoints)] {
        let state = match result {
            Ok(state) => state,
            Err(error) => json!({"capture_error": error.to_string()}),
        };
        std::fs::write(
            evidence.join(format!("failure-{section}.json")),
            serde_json::to_vec_pretty(&state)?,
        )?;
    }
    Ok(())
}

fn core_config(rms_addr: SocketAddr) -> eyre::Result<toml::Table> {
    Ok(toml::from_str(&format!(
        r#"
        [rms]
        api_url = "https://{rms_addr}"
        enforce_tls = false
        [component_manager]
        nv_switch_backend = "rms"
        power_shelf_backend = "rms"
        compute_tray_backend = "rms"
        # RMS validates inherited profiles even with zero component counts.
        [rack_profiles.GB200_NVL72_WIWYNN.rack_capabilities.switch]
        vendor = "NVIDIA"
        [rack_profiles.GB200_NVL72_WIWYNN.rack_capabilities.power_shelf]
        vendor = "LiteOn"
        [rack_profiles.GB300_NVL72_LENOVO.rack_capabilities.switch]
        vendor = "NVIDIA"
        [rack_profiles.GB300_NVL72_LENOVO.rack_capabilities.power_shelf]
        vendor = "LiteOn"
        [rack_profiles.{PROFILE_ID}]
        product_family = "gb200"
        rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"
        [rack_profiles.{PROFILE_ID}.rack_capabilities.compute]
        count = 1
        vendor = "NVIDIA"
        name = "GB200"
        [rack_profiles.{PROFILE_ID}.rack_capabilities.switch]
        count = 1
        vendor = "NVIDIA"
        [rack_profiles.{PROFILE_ID}.rack_capabilities.power_shelf]
        count = 1
        vendor = "LiteOn"
        [firmware_global]
        autoupdate = false
        [machine_validation_config]
        enabled = false
        [rack_validation_config]
        enabled = false
        [machine_state_controller]
        power_down_wait = "2s"
        [switch_state_controller.controller]
        iteration_time = "100ms"
        processor_dispatch_interval = "50ms"
        [power_shelf_state_controller.controller]
        iteration_time = "100ms"
        processor_dispatch_interval = "50ms"
        [rack_state_controller.controller]
        iteration_time = "100ms"
        processor_dispatch_interval = "50ms"
        "#,
    ))?)
}

fn mat_config(api_url: &str) -> eyre::Result<MachineATronConfig> {
    let mut config: MachineATronConfig = toml::from_str(&format!(
        "carbide_api_url = {api_url:?}\nbmc_mock_port = 0\napi_refresh_interval = \"100ms\"\nscout_stream_reconnect_interval = \"1s\"\nbmc_dhcp_renewal_interval = \"1s\"\n[machines]"
    ))?;
    let rack = RackInfo {
        rack_type: RackType::WiwynnGb200Nvl72,
    };
    for (name, hw_type, position, dpus) in [
        ("host", HardwareType::WiwynnGB200Nvl, 11, 2),
        ("switch", HardwareType::NvidiaSwitchNd5200Ld, 19, 0),
        ("shelf", HardwareType::LiteOnPowerShelf, 6, 0),
    ] {
        let mut machine: MachineConfig = toml::from_str(&format!(
            r#"
            host_count = 1
            dpu_per_host_count = {dpus}
            bmc_dhcp_relay_address = "{TEST_BMC_DHCP_RELAY_ADDRESS}"
            underlay_dhcp_relay_address = "172.20.1.1"
            host_inband_dhcp_relay_address = "172.20.0.1"
            dpf_enabled = false
            scout_run_interval = "1s"
            discovery_retry_interval = "100ms"
            run_interval_working = "100ms"
            run_interval_idle = "100ms"
            network_status_run_interval = "1s"
            [timing_overrides.host]
            power_on_os_ready = "1s"
            power_off_force = "100ms"
            reboot = "1s"
            bmc_reset = "100ms"
            [timing_overrides.dpu]
            power_on_os_ready = "1s"
            power_off_force = "100ms"
            reboot = "1s"
            bmc_reset = "100ms"
            "#,
        ))?;
        machine.hw_type = hw_type;
        machine.rack_id = Some(RackId::new(RACK_ID));
        machine.rack_placement = Some(rack.placement(position));
        config.machines.insert(name.into(), Arc::new(machine));
    }
    Ok(config)
}

struct View {
    machines: Vec<forge::Machine>,
    switch: forge::Switch,
    shelf: forge::PowerShelf,
}

impl View {
    async fn read(
        env: &IntegrationTestEnvironment,
        client: &ForgeClientT,
        host: &DeviceHandle,
    ) -> eyre::Result<Option<Self>> {
        let Some(host_id) = host.observed_machine_id() else {
            return Ok(None);
        };
        let mut ids = vec![host_id];
        for dpu in host.dpus() {
            let Some(id) = dpu.observed_machine_id() else {
                return Ok(None);
            };
            ids.push(id);
        }
        let machines = rpc(client
            .clone()
            .find_machines_by_ids(forge::MachinesByIdsRequest {
                machine_ids: ids,
                include_history: true,
            }))
        .await?
        .machines;
        let switches = rpc(client.clone().find_switches(forge::SwitchQuery::default()))
            .await?
            .switches;
        let shelves = rpc(client
            .clone()
            .find_power_shelves(forge::PowerShelfQuery::default()))
        .await?
        .power_shelves;
        if machines.len() != 3 || switches.len() != 1 || shelves.len() != 1 {
            return Ok(None);
        }
        let instance_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
            .fetch_one(&env.db_pool)
            .await?;
        eyre::ensure!(
            instance_count == 0,
            "unexpected instance allocated during decommission test"
        );
        Ok(Some(Self {
            machines,
            switch: switches
                .into_iter()
                .next()
                .context("switch missing from view")?,
            shelf: shelves
                .into_iter()
                .next()
                .context("shelf missing from view")?,
        }))
    }

    fn ready(&self) -> eyre::Result<bool> {
        Ok(self.machines.iter().all(|machine| machine.state == "Ready")
            && serde_json::from_str::<SwitchControllerState>(&self.switch.controller_state)?
                == SwitchControllerState::Ready
            && serde_json::from_str::<PowerShelfControllerState>(&self.shelf.controller_state)?
                == PowerShelfControllerState::Ready)
    }

    fn decommissioned(&self, host: MachineId) -> eyre::Result<bool> {
        Ok(self
            .machines
            .iter()
            .find(|machine| machine.id == Some(host))
            .context("host absent")?
            .state
            == "Decommissioning/Decommissioned"
            && matches!(
                serde_json::from_str::<SwitchControllerState>(&self.switch.controller_state)?,
                SwitchControllerState::Decommissioning {
                    decommissioning_state: SwitchDecommissioningState::Decommissioned
                }
            )
            && matches!(
                serde_json::from_str::<PowerShelfControllerState>(&self.shelf.controller_state)?,
                PowerShelfControllerState::Decommissioning {
                    decommissioning_state: PowerShelfDecommissioningState::Decommissioned
                }
            ))
    }
}

async fn wait_for_view(
    env: &IntegrationTestEnvironment,
    client: &ForgeClientT,
    host: &DeviceHandle,
    decommissioned: bool,
) -> eyre::Result<View> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let mut last = Value::Null;
    let mut stable_ready: Option<(Value, Instant)> = None;
    loop {
        if let Some(view) = View::read(env, client, host).await? {
            if decommissioned {
                if view.decommissioned(host.observed_machine_id().context("no host id")?)? {
                    return Ok(view);
                }
                last = json!({"machines": view.machines.iter().map(|machine| (&machine.id, &machine.state)).collect::<Vec<_>>(), "switch": view.switch.controller_state, "shelf": view.shelf.controller_state});
            } else {
                let (converged, fingerprint) = ready_fingerprint(client, host, &view).await?;
                last = fingerprint.clone();
                if converged {
                    match &stable_ready {
                        Some((previous, since)) if *previous == fingerprint => {
                            if since.elapsed() >= READY_STABILITY_WINDOW {
                                return Ok(view);
                            }
                        }
                        _ => stable_ready = Some((fingerprint, Instant::now())),
                    }
                } else {
                    stable_ready = None;
                }
            }
        } else {
            stable_ready = None;
        }
        eyre::ensure!(
            Instant::now() < deadline,
            "timed out waiting for decommissioned={decommissioned}; last states: {last}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

/// A provisional chassis/serial pick can be replaced by the first Scout PCI report
/// even after Ready. Wait for its final source and verified generation, then hold
/// all five state versions across multiple configured 100 ms controller periods.
async fn ready_fingerprint(
    client: &ForgeClientT,
    host: &DeviceHandle,
    view: &View,
) -> eyre::Result<(bool, Value)> {
    let boot =
        rpc(client
            .clone()
            .get_machine_boot_interfaces(forge::GetMachineBootInterfacesRequest {
                machine_id: host.observed_machine_id(),
            }))
        .await?;
    let primary: Vec<_> = boot
        .machine_interfaces
        .iter()
        .filter(|interface| interface.primary_interface)
        .collect();
    let converged = boot.reconciliation.as_ref().is_some_and(|status| {
        let finalized = matches!(
            status.selection_source(),
            forge::BootInterfaceSelectionSource::ExpectedMachine
                | forge::BootInterfaceSelectionSource::Operator
                | forge::BootInterfaceSelectionSource::RedfishUefiPci
                | forge::BootInterfaceSelectionSource::ScoutReportPci
        );
        let target_matches = status
            .desired_boot_interface
            .as_ref()
            .is_some_and(|desired| {
                primary.len() == 1
                    && primary[0].mac_address == desired.mac_address
                    && boot.effective_boot_interface_mac.as_ref() == Some(&desired.mac_address)
                    && desired.interface_id.as_ref().is_none_or(|id| {
                        primary[0].boot_interface_id.as_ref() == Some(id)
                            && boot.effective_boot_interface_id.as_ref() == Some(id)
                    })
            });
        finalized
            && target_matches
            && status.machine_state == "Ready"
            && !status.desired_version.is_empty()
            && status.verified_version.as_ref() == Some(&status.desired_version)
            && status.observed_at.is_some()
            && !status.is_compatibility_baseline
            && status.reconciling_version.is_none()
            && status.failure.is_none()
    });
    let machine_versions: BTreeMap<_, _> = view
        .machines
        .iter()
        .map(|machine| {
            Ok((
                machine.id.context("ready machine has no id")?.to_string(),
                (&machine.state, &machine.state_version),
            ))
        })
        .collect::<eyre::Result<_>>()?;
    Ok((
        converged && view.ready()?,
        json!({
            "machines": machine_versions,
            "switch": {"id": view.switch.id, "state": view.switch.controller_state, "version": view.switch.state_version},
            "shelf": {"id": view.shelf.id, "state": view.shelf.controller_state, "version": view.shelf.state_version},
            "boot": boot.reconciliation.as_ref().map(|status| json!({
                "desired_boot_interface": status.desired_boot_interface,
                "desired_version": status.desired_version,
                "verified_version": status.verified_version,
                "selection_source": status.selection_source,
                "machine_state": status.machine_state,
                "observation_present": status.observed_at.is_some(),
                "is_compatibility_baseline": status.is_compatibility_baseline,
                "reconciling_version": status.reconciling_version,
                "failure": status.failure,
            })),
            "effective_boot_mac": boot.effective_boot_interface_mac,
            "effective_boot_id": boot.effective_boot_interface_id,
            "primary_count": primary.len(),
        }),
    ))
}

/// Run the native rack workflow after ingestion, including after device-row rebuilds.
/// Device Ready does not imply rack maintenance has configured the fabric. Scope
/// this request to ConfigureNmxCluster so no firmware or power activity is injected.
async fn configure_fabric(
    env: &IntegrationTestEnvironment,
    client: &ForgeClientT,
) -> eyre::Result<Value> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let initial_version = loop {
        let (state, version) = rack_state(client).await?;
        let pending: bool = sqlx::query_scalar(
            "SELECT config->'maintenance_requested' IS NOT NULL
                AND config->'maintenance_requested' != 'null'::jsonb FROM racks WHERE id = $1",
        )
        .bind(RACK_ID)
        .fetch_one(&env.db_pool)
        .await?;
        if matches!(state, RackState::Ready | RackState::Error { .. }) && !pending {
            break version;
        }
        eyre::ensure!(
            Instant::now() < deadline,
            "timed out waiting to configure rack fabric; state: {state:?}, pending: {pending}"
        );
        sleep(Duration::from_millis(200)).await;
    };
    let history_cutoff: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(id), 0) FROM rack_state_history WHERE object_id = $1",
    )
    .bind(RACK_ID)
    .fetch_one(&env.db_pool)
    .await?;
    rpc(client
        .clone()
        .on_demand_rack_maintenance(forge::RackMaintenanceOnDemandRequest {
            rack_id: Some(RackId::new(RACK_ID)),
            scope: Some(forge::RackMaintenanceScope {
                activities: vec![forge::MaintenanceActivityConfig {
                    activity: Some(
                        forge::maintenance_activity_config::Activity::ConfigureNmxCluster(
                            forge::ConfigureNmxClusterActivity {},
                        ),
                    ),
                }],
                ..Default::default()
            }),
        }))
    .await?;
    loop {
        let (state, version) = rack_state(client).await?;
        eyre::ensure!(
            version == initial_version || !matches!(state, RackState::Error { .. }),
            "rack fabric maintenance failed: {state:?}"
        );
        if state == RackState::Ready && version != initial_version {
            let history: Vec<Value> = sqlx::query_scalar(
                "SELECT state FROM rack_state_history WHERE object_id = $1 AND id > $2 ORDER BY id",
            )
            .bind(RACK_ID)
            .bind(history_cutoff)
            .fetch_all(&env.db_pool)
            .await?;
            let states = history
                .iter()
                .cloned()
                .map(serde_json::from_value::<RackState>)
                .collect::<Result<Vec<_>, _>>()?;
            let configured = states.iter().any(|state| {
                matches!(
                    state,
                    RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::ConfigureNmxCluster { .. }
                    }
                )
            });
            let completed = states.iter().any(|state| {
                matches!(
                    state,
                    RackState::Maintenance {
                        maintenance_state: RackMaintenanceState::Completed
                    }
                )
            });
            if configured && completed {
                return Ok(
                    json!({"activity": "ConfigureNmxCluster", "history_cutoff": history_cutoff,
                    "initial_version": initial_version, "ready_version": version, "history": history}),
                );
            }
        }
        eyre::ensure!(
            Instant::now() < deadline,
            "timed out waiting for rack fabric maintenance; state: {state:?}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

async fn rack_state(client: &ForgeClientT) -> eyre::Result<(RackState, String)> {
    let racks = rpc(client.clone().find_racks_by_ids(forge::RacksByIdsRequest {
        rack_ids: vec![RackId::new(RACK_ID)],
    }))
    .await?;
    let rack = racks.racks.into_iter().next().context("rack is absent")?;
    let lifecycle = rack
        .status
        .context("rack status missing")?
        .lifecycle
        .context("rack lifecycle missing")?;
    Ok((serde_json::from_str(&lifecycle.state)?, lifecycle.version))
}

fn hardware_identity(handles: &BTreeMap<String, DeviceHandle>) -> Value {
    Value::Array(handles.iter().map(|(name, handle)| {
        let info = handle.host_info();
        json!({"name": name, "actor": handle.mat_id().to_string(), "serial": info.serial, "bmc_mac": info.bmc_mac_address.to_string(), "nvos_macs": info.nvos_mac_addresses.iter().map(ToString::to_string).collect::<Vec<_>>(), "dpus": handle.dpus().iter().zip(&info.dpus).map(|(dpu, info)| json!({"actor": dpu.mat_id().to_string(), "serial": info.serial, "bmc_mac": info.bmc_mac_address.to_string(), "oob_mac": info.oob_mac_address.to_string(), "host_mac": info.host_mac_address.to_string()})).collect::<Vec<_>>()})
    }).collect())
}

/// Stable identity read from Core, including BMC interfaces excluded by
/// machine-only interface lookups. Lease addresses and row IDs may change.
async fn core_identity(
    env: &IntegrationTestEnvironment,
    view: &View,
    handles: &BTreeMap<String, DeviceHandle>,
) -> eyre::Result<Value> {
    let mut serials = BTreeMap::new();
    let mut bmcs = BTreeMap::new();
    for machine in &view.machines {
        let id = machine
            .id
            .context("core machine identity missing")?
            .to_string();
        let serial = &machine
            .status
            .as_ref()
            .context("core machine status missing")?
            .discovery_info
            .as_ref()
            .context("core discovery missing")?
            .dmi_data
            .as_ref()
            .context("core DMI data missing")?
            .product_serial;
        eyre::ensure!(!serial.is_empty(), "core DMI serial is empty for {id}");
        serials.insert(id.clone(), serial.clone());
        bmcs.insert(id, core_bmc_mac(machine.bmc_info.as_ref())?);
    }
    bmcs.insert(
        view.switch
            .id
            .context("core switch ID missing")?
            .to_string(),
        core_bmc_mac(view.switch.bmc_info.as_ref())?,
    );
    bmcs.insert(
        view.shelf.id.context("core shelf ID missing")?.to_string(),
        core_bmc_mac(view.shelf.bmc_info.as_ref())?,
    );
    let expected: BTreeSet<_> = handles
        .values()
        .flat_map(|handle| {
            let info = handle.host_info();
            std::iter::once(info.bmc_mac_address)
                .chain(info.nvos_mac_addresses.iter().copied())
                .chain(
                    info.dpus
                        .iter()
                        .flat_map(|dpu| [dpu.bmc_mac_address, dpu.oob_mac_address]),
                )
                .map(|mac| mac.to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    let mut connection = env.db_pool.acquire().await?;
    let mut interfaces = BTreeMap::new();
    for row in db::machine_interface::find_all(&mut connection).await? {
        let mac = row.mac_address.to_string();
        if expected.contains(&mac) {
            let previous = interfaces.insert(mac.clone(), json!({
                "type": row.interface_type, "machine": row.machine_id,
                "dpu": row.attached_dpu_machine_id, "switch": row.switch_id, "shelf": row.power_shelf_id,
            }));
            eyre::ensure!(previous.is_none(), "duplicate core interface for {mac}");
        }
    }
    assert_eq!(
        interfaces.keys().cloned().collect::<BTreeSet<_>>(),
        expected,
        "Core BMC/OOB/NVOS MAC set changed"
    );
    Ok(json!({"serials": serials, "bmcs": bmcs, "interfaces": interfaces}))
}

fn core_bmc_mac(info: Option<&forge::BmcInfo>) -> eyre::Result<String> {
    Ok(info
        .and_then(|info| info.mac.as_deref())
        .context("core BMC MAC missing")?
        .parse::<MacAddress>()?
        .to_string())
}

async fn generations(env: &IntegrationTestEnvironment) -> eyre::Result<BTreeMap<String, String>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT 'machine:' || id, created::text FROM machines UNION ALL SELECT 'switch:' || id, created::text FROM switches UNION ALL SELECT 'shelf:' || id, created::text FROM power_shelves"
    ).fetch_all(&env.db_pool).await?.into_iter().collect())
}

async fn bmc_interface_generations(
    env: &IntegrationTestEnvironment,
    macs: &[String],
    admitted_at: &str,
) -> eyre::Result<BTreeMap<String, Value>> {
    Ok(sqlx::query_as::<_, (String, Value)>(
        "SELECT mac_address::text, jsonb_build_object(
             'id', id::text, 'created', created::text, 'last_dhcp', last_dhcp::text,
             'machine_id', machine_id,
             'created_after_admission', created >= $2::text::timestamptz,
             'dhcp_after_admission', last_dhcp >= $2::text::timestamptz)
         FROM machine_interfaces WHERE mac_address = ANY($1::text[]::macaddr[])",
    )
    .bind(macs)
    .bind(admitted_at)
    .fetch_all(&env.db_pool)
    .await?
    .into_iter()
    .collect())
}

/// Prepare can delete Ready rows without resetting the hardware. Only periodic
/// BMC DHCP can recreate these interfaces: the actors and their BMCs stay alive.
async fn ready_force_delete_rebuild(
    env: &IntegrationTestEnvironment,
    client: &ForgeClientT,
    handles: &BTreeMap<String, DeviceHandle>,
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    credentials: &dyn CredentialReader,
    evidence: &Path,
) -> eyre::Result<()> {
    let host = &handles["host"];
    let ready = wait_for_view(env, client, host, false).await?;
    let (_, before_ready) = ready_fingerprint(client, host, &ready).await?;
    let host_id = host.observed_machine_id().context("ready host has no id")?;
    let dpu_ids: BTreeSet<_> = host
        .dpus()
        .iter()
        .map(|dpu| {
            dpu.observed_machine_id()
                .context("ready DPU has no id")
                .map(|id| id.to_string())
        })
        .collect::<eyre::Result<_>>()?;
    assert_eq!(dpu_ids.len(), 2);
    let macs: Vec<_> = std::iter::once(host.host_info().bmc_mac_address.to_string())
        .chain(
            host.host_info()
                .dpus
                .iter()
                .map(|info| info.bmc_mac_address.to_string()),
        )
        .collect();
    let identity = hardware_identity(handles);
    let core_before = core_identity(env, &ready, handles).await?;
    let before_generations = generations(env).await?;
    let before_resets = bmc_reset_generations(handles)?;
    let before_aux = aux_power_generations(host)?;
    for endpoint in bmc_endpoint_states(handles, host, bmc_addr.port()) {
        assert_configured_bmc(http, bmc_addr, credentials, endpoint).await?;
    }
    let admitted_at: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
        .fetch_one(&env.db_pool)
        .await?;
    let before_interfaces = bmc_interface_generations(env, &macs, &admitted_at).await?;
    assert_eq!(before_interfaces.len(), 3);
    let old_interface_ids: Vec<_> = before_interfaces
        .values()
        .map(|interface| {
            interface["id"]
                .as_str()
                .context("BMC interface has no id")
                .map(ToOwned::to_owned)
        })
        .collect::<eyre::Result<_>>()?;
    let admission = View::read(env, client, host)
        .await?
        .context("ready host disappeared")?;
    let (converged, at_admission) = ready_fingerprint(client, host, &admission).await?;
    eyre::ensure!(
        converged && before_ready == at_admission,
        "ready/boot fingerprint changed before ready force-delete"
    );
    // One RPC deletes the host and its attached DPUs. ExpectedMachine and the
    // live sitewide BMC passwords survive; only the per-BMC Vault entries go.
    // Explorer must recover those entries using its sitewide credential fallback.
    let deletion =
        rpc(client
            .clone()
            .admin_force_delete_machine(forge::AdminForceDeleteMachineRequest {
                host_query: host_id.to_string(),
                delete_interfaces: true,
                delete_bmc_interfaces: true,
                delete_bmc_credentials: true,
                delete_bmc_suppressions: true,
                delete_retained_boot_interfaces: true,
                release_preserved_addresses: false,
                allow_delete_with_orphaned_dpf_crds: false,
                wait_for_instance_dpu: false,
            }))
        .await?;
    assert!(
        deletion.all_done,
        "Ready force-delete required continuation"
    );
    assert!(deletion.host_interfaces_deleted && deletion.dpu_interfaces_deleted);
    assert!(deletion.host_bmc_interface_deleted && deletion.dpu_bmc_interface_deleted);
    assert_eq!(
        deletion
            .dpu_machine_ids
            .into_iter()
            .collect::<BTreeSet<_>>(),
        dpu_ids
    );
    // Renewals may already have recreated rows before the RPC returns. Check
    // absence of the old generations rather than racing the next DHCP request.
    let old_machines: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM machines WHERE created::text = ($1::jsonb ->> ('machine:' || id))",
    )
    .bind(serde_json::to_value(&before_generations)?)
    .fetch_one(&env.db_pool)
    .await?;
    let old_interfaces: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM machine_interfaces WHERE id::text = ANY($1)")
            .bind(&old_interface_ids)
            .fetch_one(&env.db_pool)
            .await?;
    assert_eq!(
        old_machines, 0,
        "Ready force-delete retained old machine rows"
    );
    assert_eq!(
        old_interfaces, 0,
        "Ready force-delete retained old BMC interfaces"
    );
    let rebuilt = wait_for_view(env, client, host, false).await?;
    let (_, after_ready) = ready_fingerprint(client, host, &rebuilt).await?;
    assert_eq!(core_identity(env, &rebuilt, handles).await?, core_before);
    let after_generations = generations(env).await?;
    assert_eq!(
        after_generations.keys().collect::<Vec<_>>(),
        before_generations.keys().collect::<Vec<_>>()
    );
    for (id, created) in &after_generations {
        if id.starts_with("machine:") {
            assert_ne!(
                before_generations.get(id),
                Some(created),
                "machine was not recreated: {id}"
            );
        } else {
            assert_eq!(
                before_generations.get(id),
                Some(created),
                "unrequested device rebuild: {id}"
            );
        }
    }
    let after_interfaces = bmc_interface_generations(env, &macs, &admitted_at).await?;
    assert_eq!(after_interfaces.len(), 3);
    for (mac, interface) in &after_interfaces {
        assert_ne!(
            interface["id"], before_interfaces[mac]["id"],
            "BMC interface not recreated: {mac}"
        );
        assert_eq!(
            interface["machine_id"], before_interfaces[mac]["machine_id"],
            "BMC identity changed: {mac}"
        );
        assert_eq!(
            interface["created_after_admission"], true,
            "BMC interface predates deletion: {mac}"
        );
        assert_eq!(
            interface["dhcp_after_admission"], true,
            "BMC did not renew DHCP: {mac}"
        );
    }
    assert_eq!(hardware_identity(handles), identity);
    // Force-delete can reboot the host to unlock BIOS. Neither factory reset nor
    // the decommission-only Aux action may supply the missing DHCP discovery.
    assert_eq!(
        bmc_reset_generations(handles)?,
        before_resets,
        "Ready rebuild factory-reset a BMC"
    );
    assert_eq!(
        aux_power_generations(host)?,
        before_aux,
        "Ready rebuild applied Aux power"
    );
    for endpoint in bmc_endpoint_states(handles, host, bmc_addr.port()) {
        assert_configured_bmc(http, bmc_addr, credentials, endpoint).await?;
    }
    std::fs::write(
        evidence.join("ready-force-delete-rebuilt.json"),
        serde_json::to_vec_pretty(&json!({
            "hardware": identity,
            "admitted_at": admitted_at,
            "old_machine_generations_remaining": old_machines,
            "old_bmc_interfaces_remaining": old_interfaces,
            "before_ready": before_ready,
            "after_ready": after_ready,
            "before_generations": before_generations,
            "after_generations": after_generations,
            "before_bmc_interfaces": before_interfaces,
            "after_bmc_interfaces": after_interfaces,
            "bmc_reset_generations": before_resets,
            "aux_generations": before_aux,
        }))?,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cycles(
    env: &IntegrationTestEnvironment,
    client: &ForgeClientT,
    handles: &BTreeMap<String, DeviceHandle>,
    rms_service: &RmsMock,
    rms_addr: SocketAddr,
    bmc_addr: SocketAddr,
    registry: &machine_a_tron::BmcMockRegistry,
    evidence: &Path,
) -> eyre::Result<()> {
    assert_eq!(handles.len(), 3);
    let host = &handles["host"];
    let switch = &handles["switch"];
    assert_eq!(host.dpus().len(), 2);
    let frozen = hardware_identity(handles);
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .timeout(RPC_TIMEOUT)
        .build()?;
    let credentials = create_credential_manager(&env.credential_config).await?;
    wait_for_view(env, client, host, false).await?;
    let initial_fabric = configure_fabric(env, client).await?;
    std::fs::write(
        evidence.join("initial-fabric-maintenance.json"),
        serde_json::to_vec_pretty(&initial_fabric)?,
    )?;
    ready_force_delete_rebuild(
        env,
        client,
        handles,
        &http,
        bmc_addr,
        credentials.as_ref(),
        evidence,
    )
    .await?;
    let mut ready = wait_for_view(env, client, host, false).await?;
    let mut previous_generations = generations(env).await?;
    assert_eq!(previous_generations.len(), 5);
    let rms_client_config = librms::client_config::RmsClientConfig::new(None, None, None, false);
    let rms_url = format!("https://{rms_addr}");
    let rms_api = librms::RackManagerApi::new(&librms::client::RmsApiConfig::new(
        &rms_url,
        &rms_client_config,
    ));
    let mut nvos_jobs = BTreeSet::new();
    let mut bfb_tasks: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for cycle in 1..=2 {
        let (converged, before_admission) = ready_fingerprint(client, host, &ready).await?;
        eyre::ensure!(
            converged,
            "host boot selection changed before baseline capture"
        );
        let core_before = core_identity(env, &ready, handles).await?;
        let host_id = host.observed_machine_id().context("host has no id")?;
        let dpu_ids: BTreeSet<_> = host
            .dpus()
            .iter()
            .map(|dpu| {
                dpu.observed_machine_id()
                    .context("DPU has no machine id")
                    .map(|id| id.to_string())
            })
            .collect::<eyre::Result<_>>()?;
        let host_machine = ready
            .machines
            .iter()
            .find(|machine| machine.id == Some(host_id))
            .context("host missing from ready view")?;
        let associated: BTreeSet<_> = host_machine
            .status
            .as_ref()
            .context("host status missing")?
            .associated_dpu_machine_ids
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(associated, dpu_ids);
        assert_eq!(
            host_machine.rack_id.as_ref().map(|id| id.as_str()),
            Some(RACK_ID)
        );
        assert_eq!(
            ready.switch.rack_id.as_ref().map(|id| id.as_str()),
            Some(RACK_ID)
        );
        assert_eq!(
            ready.shelf.rack_id.as_ref().map(|id| id.as_str()),
            Some(RACK_ID)
        );
        for machine in ready
            .machines
            .iter()
            .filter(|machine| machine.id != Some(host_id))
        {
            assert_eq!(
                machine
                    .status
                    .as_ref()
                    .context("DPU status missing")?
                    .associated_host_machine_id
                    .as_ref()
                    .map(ToString::to_string),
                Some(host_id.to_string())
            );
        }

        let bmc_endpoints: Vec<_> = bmc_endpoint_states(handles, host, bmc_addr.port()).collect();
        for endpoint in &bmc_endpoints {
            assert_configured_bmc(&http, bmc_addr, credentials.as_ref(), *endpoint).await?;
        }
        let switch_node_id = ready.switch.id.context("switch has no id")?.to_string();
        let fabric_before = rms_service.switch_fabric_snapshot(RACK_ID, &switch_node_id);
        assert!(
            fabric_before.is_primary,
            "ready switch was not configured as fabric primary"
        );
        let reset_generations_before = bmc_reset_generations(handles)?;
        assert_eq!(reset_generations_before.len(), 5);
        assert_dpu_images(host, "dpu_agent")?;
        let os_before = dpu_os_generations(host, bmc_mock::DpuOsImage::Managed)?;
        let aux_before = aux_power_generations(host)?;
        let switch_reboot_before = switch
            .switch_reboot_generation()
            .context("switch reboot counter unavailable")?;
        let bios_before =
            host_bios_evidence(&http, bmc_addr, host, credentials.as_ref(), false).await?;
        assert_eq!(bios_before["embedded_uefi_shell"], "Disabled");
        assert_eq!(bios_before["uefi_password_unset"], false);
        let boot_before = dpu_boot_evidence(
            &http,
            bmc_addr,
            host,
            credentials.as_ref(),
            false,
            "OSRunning",
        )
        .await?;
        let host_history_cutoff: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(id), 0) FROM machine_state_history WHERE object_id = $1",
        )
        .bind(host_id.to_string())
        .fetch_one(&env.db_pool)
        .await?;
        let old_ip = host.bmc_ip().context("host has no BMC IP")?;
        let nvos_ip = switch.host_ip().context("switch has no NVOS IP")?;
        let nvos_key = CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: switch.host_info().bmc_mac_address,
        };
        let managed_nvos = credentials
            .get_credentials(&nvos_key)
            .await?
            .context("managed NVOS credential missing")?;
        let Credentials::UsernamePassword { username, password } = &managed_nvos;
        eyre::ensure!(
            password != bmc_mock::DUMMY_FACTORY_PASSWORD,
            "NVOS password was not rotated"
        );
        assert_eq!(
            nvos_status(&http, bmc_addr, nvos_ip, username, password).await?,
            reqwest::StatusCode::OK
        );
        let switch_id = ready.switch.id.context("switch has no id")?;
        let switch_history_cutoff: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(id), 0) FROM switch_state_history WHERE object_id = $1",
        )
        .bind(switch_id.to_string())
        .fetch_one(&env.db_pool)
        .await?;
        let admission = View::read(env, client, host)
            .await?
            .context("ready cohort disappeared before admission")?;
        let (converged, at_admission) = ready_fingerprint(client, host, &admission).await?;
        std::fs::write(
            evidence.join(format!("cycle-{cycle}-ready-admission.json")),
            serde_json::to_vec_pretty(&json!({
                "minimum_stable_ready_ms": READY_STABILITY_WINDOW.as_millis(),
                "before_baseline": before_admission,
                "at_admission": at_admission,
            }))?,
        )?;
        eyre::ensure!(
            converged && before_admission == at_admission,
            "ready/boot fingerprint changed during baseline capture"
        );
        // Observe each BFB boot while Core is still deconfiguring DPUs. Later
        // OOB suppression prevents PXE completion after the Aux power cycle.
        // Poll observers concurrently with admission so the other component RPCs
        // cannot delay observation of the short-lived pre-suppression boot.
        let ((boot_pre_ingestion, os_pre_ingestion), aux_power, terminal, ()) = tokio::try_join!(
            observe_pre_ingestion_boots(&http, bmc_addr, host, credentials.as_ref(), &os_before),
            observe_aux_power_cycle(client, host, bmc_addr.port(), &aux_before),
            wait_for_view(env, client, host, true),
            async {
                rpc(client.clone().decommission_managed_host(
                    forge::DecommissionManagedHostRequest {
                        machine_id: Some(host_id.to_string().parse()?),
                    },
                ))
                .await?;
                rpc(client
                    .clone()
                    .decommission_switch(forge::DecommissionSwitchRequest {
                        switch_id: ready.switch.id,
                    }))
                .await?;
                rpc(client.clone().decommission_power_shelf(
                    forge::DecommissionPowerShelfRequest {
                        power_shelf_id: ready.shelf.id,
                    },
                ))
                .await?;
                Ok::<(), eyre::Report>(())
            },
        )?;
        assert_new_boot(
            &boot_before,
            &os_before,
            &boot_pre_ingestion,
            &os_pre_ingestion,
        );
        assert_new_dpu_os_generation(&os_before, &os_pre_ingestion);
        let aux_terminal = aux_power_generations(host)?;
        let switch_reboot_terminal = switch
            .switch_reboot_generation()
            .context("switch reboot counter unavailable")?;
        assert_eq!(
            switch_reboot_terminal,
            switch_reboot_before + 1,
            "switch did not apply exactly one reboot"
        );
        assert_eq!(aux_terminal.len(), 3);
        for (mac, generation) in &aux_before {
            assert_eq!(
                aux_terminal[mac],
                *generation + 1,
                "host or DPU did not apply exactly one Aux action for {mac}"
            );
        }
        let host_history: Vec<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object('state', state, 'version', state_version)
             FROM machine_state_history WHERE object_id = $1 AND id > $2 ORDER BY id",
        )
        .bind(host_id.to_string())
        .bind(host_history_cutoff)
        .fetch_all(&env.db_pool)
        .await?;
        for state in [
            "powercyclinghost",
            "poweringonhost",
            "waitingforoobdhcpacknowledgement",
        ] {
            assert!(
                host_history
                    .iter()
                    .any(|row| row["state"]["decommissioning_state"]["state"] == state),
                "missing host power transition {state}"
            );
        }
        assert!(
            host_history
                .iter()
                .any(|row| row["version"] == aux_power["state_version"]),
            "power observation was not tied to this controller history"
        );
        let bmc_macs: Vec<_> = handles
            .values()
            .map(|handle| handle.host_info().bmc_mac_address)
            .chain(
                host.host_info()
                    .dpus
                    .iter()
                    .map(|info| info.bmc_mac_address),
            )
            .collect();
        let all_macs: Vec<_> = bmc_macs
            .iter()
            .copied()
            .chain(
                host.host_info()
                    .dpus
                    .iter()
                    .map(|info| info.oob_mac_address),
            )
            .chain(switch.host_info().nvos_mac_addresses.iter().copied())
            .collect();
        let suppressions: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('mac', bmc_mac_address::text, 'subsystem', subsystem, 'requested_at', requested_at::text, 'acknowledged_at', acknowledged_at::text) FROM bmc_suppressions WHERE source = 'decommissioning' ORDER BY bmc_mac_address, subsystem")
            .fetch_all(&env.db_pool).await?;
        // Preserve the complete query result even if an unexpected MAC fails the invariant.
        std::fs::write(
            evidence.join(format!("cycle-{cycle}-suppressions.json")),
            serde_json::to_vec_pretty(&suppressions)?,
        )?;
        let parsed_suppressions = suppressions
            .iter()
            .map(|row| -> eyre::Result<(mac_address::MacAddress, &Value)> {
                Ok((
                    row["mac"]
                        .as_str()
                        .context("suppression row has no MAC address")?
                        .parse()?,
                    row,
                ))
            })
            .collect::<eyre::Result<Vec<_>>>()?;
        let suppressed_macs: BTreeSet<_> =
            parsed_suppressions.iter().map(|(mac, _)| *mac).collect();
        assert!(
            suppressed_macs.iter().all(|mac| all_macs.contains(mac)),
            "unexpected decommission suppression MACs: {suppressed_macs:?}; expected {all_macs:?}"
        );
        let suppressed_macs: Vec<_> = suppressed_macs.into_iter().collect();
        for mac in &bmc_macs {
            for subsystem in ["site_explorer", "dhcp"] {
                assert!(
                    parsed_suppressions
                        .iter()
                        .any(|(row_mac, row)| row_mac == mac
                            && row["subsystem"] == subsystem
                            && !row["acknowledged_at"].is_null()),
                    "missing {subsystem} acknowledgement for {mac}"
                );
            }
            for credential_type in [
                BmcCredentialType::BmcRoot {
                    bmc_mac_address: *mac,
                },
                BmcCredentialType::BmcForgeAdmin {
                    bmc_mac_address: *mac,
                },
            ] {
                assert!(
                    credentials
                        .get_credentials(&CredentialKey::BmcCredentials { credential_type })
                        .await?
                        .is_none(),
                    "managed BMC credential survived decommission"
                );
            }
        }
        assert!(
            suppressions
                .iter()
                .all(|row| !row["acknowledged_at"].is_null())
        );
        for mac in &all_macs {
            assert!(
                parsed_suppressions
                    .iter()
                    .any(|(row_mac, row)| row_mac == mac
                        && row["subsystem"] == "dhcp"
                        && !row["acknowledged_at"].is_null()),
                "missing OOB/NVOS DHCP acknowledgement for {mac}"
            );
        }

        // NVOS loses its DHCP lease during the suppressed reboot, so inspect
        // the same live NVOS state exported by MAT persistence while it is offline.
        // Wire authentication below checks the rebuilt endpoint after DHCP resumes.
        let reset_nvos = switch
            .persisted()
            .nvos_admin
            .context("switch has no NVOS state")?;
        assert!(
            reset_nvos.username == bmc_mock::DUMMY_NVOS_USERNAME,
            "NVOS factory username was not restored"
        );
        assert!(
            reset_nvos.password == bmc_mock::DUMMY_FACTORY_PASSWORD,
            "NVOS factory password was not restored"
        );
        assert!(credentials.get_credentials(&nvos_key).await?.is_none());
        // Suppressed BMCs have abandoned their old authorities. Observe the same
        // live resource state offline, then verify real authentication after DHCP.
        let terminal_effects = bmc_effect_snapshots(handles)?;
        assert_eq!(terminal_effects.len(), 5);
        for (kind, mac, ip, _) in &bmc_endpoints {
            let ip = ip.context("BMC address missing")?;
            assert!(
                terminal_effects[&mac.to_string()].factory_accounts_restored,
                "BMC factory account was not restored: kind={kind}, ip={ip}, mac={mac}"
            );
            let Credentials::UsernamePassword { username, password } =
                factory_bmc_credentials(*kind);
            assert_eq!(
                endpoint_status(
                    &http,
                    bmc_addr,
                    ip,
                    "/redfish/v1/AccountService/Accounts",
                    &username,
                    &password
                )
                .await?,
                reqwest::StatusCode::NOT_FOUND,
                "suppressed BMC still serves its old authority: kind={kind}, ip={ip}, mac={mac}"
            );
        }

        let fabric_terminal = rms_service.switch_fabric_snapshot(RACK_ID, &switch_node_id);
        assert_eq!(
            fabric_terminal.reset_generation,
            fabric_before.reset_generation + 1
        );
        assert!(
            fabric_terminal.last_reset_cleared_primary,
            "NVOS reset did not remove the existing fabric role"
        );
        let reset_generations_terminal = bmc_reset_generations(handles)?;
        for (mac, generation) in &reset_generations_before {
            assert_eq!(
                reset_generations_terminal.get(mac),
                Some(&(*generation + 1)),
                "BMC factory reset must apply exactly once for {mac}"
            );
        }
        assert_dpu_images(host, "pre_ingestion_bfb")?;
        let os_terminal = dpu_os_generations(host, bmc_mock::DpuOsImage::PreIngestion)?;
        assert_eq!(
            os_terminal, os_pre_ingestion,
            "Aux reboot changed the installed BFB"
        );
        let host_effects = &terminal_effects[&host.host_info().bmc_mac_address.to_string()];
        let bios = host_effects
            .systems
            .values()
            .find(|system| system.bios_attributes.is_some())
            .context("host BIOS observation unavailable")?;
        let bios_terminal = json!({
            "embedded_uefi_shell": bios.bios_attributes.as_ref().unwrap()["EmbeddedUefiShell"],
            "uefi_password_unset": !bios.uefi_passwords_set,
        });
        assert_eq!(bios_terminal["embedded_uefi_shell"], "Enabled");
        assert_eq!(bios_terminal["uefi_password_unset"], true);
        let boot_terminal = offline_dpu_boot_evidence(host, &terminal_effects, "OSBootStarted")?;
        assert_new_boot(
            &boot_pre_ingestion,
            &os_pre_ingestion,
            &boot_terminal,
            &os_terminal,
        );
        let task_states: Vec<Value> = sqlx::query_scalar(
            "SELECT state #> '{decommissioning_state,dpu_states}' FROM machine_state_history
             WHERE object_id = $1 AND id > $2
               AND state #> '{decommissioning_state,dpu_states}' IS NOT NULL ORDER BY id",
        )
        .bind(host_id.to_string())
        .bind(host_history_cutoff)
        .fetch_all(&env.db_pool)
        .await?;
        for (dpu, info) in host.dpus().iter().zip(&host.host_info().dpus) {
            let id = dpu
                .observed_machine_id()
                .context("DPU has no machine id")?
                .to_string();
            let mut current_tasks: BTreeSet<String> = task_states
                .iter()
                .filter_map(|states| states[&id]["task_id"].as_str().map(ToOwned::to_owned))
                .collect();
            assert_eq!(current_tasks.len(), 1, "expected one BFB task for DPU {id}");
            let task_id = current_tasks
                .pop_first()
                .context("BFB task absent from controller history")?;
            assert!(
                bfb_tasks
                    .entry(info.bmc_mac_address.to_string())
                    .or_default()
                    .insert(task_id),
                "DPU BFB task reused across cycles"
            );
        }
        for (mac, tasks) in &bfb_tasks {
            for task in tasks {
                assert_eq!(
                    terminal_effects[mac].tasks.get(task).copied(),
                    Some(bmc_mock::BmcTaskState::Completed),
                    "offline BFB task did not complete: mac={mac}, task={task}"
                );
            }
        }
        assert_eq!(
            dpu_os_generations(host, bmc_mock::DpuOsImage::PreIngestion)?,
            os_terminal,
            "offline task observation changed a DPU installation"
        );
        let histories: Vec<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object('kind', 'machine', 'object_id', object_id,
                 'state', state, 'version', state_version, 'time', timestamp::text)
             FROM machine_state_history
             UNION ALL
             SELECT jsonb_build_object('kind', 'switch', 'object_id', object_id,
                 'state', state, 'version', state_version, 'time', timestamp::text)
             FROM switch_state_history
             UNION ALL
             SELECT jsonb_build_object('kind', 'shelf', 'object_id', object_id,
                 'state', state, 'version', state_version, 'time', timestamp::text)
             FROM power_shelf_state_history",
        )
        .fetch_all(&env.db_pool)
        .await?;
        // Switch identities and their histories survive deletion/re-ingestion.
        // Only entries after this cycle's pre-request cutoff identify its reset job.
        let mut new_jobs: BTreeSet<String> = sqlx::query_scalar(
            "SELECT state #>> '{decommissioning_state,job_id}'
             FROM switch_state_history
             WHERE object_id = $1 AND id > $2
               AND state #>> '{decommissioning_state,job_id}' IS NOT NULL
             ORDER BY id",
        )
        .bind(switch_id.to_string())
        .bind(switch_history_cutoff)
        .fetch_all(&env.db_pool)
        .await?
        .into_iter()
        .collect();
        assert_eq!(
            new_jobs.len(),
            1,
            "expected one NVOS reset job for this cycle"
        );
        let nvos_job = new_jobs
            .pop_first()
            .context("NVOS reset job missing from state history")?;
        assert!(
            nvos_jobs.insert(nvos_job.clone()),
            "NVOS reset reused a previous operation"
        );
        std::fs::write(
            evidence.join(format!("cycle-{cycle}-terminal.json")),
            serde_json::to_vec_pretty(&json!({
                "hardware": frozen,
                "histories": histories,
                "suppressions": suppressions,
                "switch_history_cutoff": switch_history_cutoff,
                "nvos_job_id": nvos_job,
                "host_history_cutoff": host_history_cutoff,
                "aux_power": aux_power,
                "aux_generations_before": aux_before,
                "aux_generations_terminal": aux_terminal,
                "switch_reboot_generation_before": switch_reboot_before,
                "switch_reboot_generation_terminal": switch_reboot_terminal,
                "bios": bios_terminal,
                "bmc_effects": terminal_effects,
                "bmc_reset_generations": reset_generations_terminal,
                "dpu_boot_pre_ingestion": boot_pre_ingestion,
                "dpu_boot": boot_terminal,
                "dpu_os_generations_before": os_before,
                "dpu_os_generations_pre_ingestion": os_pre_ingestion,
                "dpu_os_generations_terminal": os_terminal,
                "bfb_tasks": bfb_tasks,
                "fabric_reset": {
                    "is_primary": fabric_terminal.is_primary,
                    "reset_generation": fabric_terminal.reset_generation,
                    "last_reset_cleared_primary": fabric_terminal.last_reset_cleared_primary,
                },
            }))?,
        )?;
        let deletion =
            rpc(client
                .clone()
                .admin_force_delete_machine(forge::AdminForceDeleteMachineRequest {
                    host_query: host_id.to_string(),
                    delete_interfaces: true,
                    delete_bmc_interfaces: true,
                    delete_bmc_credentials: false,
                    delete_bmc_suppressions: cycle == 1,
                    delete_retained_boot_interfaces: true,
                    release_preserved_addresses: false,
                    allow_delete_with_orphaned_dpf_crds: false,
                    wait_for_instance_dpu: false,
                }))
            .await?;
        assert!(
            deletion.all_done,
            "instance-free deletion unexpectedly needs continuation"
        );
        assert!(deletion.host_interfaces_deleted && deletion.dpu_interfaces_deleted);
        assert!(deletion.host_bmc_interface_deleted && deletion.dpu_bmc_interface_deleted);
        assert_eq!(
            deletion
                .dpu_machine_ids
                .into_iter()
                .collect::<BTreeSet<_>>(),
            dpu_ids
        );
        rpc(client
            .clone()
            .admin_force_delete_switch(forge::AdminForceDeleteSwitchRequest {
                switch_id: terminal.switch.id,
                delete_interfaces: true,
                delete_bmc_suppressions: cycle == 1,
            }))
        .await?;
        rpc(client.clone().admin_force_delete_power_shelf(
            forge::AdminForceDeletePowerShelfRequest {
                power_shelf_id: terminal.shelf.id,
                delete_interfaces: true,
                delete_bmc_suppressions: cycle == 1,
            },
        ))
        .await?;
        if cycle == 2 {
            assert!(
                generations(env).await?.is_empty(),
                "force deletion left device rows"
            );
            // Force-delete needs an existing row: an absent host returns before
            // suppression cleanup, and an absent switch/shelf returns NotFound.
            // There is no standalone cleanup RPC; AllowIngestion changes only an
            // explored flag. Keep suppressions until allocation owns the old IP,
            // then use these normal DB helpers for this changed-address fixture.
            // Cycle one above independently proves native suppression cleanup.
            let mut txn = env.db_pool.begin().await?;
            let segment =
                db::network_segment::for_managed_static_address(&mut txn, old_ip.into(), None)
                    .await?;
            let placeholder_mac = mac_address::MacAddress::new([2, 0xff, 0x57, 0x50, 0, cycle]);
            db::machine_interface::create_with_type(
                &mut txn,
                &[segment],
                &placeholder_mac,
                true,
                AddressSelectionStrategy::StaticAddress(old_ip.into()),
                InterfaceType::Bmc,
                None,
            )
            .await?;
            for subsystem in [
                BmcSuppressionSubsystem::SiteExplorer,
                BmcSuppressionSubsystem::Dhcp,
            ] {
                db::bmc_suppression::delete_many_for_source(
                    &mut txn,
                    &suppressed_macs,
                    subsystem,
                    BmcSuppressionSource::Decommissioning,
                )
                .await?;
            }
            txn.commit().await?;
        }
        wait_for_view(env, client, host, false).await?;
        let fabric_maintenance = configure_fabric(env, client).await?;
        ready = wait_for_view(env, client, host, false).await?;
        let new_ip = host.bmc_ip().context("rebuilt host has no BMC IP")?;
        if cycle == 2 {
            assert_ne!(new_ip, old_ip, "Core reused the occupied BMC address");
            let routes = registry.read().await;
            assert!(
                routes.contains_key(&new_ip.to_string()),
                "new BMC address has no route"
            );
            assert!(
                !routes.contains_key(&old_ip.to_string()),
                "old BMC route survived rekey"
            );
            drop(routes);
            assert_eq!(
                endpoint_status(
                    &http,
                    bmc_addr,
                    old_ip,
                    "/redfish/v1/AccountService/Accounts",
                    bmc_mock::DUMMY_FACTORY_USERNAME,
                    bmc_mock::DUMMY_FACTORY_PASSWORD
                )
                .await?,
                reqwest::StatusCode::NOT_FOUND,
                "old BMC route still answers: kind={}, ip={old_ip}, mac={}",
                DeviceKind::Machine,
                host.host_info().bmc_mac_address
            );
            let new_key = CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: host.host_info().bmc_mac_address,
                },
            };
            let new_credential = credentials
                .get_credentials(&new_key)
                .await?
                .context("rebuilt host BMC credential missing")?;
            let Credentials::UsernamePassword { username, password } = new_credential;
            assert_eq!(
                endpoint_status(
                    &http,
                    bmc_addr,
                    new_ip,
                    "/redfish/v1/AccountService/Accounts",
                    &username,
                    &password
                )
                .await?,
                reqwest::StatusCode::OK,
                "rebuilt BMC account was not accepted: kind={}, ip={new_ip}, mac={}",
                DeviceKind::Machine,
                host.host_info().bmc_mac_address
            );
        }
        for endpoint in bmc_endpoint_states(handles, host, bmc_addr.port()) {
            assert_configured_bmc(&http, bmc_addr, credentials.as_ref(), endpoint).await?;
        }
        assert_eq!(hardware_identity(handles), frozen);
        assert_eq!(
            host.observed_machine_id(),
            Some(host_id),
            "host machine ID changed after rebuild"
        );
        let rebuilt_dpu_ids: BTreeSet<_> = host
            .dpus()
            .iter()
            .map(|dpu| {
                dpu.observed_machine_id()
                    .context("rebuilt DPU has no ID")
                    .map(|id| id.to_string())
            })
            .collect::<eyre::Result<_>>()?;
        assert_eq!(
            rebuilt_dpu_ids, dpu_ids,
            "DPU machine IDs changed after rebuild"
        );
        assert_eq!(
            core_identity(env, &ready, handles).await?,
            core_before,
            "Core identity changed after rebuild"
        );
        assert_eq!(
            aux_power_generations(host)?,
            aux_terminal,
            "an unrequested Aux action ran during rebuild"
        );
        assert_eq!(bmc_reset_generations(handles)?, reset_generations_terminal);
        let os_rebuilt = dpu_os_generations(host, bmc_mock::DpuOsImage::Managed)?;
        assert_new_dpu_os_generation(&os_terminal, &os_rebuilt);
        let bios_rebuilt =
            host_bios_evidence(&http, bmc_addr, host, credentials.as_ref(), false).await?;
        assert_eq!(bios_rebuilt["embedded_uefi_shell"], "Disabled");
        assert_eq!(bios_rebuilt["uefi_password_unset"], false);
        let boot_rebuilt = dpu_boot_evidence(
            &http,
            bmc_addr,
            host,
            credentials.as_ref(),
            false,
            "OSRunning",
        )
        .await?;
        assert_new_boot(&boot_terminal, &os_terminal, &boot_rebuilt, &os_rebuilt);
        assert_completed_bfb_tasks(
            &http,
            bmc_addr,
            host,
            credentials.as_ref(),
            false,
            &bfb_tasks,
        )
        .await?;
        assert_dpu_images(host, "dpu_agent")?;
        assert_eq!(
            dpu_os_generations(host, bmc_mock::DpuOsImage::Managed)?,
            os_rebuilt,
            "retained completed task polling replayed a DPU installation"
        );

        let new_generations = generations(env).await?;
        assert_eq!(
            new_generations.len(),
            5,
            "duplicate or missing device registrations"
        );
        assert_eq!(
            new_generations.keys().collect::<Vec<_>>(),
            previous_generations.keys().collect::<Vec<_>>(),
            "device generation key set changed"
        );
        for (id, created) in &new_generations {
            assert_ne!(
                previous_generations.get(id),
                Some(created),
                "device row was not recreated: {id}"
            );
        }
        previous_generations = new_generations;
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM bmc_suppressions WHERE source = 'decommissioning'",
        )
        .fetch_one(&env.db_pool)
        .await?;
        assert_eq!(remaining, 0);
        // Polling retained completed jobs must not reset the newly rotated NVOS again.
        for job_id in &nvos_jobs {
            let completed = timeout(
                RPC_TIMEOUT,
                rms_api
                    .client
                    .get_job_status(librms::protos::rack_manager::GetJobStatusRequest {
                        job_id: job_id.clone(),
                        include_child_job_states: true,
                    }),
            )
            .await??;
            assert!(!completed.job_states.is_empty());
            assert!(completed.job_states.iter().all(|job| job.execution_state
                == librms::protos::rack_manager::JobExecutionState::Completed as i32));
        }
        let fabric_rebuilt = rms_service.switch_fabric_snapshot(RACK_ID, &switch_node_id);
        assert!(
            fabric_rebuilt.is_primary,
            "rebuilt switch did not regain its fabric role"
        );
        assert_eq!(
            fabric_rebuilt.reset_generation,
            fabric_terminal.reset_generation
        );
        assert!(fabric_rebuilt.last_reset_cleared_primary);
        std::fs::write(
            evidence.join(format!("cycle-{cycle}-rebuilt.json")),
            serde_json::to_vec_pretty(&json!({
                "hardware": frozen,
                "bios": bios_rebuilt,
                "aux_generations_before": aux_before,
                "aux_generations_rebuilt": aux_terminal,
                "bmc_reset_generations": reset_generations_terminal,
                "dpu_boot": boot_rebuilt,
                "dpu_os_generations_before": os_before,
                "dpu_os_generations_pre_ingestion": os_pre_ingestion,
                "dpu_os_generations_terminal": os_terminal,
                "dpu_os_generations_rebuilt": os_rebuilt,
                "bfb_tasks": bfb_tasks,
                "fabric_maintenance": fabric_maintenance,
                "fabric_before": {
                    "is_primary": fabric_before.is_primary,
                    "reset_generation": fabric_before.reset_generation,
                },
                "fabric_rebuilt": {
                    "is_primary": fabric_rebuilt.is_primary,
                    "reset_generation": fabric_rebuilt.reset_generation,
                    "last_reset_cleared_primary": fabric_rebuilt.last_reset_cleared_primary,
                },
            }))?,
        )?;
        let rotated = credentials
            .get_credentials(&nvos_key)
            .await?
            .context("rebuilt switch has no NVOS credential")?;
        let Credentials::UsernamePassword { username, password } = rotated;
        assert_eq!(
            nvos_status(
                &http,
                bmc_addr,
                switch.host_ip().context("rebuilt NVOS has no IP")?,
                &username,
                &password
            )
            .await?,
            reqwest::StatusCode::OK
        );
        assert_eq!(
            nvos_status(
                &http,
                bmc_addr,
                switch.host_ip().context("rebuilt NVOS has no IP")?,
                bmc_mock::DUMMY_NVOS_USERNAME,
                bmc_mock::DUMMY_FACTORY_PASSWORD
            )
            .await?,
            reqwest::StatusCode::UNAUTHORIZED,
            "rebuilt NVOS still accepts its factory password"
        );
        let duplicate_interfaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT mac_address FROM machine_interfaces GROUP BY mac_address HAVING COUNT(*) > 1) duplicates").fetch_one(&env.db_pool).await?;
        assert_eq!(duplicate_interfaces, 0);
    }
    Ok(())
}

async fn nvos_status(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    nvos_ip: Ipv4Addr,
    username: &str,
    password: &str,
) -> eyre::Result<reqwest::StatusCode> {
    endpoint_status(
        http,
        bmc_addr,
        nvos_ip,
        "/nvue_v1/system",
        username,
        password,
    )
    .await
}

async fn endpoint_status(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    endpoint_ip: Ipv4Addr,
    path: &str,
    username: &str,
    password: &str,
) -> eyre::Result<reqwest::StatusCode> {
    Ok(http
        .get(format!("https://{bmc_addr}{path}"))
        .header("forwarded", format!("host={endpoint_ip}"))
        .basic_auth(username, Some(password))
        .send()
        .await?
        .status())
}

type BmcEndpoint = (DeviceKind, mac_address::MacAddress, Option<Ipv4Addr>, bool);

fn bmc_endpoint_states<'a>(
    handles: &'a BTreeMap<String, DeviceHandle>,
    host: &'a DeviceHandle,
    bmc_port: u16,
) -> impl Iterator<Item = BmcEndpoint> + 'a {
    handles
        .values()
        .map(move |handle| {
            (
                handle
                    .status(&DeviceStatusConfig::new(bmc_port))
                    .device_kind,
                handle.host_info().bmc_mac_address,
                handle.bmc_ip(),
                handle.host_info().hw_type == HardwareType::LiteOnPowerShelf,
            )
        })
        .chain(
            host.host_info()
                .dpus
                .iter()
                .zip(host.dpus())
                .map(|(info, dpu)| (DeviceKind::Dpu, info.bmc_mac_address, dpu.bmc_ip(), false)),
        )
}

async fn assert_configured_bmc(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    credentials: &dyn CredentialReader,
    (kind, mac, ip, retain_factory_credentials): BmcEndpoint,
) -> eyre::Result<()> {
    let ip = ip.context("BMC has no address")?;
    let Credentials::UsernamePassword {
        username: factory_username,
        password: factory_password,
    } = factory_bmc_credentials(kind);
    let key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac,
        },
    };
    let credential = credentials
        .get_credentials(&key)
        .await?
        .context("managed BMC credential missing")?;
    let Credentials::UsernamePassword { username, password } = credential;
    assert_eq!(
        endpoint_status(
            http,
            bmc_addr,
            ip,
            "/redfish/v1/AccountService/Accounts",
            &username,
            &password
        )
        .await?,
        reqwest::StatusCode::OK,
        "managed BMC account was not accepted: kind={kind}, ip={ip}, mac={mac}"
    );
    assert_eq!(
        endpoint_status(
            http,
            bmc_addr,
            ip,
            "/redfish/v1/AccountService/Accounts",
            &factory_username,
            &factory_password
        )
        .await?,
        if retain_factory_credentials {
            // MAT registers the shelf with bmc_retain_credentials=true.
            // Its reset needs the separate generation proof below.
            reqwest::StatusCode::OK
        } else {
            reqwest::StatusCode::UNAUTHORIZED
        },
        "unexpected factory BMC account state: kind={kind}, ip={ip}, mac={mac}"
    );
    Ok(())
}

fn bmc_effect_snapshots(
    handles: &BTreeMap<String, DeviceHandle>,
) -> eyre::Result<BTreeMap<String, bmc_mock::persistence::BmcEffectSnapshot>> {
    let mut snapshots = BTreeMap::new();
    for handle in handles.values() {
        let mac = handle.host_info().bmc_mac_address;
        snapshots.insert(
            mac.to_string(),
            handle
                .bmc_effect_snapshot()
                .ok_or_else(|| eyre::eyre!("BMC observation missing for {mac}"))?,
        );
        for (dpu, info) in handle.dpus().iter().zip(&handle.host_info().dpus) {
            let mac = info.bmc_mac_address;
            snapshots.insert(
                mac.to_string(),
                dpu.bmc_effect_snapshot()
                    .ok_or_else(|| eyre::eyre!("DPU BMC observation missing for {mac}"))?,
            );
        }
    }
    Ok(snapshots)
}

fn offline_dpu_boot_evidence(
    host: &DeviceHandle,
    snapshots: &BTreeMap<String, bmc_mock::persistence::BmcEffectSnapshot>,
    expected: &str,
) -> eyre::Result<BTreeMap<String, Value>> {
    let mut observations = BTreeMap::new();
    for info in &host.host_info().dpus {
        let mac = info.bmc_mac_address.to_string();
        let progress = snapshots[&mac]
            .systems
            .values()
            .find_map(|system| system.boot_progress.as_ref())
            .context("DPU boot observation missing")?;
        assert_eq!(
            progress["LastState"], expected,
            "unexpected offline DPU boot: mac={mac}"
        );
        observations.insert(mac, progress.clone());
    }
    Ok(observations)
}

fn bmc_reset_generations(
    handles: &BTreeMap<String, DeviceHandle>,
) -> eyre::Result<BTreeMap<String, u64>> {
    let mut generations = BTreeMap::new();
    for handle in handles.values() {
        let device = handle.persisted();
        let generation = device
            .bmc_state
            .context("BMC reset snapshot is unavailable")?
            .factory_reset_generation();
        generations.insert(device.bmc_mac_address.to_string(), generation);
        for dpu in device.dpus {
            let generation = dpu
                .bmc_state
                .context("DPU BMC reset snapshot is unavailable")?
                .factory_reset_generation();
            generations.insert(dpu.bmc_mac_address.to_string(), generation);
        }
    }
    Ok(generations)
}

fn assert_dpu_images(host: &DeviceHandle, expected: &str) -> eyre::Result<()> {
    let persisted = host.persisted();
    assert_eq!(persisted.dpus.len(), 2);
    for dpu in persisted.dpus {
        assert_eq!(
            serde_json::to_value(dpu.installed_os)?,
            expected,
            "unexpected installed OS for DPU {}",
            dpu.bmc_mac_address,
        );
    }
    Ok(())
}

/// Read live disk state without retaining the BMC or its installation callbacks.
/// Evidence contains only the existing generation, indexed by the frozen BMC MAC.
fn dpu_os_generations(
    host: &DeviceHandle,
    expected: bmc_mock::DpuOsImage,
) -> eyre::Result<BTreeMap<String, u64>> {
    let info = host.host_info();
    assert_eq!(host.dpus().len(), 2);
    assert_eq!(info.dpus.len(), 2);
    let mut generations = BTreeMap::new();
    for (dpu, info) in host.dpus().iter().zip(&info.dpus) {
        let status = dpu
            .dpu_os_status()
            .context("DPU disk status is unavailable")?;
        assert_eq!(
            status.image,
            Some(expected),
            "unexpected disk image for {}",
            info.bmc_mac_address
        );
        assert!(
            generations
                .insert(info.bmc_mac_address.to_string(), status.generation)
                .is_none()
        );
    }
    assert_eq!(generations.len(), 2);
    Ok(generations)
}

fn assert_new_dpu_os_generation(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) {
    assert_eq!(before.len(), 2);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (mac, generation) in before {
        assert!(
            after[mac] > *generation,
            "DPU {mac} did not install a new image"
        );
    }
}

fn factory_bmc_credentials(kind: DeviceKind) -> Credentials {
    let (username, password) = match kind {
        // The retained GB200 fixture has BF3 DPUs. Use the model's shared
        // default, as the DPU mock does, rather than the generic host password.
        DeviceKind::Dpu => bmc_vendor::DpuModel::BlueField3.default_factory_credentials(),
        DeviceKind::Machine | DeviceKind::Switch | DeviceKind::PowerShelf => (
            bmc_mock::DUMMY_FACTORY_USERNAME,
            bmc_mock::DUMMY_FACTORY_PASSWORD,
        ),
    };
    Credentials::new(username, password)
}

async fn probe_credentials(
    credentials: &dyn CredentialReader,
    mac: mac_address::MacAddress,
    kind: DeviceKind,
    factory: bool,
) -> eyre::Result<Credentials> {
    if factory {
        return Ok(factory_bmc_credentials(kind));
    }
    timeout(
        RPC_TIMEOUT,
        credentials.get_credentials(&CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: mac,
            },
        }),
    )
    .await??
    .context("managed BMC credential unavailable for effect probe")
}

async fn bmc_session_token(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    ip: Ipv4Addr,
    kind: DeviceKind,
    mac: mac_address::MacAddress,
    credentials: &Credentials,
) -> eyre::Result<String> {
    let Credentials::UsernamePassword { username, password } = credentials;
    let response = http
        .post(format!(
            "https://{bmc_addr}/redfish/v1/SessionService/Sessions"
        ))
        .header("forwarded", format!("host={ip}"))
        .json(&json!({"UserName": username, "Password": password}))
        .send()
        .await?
        .error_for_status()
        .wrap_err_with(|| format!("BMC session login failed: kind={kind}, ip={ip}, mac={mac}"))?;
    Ok(response
        .headers()
        .get("x-auth-token")
        .context("BMC session response has no authentication token")?
        .to_str()?
        .to_owned())
}

async fn endpoint_json(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    ip: Ipv4Addr,
    path: &str,
    token: &str,
) -> eyre::Result<Value> {
    Ok(http
        .get(format!("https://{bmc_addr}{path}"))
        .header("forwarded", format!("host={ip}"))
        .header("x-auth-token", token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn system_resources(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    ip: Ipv4Addr,
    token: &str,
) -> eyre::Result<Vec<Value>> {
    let collection = endpoint_json(http, bmc_addr, ip, "/redfish/v1/Systems", token).await?;
    let mut systems = Vec::new();
    for member in collection["Members"]
        .as_array()
        .context("systems collection has no members")?
    {
        let path = member["@odata.id"]
            .as_str()
            .context("system member has no URI")?;
        systems.push(endpoint_json(http, bmc_addr, ip, path, token).await?);
    }
    Ok(systems)
}

async fn host_bios_evidence(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    host: &DeviceHandle,
    reader: &dyn CredentialReader,
    factory: bool,
) -> eyre::Result<Value> {
    let ip = host.bmc_ip().context("host BMC address unavailable")?;
    let mac = host.host_info().bmc_mac_address;
    let kind = DeviceKind::Machine;
    let credentials = probe_credentials(reader, mac, kind, factory).await?;
    let token = bmc_session_token(http, bmc_addr, ip, kind, mac, &credentials).await?;
    let systems = system_resources(http, bmc_addr, ip, &token).await?;
    let path = systems
        .iter()
        .find_map(|system| system["Bios"]["@odata.id"].as_str())
        .context("host system has no BIOS resource")?;
    let bios = endpoint_json(http, bmc_addr, ip, path, &token).await?;
    let shell = bios["Attributes"]["EmbeddedUefiShell"]
        .as_str()
        .context("GB200 BIOS has no EmbeddedUefiShell attribute")?;
    // Empty -> empty is a no-op only when already clear; a set password rejects
    // the old empty value without changing it. Match pinned GB200 AdminPassword.
    let response = http
        .post(format!(
            "https://{bmc_addr}{path}/Actions/Bios.ChangePassword"
        ))
        .header("forwarded", format!("host={ip}"))
        .header("x-auth-token", &token)
        .json(&json!({"PasswordName": "AdminPassword", "OldPassword": "", "NewPassword": ""}))
        .send()
        .await?;
    let unset = match response.status() {
        reqwest::StatusCode::OK => true,
        reqwest::StatusCode::BAD_REQUEST => false,
        status => eyre::bail!("unexpected status {status} probing the UEFI password"),
    };
    Ok(json!({"embedded_uefi_shell": shell, "uefi_password_unset": unset}))
}

async fn dpu_boot_evidence(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    host: &DeviceHandle,
    reader: &dyn CredentialReader,
    factory: bool,
    expected: &str,
) -> eyre::Result<BTreeMap<String, Value>> {
    let mut observations = BTreeMap::new();
    for (dpu, info) in host.dpus().iter().zip(&host.host_info().dpus) {
        let kind = DeviceKind::Dpu;
        let mac = info.bmc_mac_address;
        let ip = dpu.bmc_ip().context("DPU BMC address unavailable")?;
        let credentials = probe_credentials(reader, mac, kind, factory).await?;
        let token = bmc_session_token(http, bmc_addr, ip, kind, mac, &credentials).await?;
        let systems = system_resources(http, bmc_addr, ip, &token).await?;
        let progress = systems
            .iter()
            .find_map(|system| system.get("BootProgress"))
            .context("DPU system has no BootProgress")?;
        assert_eq!(
            progress["LastState"], expected,
            "unexpected boot state: kind={kind}, ip={ip}, mac={mac}"
        );
        assert!(
            progress["LastStateTime"]
                .as_str()
                .is_some_and(|time| !time.is_empty()),
            "boot timestamp missing: kind={kind}, ip={ip}, mac={mac}"
        );
        observations.insert(info.bmc_mac_address.to_string(), progress.clone());
    }
    Ok(observations)
}

/// Capture the BFB's completed boot before Core suppresses OOB DHCP and cycles
/// power. Each DPU is observed independently because Core deconfigures them
/// sequentially; waiting for both to boot before reading can miss the first.
async fn observe_pre_ingestion_boots(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    host: &DeviceHandle,
    reader: &dyn CredentialReader,
    before: &BTreeMap<String, u64>,
) -> eyre::Result<(BTreeMap<String, Value>, BTreeMap<String, u64>)> {
    assert_eq!(host.dpus().len(), 2);
    let info = host.host_info();
    let observed = futures::future::try_join_all(host.dpus().iter().zip(&info.dpus).map(
        |(dpu, info)| async move {
            let mac = info.bmc_mac_address.to_string();
            let previous = *before
                .get(&mac)
                .context("DPU baseline generation missing")?;
            let credentials =
                probe_credentials(reader, info.bmc_mac_address, DeviceKind::Dpu, false).await?;
            let ip = dpu.bmc_ip().context("DPU BMC address unavailable")?;
            let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
            loop {
                let status = dpu
                    .dpu_os_status()
                    .context("DPU disk status is unavailable")?;
                if status.image == Some(bmc_mock::DpuOsImage::PreIngestion)
                    && status.generation > previous
                {
                    // The convergence wait can exceed the session's 120s lifetime.
                    let token = bmc_session_token(
                        http,
                        bmc_addr,
                        ip,
                        DeviceKind::Dpu,
                        info.bmc_mac_address,
                        &credentials,
                    )
                    .await?;
                    let systems = system_resources(http, bmc_addr, ip, &token).await?;
                    let progress = systems
                        .iter()
                        .find_map(|system| system.get("BootProgress"))
                        .context("DPU system has no BootProgress")?;
                    let current = dpu
                        .dpu_os_status()
                        .context("DPU disk status is unavailable")?;
                    if current == status
                        && progress["LastState"] == "OSRunning"
                        && progress["LastStateTime"]
                            .as_str()
                            .is_some_and(|time| !time.is_empty())
                    {
                        return Ok::<_, eyre::Report>((mac, progress.clone(), status.generation));
                    }
                }
                eyre::ensure!(
                    Instant::now() < deadline,
                    "timed out observing the pre-ingestion BFB boot for {mac}"
                );
                sleep(Duration::from_millis(25)).await;
            }
        },
    ))
    .await?;
    let mut progress = BTreeMap::new();
    let mut generations = BTreeMap::new();
    for (mac, boot, generation) in observed {
        assert!(progress.insert(mac.clone(), boot).is_none());
        assert!(generations.insert(mac, generation).is_none());
    }
    Ok((progress, generations))
}

fn assert_new_boot(
    before: &BTreeMap<String, Value>,
    before_generations: &BTreeMap<String, u64>,
    after: &BTreeMap<String, Value>,
    after_generations: &BTreeMap<String, u64>,
) {
    assert_eq!(before.len(), 2);
    let macs = before.keys().collect::<Vec<_>>();
    assert_eq!(macs, after.keys().collect::<Vec<_>>());
    assert_eq!(macs, before_generations.keys().collect::<Vec<_>>());
    assert_eq!(macs, after_generations.keys().collect::<Vec<_>>());
    for (mac, previous) in before {
        // Boot timestamps have second resolution; state and the current image's
        // install generation distinguish these transitions even within one second.
        assert_ne!(
            (&previous["LastState"], before_generations[mac]),
            (&after[mac]["LastState"], after_generations[mac]),
            "DPU {mac} did not progress to a new boot state or image generation"
        );
    }
}

async fn assert_completed_bfb_tasks(
    http: &reqwest::Client,
    bmc_addr: SocketAddr,
    host: &DeviceHandle,
    reader: &dyn CredentialReader,
    factory: bool,
    tasks: &BTreeMap<String, BTreeSet<String>>,
) -> eyre::Result<()> {
    assert_eq!(tasks.len(), 2);
    for (dpu, info) in host.dpus().iter().zip(&host.host_info().dpus) {
        let mac = info.bmc_mac_address.to_string();
        let credentials =
            probe_credentials(reader, info.bmc_mac_address, DeviceKind::Dpu, factory).await?;
        let ip = dpu.bmc_ip().context("DPU BMC address unavailable")?;
        let token = bmc_session_token(
            http,
            bmc_addr,
            ip,
            DeviceKind::Dpu,
            info.bmc_mac_address,
            &credentials,
        )
        .await?;
        for task in tasks.get(&mac).context("DPU BFB task not captured")? {
            let path = format!("/redfish/v1/TaskService/Tasks/{task}");
            let record = endpoint_json(http, bmc_addr, ip, &path, &token).await?;
            assert_eq!(
                record["Id"],
                task.as_str(),
                "unexpected task ID: kind=dpu, ip={ip}, mac={mac}"
            );
            assert_eq!(
                record["TaskState"], "Completed",
                "task did not complete: kind=dpu, ip={ip}, mac={mac}"
            );
        }
    }
    Ok(())
}

/// Applied Aux actions on the host and its two children. Ordinary ForceOff and
/// power-on actions do not change these counters; actor identities survive rebuild.
fn aux_power_generations(host: &DeviceHandle) -> eyre::Result<BTreeMap<String, u64>> {
    let info = host.host_info();
    assert_eq!(host.dpus().len(), 2);
    assert_eq!(info.dpus.len(), 2);
    let mut generations = BTreeMap::from([(
        info.bmc_mac_address.to_string(),
        host.aux_power_cycle_generation()
            .context("host aux generation is unavailable")?,
    )]);
    for (dpu, info) in host.dpus().iter().zip(&info.dpus) {
        assert!(
            generations
                .insert(
                    info.bmc_mac_address.to_string(),
                    dpu.aux_power_cycle_generation()
                )
                .is_none()
        );
    }
    assert_eq!(generations.len(), 3);
    Ok(generations)
}

async fn observe_aux_power_cycle(
    client: &ForgeClientT,
    host: &DeviceHandle,
    bmc_port: u16,
    before: &BTreeMap<String, u64>,
) -> eyre::Result<Value> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let id = host
        .observed_machine_id()
        .context("host identity unavailable for power observer")?;
    loop {
        let machines = rpc(client
            .clone()
            .find_machines_by_ids(forge::MachinesByIdsRequest {
                machine_ids: vec![id],
                include_history: false,
            }))
        .await?
        .machines;
        if let Some(machine) = machines.first()
            && machine.state == "Decommissioning/PoweringOnHost"
        {
            let status = serde_json::to_value(host.status(&DeviceStatusConfig::new(bmc_port)))?;
            let dpus = status["dpus"]
                .as_array()
                .context("host status has no DPU observations")?;
            let applied = aux_power_generations(host)?;
            // Core's earlier ordinary ForceOff also leaves all actors Off. The
            // distinct Aux counters advance only after the owning actors apply
            // that action, including delivery to both DPU actor mailboxes.
            if status["power_state"] == "Off"
                && dpus.len() == 2
                && dpus.iter().all(|dpu| dpu["power_state"] == "Off")
                && before
                    .iter()
                    .all(|(mac, previous)| applied[mac] > *previous)
            {
                for (mac, generation) in before {
                    assert_eq!(
                        applied[mac],
                        *generation + 1,
                        "multiple Aux actions applied before power-on for {mac}"
                    );
                }
                return Ok(json!({
                    "state_version": machine.state_version,
                    "host_power": status["power_state"],
                    "dpu_power": dpus.iter().map(|dpu| dpu["power_state"].clone()).collect::<Vec<_>>(),
                    "aux_generations_before": before,
                    "aux_generations_applied": applied,
                }));
            }
        }
        eyre::ensure!(
            Instant::now() < deadline,
            "did not observe host and both DPUs off after the core aux power reset"
        );
        sleep(Duration::from_millis(20)).await;
    }
}
