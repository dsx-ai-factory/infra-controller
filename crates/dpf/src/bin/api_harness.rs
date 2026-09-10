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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

//! DPF API harness.
//!
//! Exercises the DPF SDK against a real DPF operator. Used to validate
//! provisioning flows.

use std::io::Read;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use carbide_dpf::repository::{DpuRepository, K8sConfigRepository};
use carbide_dpf::{
    DpfError, DpfSdk, DpfSdkBuilder, DpuDeploymentType, DpuDeviceInfo, DpuNodeInfo,
    InitDpfResourcesConfigBuilder, KubeRepository, NAMESPACE, ServiceDefinition, dpu_node_cr_name,
};
use clap::{Parser, Subcommand};
use libredfish::model::BootProgressTypes;
use libredfish::{Redfish, SystemPowerControl};
use serde::Deserialize;

const BMC_SECRET_NAME: &str = "bmc-shared-password";
const DEFAULT_BMC_USERNAME: &str = "root";

#[derive(Parser)]
#[command(name = "dpf-api-harness")]
#[command(about = "Exercise DPF SDK against a real DPF operator", long_about = None)]
struct Cli {
    #[arg(short, long, default_value = NAMESPACE)]
    namespace: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a complete DPU provisioning flow
    Provision {
        /// BFB URL for initialization
        #[arg(long)]
        bfb_url: String,

        /// BMC password for DPU access
        #[arg(long)]
        bmc_password: String,

        /// BMC IP of the host
        #[arg(long)]
        host_bmc_ip: String,

        /// Stable identifier for the host node (e.g. machine UUID)
        #[arg(long)]
        node_id: String,

        /// Comma-separated list of DPU configurations: device_name:dpu_bmc_ip:serial
        #[arg(long)]
        dpus: String,

        /// Timeout in seconds for provisioning to complete
        #[arg(long, default_value = "600")]
        timeout: u64,

        /// Path to JSON file with array of service definitions: [{"name","helm_repo_url","helm_chart","helm_version"}]
        #[arg(long)]
        services_file: Option<std::path::PathBuf>,
    },

    /// Run a reprovisioning flow for an existing DPU
    Reprovision {
        /// BMC IP of the host
        #[arg(long)]
        host_bmc_ip: String,

        /// DPU device name (DPUDevice CR name)
        #[arg(long)]
        device_name: String,

        /// Timeout in seconds
        #[arg(long, default_value = "600")]
        timeout: u64,
    },

    /// Clean up all DPF resources for a host
    Cleanup {
        /// BMC IP of the host
        #[arg(long)]
        host_bmc_ip: String,

        /// Comma-separated list of DPU device names
        #[arg(long)]
        dpu_device_names: String,
    },

    /// Show status of DPF resources
    Status {
        /// BMC IP of the host (optional, shows all if not specified)
        #[arg(long)]
        host_bmc_ip: Option<String>,
    },

    /// Watch for DPF events and log them
    Watch,

    /// Update the BFB reference in a DPUDeployment
    UpdateBfb {
        #[arg(long, default_value = "dpu-deployment")]
        deployment_name: String,
        /// Name of an existing BFB CR
        #[arg(long)]
        bfb_name: String,
    },

    /// Get the current phase of a DPU
    GetPhase {
        #[arg(long)]
        device_name: String,
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Delete the operator-managed DPU CR (triggers the operator to recreate it)
    DeleteDpu {
        #[arg(long)]
        device_name: String,
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Force delete a single DPU and its device
    ForceDeleteDpu {
        #[arg(long)]
        device_name: String,
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Force delete a DPU node and all its DPU devices
    ForceDeleteNode {
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Delete a DPU device
    DeleteDevice {
        #[arg(long)]
        device_name: String,
    },

    /// Delete a DPU node and associated resources
    DeleteNode {
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Check if a DPU node is waiting for external reboot
    IsRebootRequired {
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Clear the external reboot required annotation on a DPU node
    ClearReboot {
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Release the maintenance hold on a DPU node
    ReleaseHold {
        /// DPU node name (e.g. node-{node_id})
        #[arg(long)]
        node_name: String,
    },

    /// Reboot a host via Redfish using BMC credentials from the K8s secret
    RebootHost {
        /// BMC IP of the host to reboot
        #[arg(long)]
        host_bmc_ip: String,

        /// BMC username
        #[arg(long, default_value = DEFAULT_BMC_USERNAME)]
        bmc_username: String,

        /// BMC password override (reads from K8s bmc-shared-password secret if not provided)
        #[arg(long)]
        bmc_password: Option<String>,
    },
}

struct DpuConfig {
    device_name: String,
    dpu_bmc_ip: IpAddr,
    serial_number: String,
}

#[derive(Debug, Deserialize)]
struct ServiceDefinitionFile {
    name: String,
    #[serde(rename = "helm_repo_url")]
    helm_repo_url: String,
    #[serde(rename = "helm_chart")]
    helm_chart: String,
    #[serde(rename = "helm_version")]
    helm_version: String,
}

fn load_services_from_file(path: &std::path::Path) -> Result<Vec<ServiceDefinition>, String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open services file {}: {}", path.display(), e))?;
    let mut contents = String::new();
    f.read_to_string(&mut contents)
        .map_err(|e| format!("Failed to read services file: {}", e))?;
    let raw: Vec<ServiceDefinitionFile> =
        serde_json::from_str(&contents).map_err(|e| format!("Invalid services JSON: {}", e))?;
    Ok(raw
        .into_iter()
        .map(|s| ServiceDefinition::new(s.name, s.helm_repo_url, s.helm_chart, s.helm_version))
        .collect())
}

fn parse_dpus(dpus: &str) -> Result<Vec<DpuConfig>, String> {
    dpus.split(',')
        .map(|s| {
            let parts: Vec<&str> = s.trim().split(':').collect();
            if parts.len() != 3 {
                return Err(format!(
                    "Invalid DPU config '{}', expected device_name:dpu_bmc_ip:serial",
                    s
                ));
            }
            Ok(DpuConfig {
                device_name: parts[0].to_string(),
                dpu_bmc_ip: parts[1]
                    .parse()
                    .map_err(|_| format!("Invalid DPU BMC IP '{}'", parts[1]))?,
                serial_number: parts[2].to_string(),
            })
        })
        .collect()
}

/// Read the BMC password from the K8s secret, falling back to an explicit override.
async fn resolve_bmc_password(
    repo: &KubeRepository,
    namespace: &str,
    password_override: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(pw) = password_override {
        return Ok(pw.to_string());
    }
    let secret = K8sConfigRepository::get_secret(repo, BMC_SECRET_NAME, namespace)
        .await
        .map_err(|e| format!("Failed to read K8s secret {BMC_SECRET_NAME}: {e}"))?
        .ok_or_else(|| {
            format!("K8s secret {BMC_SECRET_NAME} not found in namespace {namespace}")
        })?;
    let bytes = secret
        .get("password")
        .ok_or_else(|| format!("K8s secret {BMC_SECRET_NAME} missing 'password' key"))?;
    String::from_utf8(bytes.clone())
        .map_err(|e| format!("BMC password is not valid UTF-8: {e}").into())
}

/// Reboot a host via Redfish ForceRestart and wait for the OS to come back.
///
/// Snapshots `boot_progress.last_state_time` before issuing the restart, then
/// polls until the timestamp changes and `last_state` reports `OSRunning`.
async fn redfish_reboot_host(
    host_bmc_ip: &str,
    bmc_username: &str,
    bmc_password: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = libredfish::Endpoint {
        host: host_bmc_ip.to_string(),
        user: Some(bmc_username.to_string()),
        password: Some(bmc_password.to_string()),
        ..Default::default()
    };

    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    let client: Box<dyn Redfish> = pool.create_client(endpoint).await?;

    // Snapshot the boot progress timestamp before restarting.
    let pre_reboot_time = client
        .get_system()
        .await
        .ok()
        .and_then(|sys| sys.boot_progress)
        .and_then(|bp| bp.last_state_time);
    tracing::info!(
        host_bmc_ip_address = %host_bmc_ip,
        pre_reboot_time = ?pre_reboot_time,
        "Sending ForceRestart via Redfish"
    );

    client.power(SystemPowerControl::ForceRestart).await?;

    // Poll until boot_progress shows a new timestamp with OSRunning.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if Instant::now() > deadline {
            return Err(
                format!("Timed out waiting for host {host_bmc_ip} to finish rebooting").into(),
            );
        }
        match client.get_system().await {
            Ok(system) => {
                let bp = system.boot_progress.as_ref();
                let current_time = bp.and_then(|b| b.last_state_time.as_deref());
                let current_state = bp.and_then(|b| b.last_state.clone());

                let timestamp_changed = match (&pre_reboot_time, current_time) {
                    (Some(before), Some(now)) => before != now,
                    (None, Some(_)) => true,
                    _ => false,
                };

                let os_running = matches!(current_state, Some(BootProgressTypes::OSRunning))
                    || bp
                        .and_then(|b| b.oem_last_state.as_deref())
                        .is_some_and(|s| s == "OsIsRunning");

                if timestamp_changed && os_running {
                    tracing::info!(
                        host_bmc_ip_address = %host_bmc_ip,
                        last_state_time = ?current_time,
                        "Host rebooted and OS is running"
                    );
                    break;
                }

                tracing::debug!(
                    host_bmc_ip_address = %host_bmc_ip,
                    last_state_time = ?current_time,
                    host_state = ?current_state,
                    timestamp_changed,
                    os_running,
                    "Waiting for host reboot to complete"
                );
            }
            Err(e) => {
                tracing::debug!(
                    host_bmc_ip_address = %host_bmc_ip,
                    error = %e,
                    "Redfish unreachable during reboot, retrying"
                );
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install default CryptoProvider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("carbide_dpf=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    let repo = KubeRepository::new().await?;
    let password_override = match &cli.command {
        Commands::Provision { bmc_password, .. } => Some(bmc_password.as_str()),
        _ => None,
    };
    let password = resolve_bmc_password(&repo, &cli.namespace, password_override).await?;
    let sdk = Arc::new(
        DpfSdkBuilder::new(repo, &cli.namespace, password)
            .build_without_resources()
            .await?,
    );

    match cli.command {
        Commands::Provision {
            bfb_url,
            bmc_password,
            host_bmc_ip,
            node_id,
            dpus,
            timeout,
            services_file,
        } => {
            let services = if let Some(ref path) = services_file {
                load_services_from_file(path)?
            } else {
                vec![]
            };
            run_provisioning_flow(
                sdk,
                &bfb_url,
                &bmc_password,
                &host_bmc_ip,
                &node_id,
                &dpus,
                timeout,
                &services,
            )
            .await?;
        }
        Commands::Reprovision {
            host_bmc_ip,
            device_name,
            timeout,
        } => {
            let node_name = dpu_node_cr_name(&device_name);
            run_reprovisioning_flow(
                sdk,
                &cli.namespace,
                &host_bmc_ip,
                &node_name,
                &device_name,
                timeout,
            )
            .await?;
        }
        Commands::Cleanup {
            host_bmc_ip: _,
            dpu_device_names,
        } => {
            run_cleanup(sdk, &dpu_device_names).await?;
        }
        Commands::Status { host_bmc_ip } => {
            show_status(sdk, &cli.namespace, host_bmc_ip.as_deref()).await?;
        }
        Commands::Watch => {
            run_watcher(sdk).await?;
        }
        Commands::UpdateBfb {
            deployment_name,
            bfb_name,
        } => {
            sdk.update_deployment_bfb(&deployment_name, &bfb_name)
                .await?;
            tracing::info!(deployment = %deployment_name, bfb_name = %bfb_name, "BFB updated");
        }
        Commands::GetPhase {
            device_name,
            node_name,
        } => {
            let phase = sdk.get_dpu_phase(&device_name, &node_name).await?;
            tracing::info!(device_name = %device_name, node = %node_name, phase = ?phase, "DPU phase");
        }
        Commands::DeleteDpu {
            device_name,
            node_name,
        } => {
            sdk.reprovision_dpu(&device_name, &node_name).await?;
            tracing::info!(device_name = %device_name, node = %node_name, "DPU CR deleted");
        }
        Commands::ForceDeleteDpu {
            device_name,
            node_name,
        } => {
            sdk.force_delete_dpu(&device_name, &node_name).await?;
            tracing::info!(device_name = %device_name, node = %node_name, "DPU force deleted");
        }
        Commands::ForceDeleteNode { node_name } => {
            sdk.force_delete_dpu_node(&node_name).await?;
            tracing::info!(node = %node_name, "DPU node force deleted");
        }
        Commands::DeleteDevice { device_name } => {
            sdk.delete_dpu_device(&device_name).await?;
            tracing::info!(device_name = %device_name, "DPU device deleted");
        }
        Commands::DeleteNode { node_name } => {
            sdk.delete_dpu_node(&node_name).await?;
            tracing::info!(node = %node_name, "DPU node deleted");
        }
        Commands::IsRebootRequired { node_name } => {
            let required = sdk.is_reboot_required(&node_name).await?;
            tracing::info!(node = %node_name, required, "Reboot required");
        }
        Commands::ClearReboot { node_name } => {
            sdk.reboot_complete(&node_name).await?;
            tracing::info!(node = %node_name, "Reboot annotation cleared");
        }
        Commands::ReleaseHold { node_name } => {
            sdk.release_maintenance_hold(&node_name).await?;
            tracing::info!(node = %node_name, "Maintenance hold released");
        }
        Commands::RebootHost {
            host_bmc_ip,
            bmc_username,
            bmc_password,
        } => {
            let repo = KubeRepository::new().await?;
            let password =
                resolve_bmc_password(&repo, &cli.namespace, bmc_password.as_deref()).await?;
            redfish_reboot_host(&host_bmc_ip, &bmc_username, &password).await?;
            tracing::info!(host_bmc_ip_address = %host_bmc_ip, "Host reboot initiated via Redfish");
        }
    }

    Ok(())
}

/// Actionable events forwarded from the DPU watcher for serial processing.
#[derive(Debug)]
enum DpuAction {
    MaintenanceNeeded { node_name: String },
    RebootRequired { node_name: String },
    Ready,
    Error { dpu_name: String },
}

/// Read the next action from the channel, respecting a deadline.
async fn recv_action(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DpuAction>,
    deadline: Instant,
) -> Result<DpuAction, Box<dyn std::error::Error>> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or("Timeout waiting for DPU action")?;
    let action = tokio::time::timeout(remaining, rx.recv())
        .await
        .map_err(|_| "Timeout waiting for DPU action")?
        .ok_or("Event channel closed unexpectedly")?;
    if let DpuAction::Error { ref dpu_name } = action {
        return Err(format!("DPU {dpu_name} entered error phase").into());
    }
    Ok(action)
}

/// Watch a single device and wait for it to reach Ready.
///
/// Flow (per DPF design): (1) MaintenanceNeeded -> remove maintenance
/// annotation; (2) RebootRequired -> reboot host -> remove reboot annotation;
/// (3) Ready. Caller must do delete and "wait for new DPU by creation time"
/// before this for reprovisioning; do not release maintenance hold before
/// calling this.
///
/// Returns the elapsed wall-clock time on success.
async fn monitor_until_ready(
    sdk: Arc<DpfSdk<KubeRepository>>,
    host_bmc_ip: &str,
    bmc_password: &str,
    device_name: &str,
    timeout: Duration,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DpuAction>();
    let target = device_name.to_string();

    let _watcher = sdk
        .watcher()
        .on_dpu_event(|event| async move {
            tracing::info!(dpu_name = %event.dpu_name, phase = ?event.phase, "DPU event");
            Ok(())
        })
        .on_reboot_required({
            let tx = tx.clone();
            let target = target.clone();
            move |event| {
                let tx = tx.clone();
                let target = target.clone();
                async move {
                    if event.dpu_name.contains(&target) {
                        tx.send(DpuAction::RebootRequired {
                            node_name: event.node_name,
                        })
                        .map_err(|e| DpfError::WatcherError(e.to_string()))?;
                    }
                    Ok(())
                }
            }
        })
        .on_dpu_ready({
            let tx = tx.clone();
            let target = target.clone();
            move |event| {
                let tx = tx.clone();
                let target = target.clone();
                async move {
                    if event.device_name == target {
                        tx.send(DpuAction::Ready)
                            .map_err(|e| DpfError::WatcherError(e.to_string()))?;
                    }
                    Ok(())
                }
            }
        })
        .on_maintenance_needed({
            let tx = tx.clone();
            let target = target.clone();
            move |event| {
                let tx = tx.clone();
                let target = target.clone();
                async move {
                    if event.dpu_name.contains(&target) {
                        tx.send(DpuAction::MaintenanceNeeded {
                            node_name: event.node_name,
                        })
                        .map_err(|e| DpfError::WatcherError(e.to_string()))?;
                    }
                    Ok(())
                }
            }
        })
        .on_error({
            let tx = tx.clone();
            let target = target.clone();
            move |event| {
                let tx = tx.clone();
                let target = target.clone();
                async move {
                    if event.dpu_name.contains(&target) {
                        tx.send(DpuAction::Error {
                            dpu_name: event.dpu_name,
                        })
                        .map_err(|e| DpfError::WatcherError(e.to_string()))?;
                    }
                    Ok(())
                }
            }
        })
        .start()?;
    drop(tx);

    let start = Instant::now();
    let deadline = start + timeout;

    let mut maintenance_released = false;
    let node_name = loop {
        match recv_action(&mut rx, deadline).await? {
            DpuAction::MaintenanceNeeded { node_name: n } => {
                if !maintenance_released {
                    tracing::info!(node = %n, "Maintenance needed, releasing maintenance hold");
                    sdk.release_maintenance_hold(&n).await?;
                    maintenance_released = true;
                }
                continue;
            }
            DpuAction::RebootRequired { node_name: n } => break n,
            other => {
                return Err(format!(
                    "Expected MaintenanceNeeded then RebootRequired, got {other:?}"
                )
                .into());
            }
        }
    };
    tracing::info!(node = %node_name, host_bmc_ip_address = %host_bmc_ip, "Reboot required, rebooting host via Redfish");
    redfish_reboot_host(host_bmc_ip, DEFAULT_BMC_USERNAME, bmc_password)
        .await
        .map_err(|e| format!("Redfish reboot failed: {e}"))?;
    tracing::info!(node = %node_name, "Removing reboot annotation");
    sdk.reboot_complete(&node_name).await?;
    tracing::info!(node = %node_name, "Reboot complete, annotation removed");

    loop {
        match recv_action(&mut rx, deadline).await? {
            DpuAction::RebootRequired { .. } => continue,
            DpuAction::Ready => break,
            other => return Err(format!("Expected Ready, got {other:?}").into()),
        }
    }

    let elapsed = start.elapsed();
    tracing::info!(device_name = %device_name, elapsed = ?elapsed, "DPU ready");
    Ok(elapsed)
}

#[allow(clippy::too_many_arguments)]
async fn run_provisioning_flow(
    sdk: Arc<DpfSdk<KubeRepository>>,
    bfb_url: &str,
    bmc_password: &str,
    host_bmc_ip: &str,
    node_id: &str,
    dpus_str: &str,
    timeout_secs: u64,
    services: &[ServiceDefinition],
) -> Result<(), Box<dyn std::error::Error>> {
    let dpus = parse_dpus(dpus_str)?;
    let host_bmc_ip_addr: IpAddr = host_bmc_ip.parse()?;
    let timeout = Duration::from_secs(timeout_secs);

    tracing::info!("=== DPF Provisioning Flow ===");
    tracing::info!(host_bmc_ip_address = %host_bmc_ip, dpu_count = dpus.len(), timeout_seconds = timeout_secs, "Starting provisioning");

    tracing::info!("[1/4] Initializing DPF resources...");
    let init_config = InitDpfResourcesConfigBuilder::default()
        .bfb_url(bfb_url)
        .services(services.to_vec())
        .build()?;
    sdk.create_initialization_objects(&init_config).await?;
    tracing::info!("BFB, DPUFlavor, and DPUDeployment created");

    tracing::info!("[2/4] Registering DPU devices...");
    for dpu in &dpus {
        let info = DpuDeviceInfo {
            device_id: dpu.device_name.clone(),
            dpu_bmc_ip: dpu.dpu_bmc_ip,
            host_bmc_ip: host_bmc_ip_addr,
            serial_number: dpu.serial_number.clone(),
            dpu_machine_id: String::new(),
            is_primary: true,
        };
        sdk.register_dpu_device(info, None).await?;
        tracing::info!(device_name = %dpu.device_name, serial = %dpu.serial_number, "Registered device");
    }

    tracing::info!("[3/4] Registering DPU node...");
    let node_info = DpuNodeInfo {
        node_id: node_id.to_string(),
        host_bmc_ip: host_bmc_ip_addr,
        device_ids: dpus.iter().map(|d| d.device_name.clone()).collect(),
        deployment_type: DpuDeploymentType::Bf3,
    };
    sdk.register_dpu_node(node_info).await?;
    tracing::info!(dpu_count = dpus.len(), "Node registered");

    let _node_name = dpu_node_cr_name(node_id);
    tracing::info!(
        "[4/4] Monitoring DPU provisioning (maintenance -> release hold; reboot -> reboot + clear annotation)..."
    );
    let first_device = dpus
        .first()
        .map(|d| d.device_name.as_str())
        .ok_or("No DPUs registered on node")?;

    let elapsed =
        monitor_until_ready(sdk, host_bmc_ip, bmc_password, first_device, timeout).await?;

    tracing::info!(
        dpu_count = dpus.len(),
        elapsed_seconds = elapsed.as_secs(),
        "=== Provisioning Complete ==="
    );
    Ok(())
}

fn k8s_time_to_system_time(
    t: &k8s_openapi::apimachinery::pkg::apis::meta::v1::Time,
) -> Option<SystemTime> {
    let ms = t.0.as_millisecond();
    let ms = ms.max(0) as u64;
    Some(UNIX_EPOCH + Duration::from_millis(ms))
}

/// Clock skew tolerance when comparing harness time to API server creation_timestamp.
const CREATED_AFTER_TOLERANCE: Duration = Duration::from_secs(15);

/// Wait for a DPU with the given device name to appear with creation time after `created_after`.
/// Idempotent: any phase is fine; we only require that the CR was created after the delete.
/// Uses a tolerance so CRs are accepted when the API server clock is behind the harness.
async fn wait_for_dpu_created_after(
    repo: Arc<KubeRepository>,
    namespace: &str,
    device_name: &str,
    created_after: SystemTime,
    timeout: Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    use carbide_dpf::crds::dpus_generated::DPU;
    let cutoff = created_after
        .checked_sub(CREATED_AFTER_TOLERANCE)
        .unwrap_or(UNIX_EPOCH);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let tx_for_watch = tx.clone();
    let namespace = namespace.to_string();
    let device_name = device_name.to_string();
    let watch_repo = repo.clone();
    let _watch_handle = tokio::spawn(async move {
        watch_repo
            .watch(&namespace, None, move |dpu: Arc<DPU>| {
                let name = dpu.metadata.name.as_deref().unwrap_or_default();
                if dpu.spec.dpu_device_name != device_name {
                    tracing::debug!(
                        dpu_name = %name,
                        spec_device = %dpu.spec.dpu_device_name,
                        want_device = %device_name,
                        "DPU skip (device name mismatch)"
                    );
                    return std::future::ready(Ok::<(), DpfError>(()));
                }
                let Some(ct) = dpu
                    .metadata
                    .creation_timestamp
                    .as_ref()
                    .and_then(k8s_time_to_system_time)
                else {
                    tracing::info!(
                        dpu_name = %name,
                        "DPU skip (no creation_timestamp or conversion failed)"
                    );
                    return std::future::ready(Ok::<(), DpfError>(()));
                };
                let ct_ms = ct
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let cutoff_ms = cutoff
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let passes = ct > cutoff;
                tracing::info!(
                    dpu_name = %name,
                    creation_time_milliseconds = ct_ms,
                    cutoff_time_milliseconds = cutoff_ms,
                    passes = passes,
                    "DPU created_after check"
                );
                if passes {
                    let _ = tx_for_watch.send(dpu.spec.dpu_node_name.clone());
                }
                std::future::ready(Ok::<(), DpfError>(()))
            })
            .await;
    });
    let result = tokio::time::timeout(timeout, rx.recv()).await;
    drop(tx);
    match result {
        Ok(Some(node_name)) => Ok(node_name),
        Ok(None) => Err("Watch channel closed before DPU recreated".into()),
        Err(_) => Err("Timeout waiting for new DPU after reprovision".into()),
    }
}

async fn run_reprovisioning_flow(
    sdk: Arc<DpfSdk<KubeRepository>>,
    namespace: &str,
    host_bmc_ip: &str,
    node_name: &str,
    device_name: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(timeout_secs);
    let repo = Arc::new(KubeRepository::new().await?);

    tracing::info!("=== DPF Reprovisioning Flow ===");
    tracing::info!(node = %node_name, device_name = %device_name, "Starting reprovisioning");

    let created_after = SystemTime::now();
    tracing::info!("[1/4] Triggering reprovision (delete DPU CR)...");
    sdk.reprovision_dpu(device_name, node_name).await?;
    tracing::info!("Reprovision triggered");

    tracing::info!("[2/4] Waiting for new DPU (creation time after delete)...");
    let _new_node_name =
        wait_for_dpu_created_after(repo.clone(), namespace, device_name, created_after, timeout)
            .await?;
    tracing::info!("New DPU CR observed");

    tracing::info!(
        "[3/4] Monitoring DPU until ready (maintenance -> release hold; reboot -> reboot + clear annotation)..."
    );
    let bmc_password = resolve_bmc_password(repo.as_ref(), namespace, None).await?;
    let elapsed =
        monitor_until_ready(sdk, host_bmc_ip, &bmc_password, device_name, timeout).await?;

    tracing::info!(
        device_name = %device_name,
        elapsed_seconds = elapsed.as_secs(),
        "=== Reprovisioning Complete ==="
    );
    Ok(())
}

async fn run_cleanup(
    sdk: Arc<DpfSdk<KubeRepository>>,
    dpu_device_names: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let device_names: Vec<String> = dpu_device_names
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let node_id = device_names
        .first()
        .ok_or("dpu_device_names must not be empty")?;

    tracing::info!("=== DPF Cleanup ===");
    tracing::info!(node = %node_id, device_names = ?device_names, "Starting cleanup");

    sdk.force_delete_host(node_id, &device_names).await?;

    tracing::info!("Cleanup complete");
    Ok(())
}

async fn show_status(
    sdk: Arc<DpfSdk<KubeRepository>>,
    namespace: &str,
    host_bmc_ip: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_dpf::repository::{
        BfbRepository, DpuDeploymentRepository, DpuDeviceRepository, DpuNodeRepository,
        DpuRepository, DpuServiceConfigurationRepository, DpuServiceTemplateRepository,
    };

    let repo = KubeRepository::new().await?;

    tracing::info!("=== DPF Status ===");
    tracing::info!(namespace, "Fetching status");

    // List BFBs
    tracing::info!("BFBs:");
    let bfbs = BfbRepository::list(&repo, namespace).await?;
    if bfbs.is_empty() {
        tracing::info!("  (none)");
    }
    for bfb in bfbs {
        let name = bfb.metadata.name.unwrap_or_default();
        let phase = bfb
            .status
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "Unknown".to_string());
        tracing::info!(name = %name, phase = %phase, "BFB");
    }

    // List DPU Nodes
    tracing::info!("DPU Nodes:");
    let nodes = DpuNodeRepository::list(&repo, namespace).await?;
    if nodes.is_empty() {
        tracing::info!("  (none)");
    }
    for node in &nodes {
        let name = node.metadata.name.clone().unwrap_or_default();
        let dpu_count = node.spec.dpus.as_ref().map(|d| d.len()).unwrap_or(0);

        // Filter by host_bmc_ip if specified
        if let Some(filter_ip) = host_bmc_ip
            && !name.contains(&filter_ip.replace('.', "-"))
        {
            continue;
        }

        tracing::info!(name = %name, dpu_count, "DPU Node");
        if let Some(dpus) = &node.spec.dpus {
            for dpu in dpus {
                tracing::info!(dpu_name = %dpu.name, "  DPU");
            }
        }
    }

    // List DPU CRs (operator-created)
    tracing::info!("DPUs:");
    let dpus = DpuRepository::list(&repo, namespace, None).await?;
    if dpus.is_empty() {
        tracing::info!("  (none)");
    }
    for dpu in &dpus {
        let name = dpu.metadata.name.clone().unwrap_or_default();
        let phase_str = dpu
            .status
            .as_ref()
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "Unknown".to_string());
        if let Some(filter_ip) = host_bmc_ip
            && !name.contains(&filter_ip.replace('.', "-"))
        {
            continue;
        }
        tracing::info!(name = %name, phase = %phase_str, "DPU");
    }

    // List DPU Devices
    tracing::info!("DPU Devices:");
    let devices = DpuDeviceRepository::list(&repo, namespace).await?;
    if devices.is_empty() {
        tracing::info!("  (none)");
    }
    for device in devices {
        let name = device.metadata.name.unwrap_or_default();
        let serial = &device.spec.serial_number;
        let conditions = device.status.and_then(|s| s.conditions).unwrap_or_default();
        let status_str = conditions
            .first()
            .map(|c| c.type_.clone())
            .unwrap_or_else(|| "Unknown".to_string());
        tracing::info!(name = %name, serial = %serial, dpu_device_status = %status_str, "DPU Device");
    }

    // List DPUDeployments
    tracing::info!("DPU Deployments:");
    let deployments = DpuDeploymentRepository::list(&repo, namespace).await?;
    if deployments.is_empty() {
        tracing::info!("  (none)");
    }
    for dep in &deployments {
        let name = dep.metadata.name.clone().unwrap_or_default();
        let bfb = dep.spec.dpus.bfb.clone().unwrap_or_default();
        tracing::info!(name = %name, bfb = %bfb, "DPUDeployment");
    }

    // List DPUServiceTemplates
    tracing::info!("DPU Service Templates:");
    let templates = DpuServiceTemplateRepository::list(&repo, namespace).await?;
    if templates.is_empty() {
        tracing::info!("  (none)");
    }
    for tmpl in &templates {
        let name = tmpl.metadata.name.clone().unwrap_or_default();
        tracing::info!(name = %name, "DPUServiceTemplate");
    }

    // List DPUServiceConfigurations
    tracing::info!("DPU Service Configurations:");
    let configs = DpuServiceConfigurationRepository::list(&repo, namespace).await?;
    if configs.is_empty() {
        tracing::info!("  (none)");
    }
    for cfg in &configs {
        let name = cfg.metadata.name.clone().unwrap_or_default();
        tracing::info!(name = %name, "DPUServiceConfiguration");
    }

    let _ = sdk; // Suppress unused warning
    Ok(())
}

async fn run_watcher(sdk: Arc<DpfSdk<KubeRepository>>) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("=== DPF Event Watcher ===");
    tracing::info!("Press Ctrl+C to stop");

    let watcher = sdk
        .watcher()
        .on_dpu_event(|event| async move {
            tracing::info!(dpu_name = %event.dpu_name, phase = ?event.phase, "PHASE");
            Ok(())
        })
        .on_reboot_required(|event| async move {
            tracing::warn!(node = %event.node_name, host_bmc_ip_address = %event.host_bmc_ip, "REBOOT REQUIRED");
            Ok(())
        })
        .on_dpu_ready(|event| async move {
            tracing::info!(dpu_name = %event.dpu_name, device_name = %event.device_name, "READY");
            Ok(())
        })
        .on_error(|event| async move {
            tracing::error!(dpu_name = %event.dpu_name, device_name = %event.device_name, node = %event.node_name, "ERROR");
            Ok(())
        })
        .start()?;

    tokio::signal::ctrl_c().await?;
    tracing::info!("Stopping watcher...");
    drop(watcher);

    Ok(())
}
