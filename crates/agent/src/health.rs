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

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use health_report::HealthProbeId;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use crate::{HBNDeviceNames, hbn};
mod bgp;
pub(crate) mod nvue;
mod probe_ids;

const HBN_DAEMONS_FILE: &str = "etc/frr/daemons";
const DHCP_SERVER_FILE: &str = "etc/supervisor/conf.d/default-forge-dhcp-server.conf";
// const NVUE_FILE: &str = "etc/nvue.d/startup.yaml";

const EXPECTED_FILES: [&str; 4] = [
    "etc/frr/frr.conf",
    "etc/network/interfaces",
    DHCP_SERVER_FILE,
    HBN_DAEMONS_FILE,
];

const RSYSLOG_SERVICE: &str = "rsyslog";
const EXPECTED_SERVICES: [&str; 3] = ["frr", "nl2doca", RSYSLOG_SERVICE];
const DHCP_SERVER_SERVICE: &str = "forge-dhcp-server-default";
/// Maximum allowed disk utilization in % before the DpuDiskUtilizationCritical health alert will be sent
const MAX_DISK_UTILIZATION: u32 = 85;

fn failed(
    health_report: &mut health_report::HealthReport,
    probe_id: impl Into<HealthProbeId>,
    probe_target: Option<String>,
    message: String,
) {
    health_report
        .alerts
        .push(make_alert(probe_id, probe_target, message, true));
}

fn make_alert(
    probe_id: impl Into<HealthProbeId>,
    probe_target: Option<String>,
    message: String,
    is_critical: bool,
) -> health_report::HealthProbeAlert {
    let health_classifications = match is_critical {
        true => vec![
            health_report::HealthAlertClassification::prevent_allocations(),
            health_report::HealthAlertClassification::prevent_host_state_changes(),
        ],
        false => vec![],
    };
    make_classified_alert(probe_id, probe_target, message, health_classifications)
}

fn make_classified_alert(
    probe_id: impl Into<HealthProbeId>,
    probe_target: Option<String>,
    message: String,
    health_classifications: Vec<health_report::HealthAlertClassification>,
) -> health_report::HealthProbeAlert {
    health_report::HealthProbeAlert {
        id: probe_id.into(),
        target: probe_target,
        in_alert_since: None,
        message,
        tenant_message: None,
        classifications: health_classifications,
    }
}

fn passed(
    health_report: &mut health_report::HealthReport,
    probe_id: impl Into<HealthProbeId>,
    probe_target: Option<String>,
) {
    health_report
        .successes
        .push(health_report::HealthProbeSuccess {
            id: probe_id.into(),
            target: probe_target,
        })
}

/// Makes the controller wait for one more health sample after a network update.
///
/// The config version and health report are published together. This critical
/// alert makes the controller wait for the next health sample instead of acting
/// on neighbor state collected before HBN applied the update.
fn add_post_config_wait_alert(
    health_report: &mut health_report::HealthReport,
    should_wait_for_post_config_check: bool,
) {
    if should_wait_for_post_config_check {
        failed(
            health_report,
            probe_ids::PostConfigCheckWait.clone(),
            None,
            "Post-config waiting period".to_string(),
        );
    }
}

/// Is enough of HBN ready so that we can configure it?
pub(super) fn is_up(health_report: &health_report::HealthReport) -> bool {
    let has_failed_services = health_report
        .alerts
        .iter()
        .any(|a| a.id == *probe_ids::ServiceRunning);
    let container_exists = health_report
        .successes
        .iter()
        .any(|s| s.id == *probe_ids::ContainerExists);
    let supervisord_up = health_report
        .successes
        .iter()
        .any(|s| s.id == *probe_ids::SupervisorctlStatus);
    let nvue_api_up = health_report
        .successes
        .iter()
        .any(|s| s.id == *probe_ids::NvueApiRunning);
    let hbn_healthy = (container_exists && supervisord_up) || nvue_api_up;
    hbn_healthy && !has_failed_services
}

pub(super) struct HealthCheckParams<'a> {
    pub(super) hbn_root: &'a Path,
    pub(super) host_routes: &'a [&'a str],
    pub(super) has_changed_configs: bool,
    pub(super) min_healthy_links: usize,
    pub(super) route_servers: &'a [String],
    /// Whether this check should require the FNN IPv6-unicast underlay.
    pub(super) should_check_ipv6_unicast: bool,
    pub(super) hbn_device_names: HBNDeviceNames,
    pub(super) include_dhcp_server: bool,
    pub(super) run_restricted_mode_check: bool,
}

/// Check the health of HBN
pub(super) async fn health_check(params: HealthCheckParams<'_>) -> health_report::HealthReport {
    let mut hr = health_report::HealthReport::empty("forge-dpu-agent".to_string());

    // Check whether the disk is full
    check_disk_utilization(&mut hr).await;

    // Check whether HBN is up
    let container_id = match hbn::get_hbn_container_id().await {
        Ok(id) => id,
        Err(err) => {
            failed(
                &mut hr,
                probe_ids::ContainerExists.clone(),
                None,
                err.to_string(),
            );
            return hr;
        }
    };
    passed(&mut hr, probe_ids::ContainerExists.clone(), None);
    check_hbn_services_running(&mut hr, &container_id, &EXPECTED_SERVICES).await;

    if params.run_restricted_mode_check {
        // We want these checks whether HBN is up or not
        check_restricted_mode(&mut hr).await;
    }

    // We only want these checks if HBN is up
    if !is_up(&hr) {
        return hr;
    }
    if params.include_dhcp_server {
        check_dhcp_server(&mut hr, &container_id).await;
    }
    check_ifreload(&mut hr, &container_id).await;
    let hbn_daemons_file = params.hbn_root.join(HBN_DAEMONS_FILE);
    bgp::check_daemon_enabled(&mut hr, &hbn_daemons_file.to_string_lossy());
    bgp::check_bgp_stats(
        &mut hr,
        &container_id,
        bgp::BgpHealthCheckParams {
            host_routes: params.host_routes,
            min_healthy_links: params.min_healthy_links,
            route_servers: params.route_servers,
            should_check_ipv6_unicast: params.should_check_ipv6_unicast,
            hbn_device_names: &params.hbn_device_names,
        },
    )
    .await;
    check_files(&mut hr, params.hbn_root, &EXPECTED_FILES);

    add_post_config_wait_alert(&mut hr, params.has_changed_configs);

    hr
}

// HBN processes should be running
async fn check_hbn_services_running(
    hr: &mut health_report::HealthReport,
    container_id: &str,
    expected_services: &[&str],
) {
    // `supervisorctl status` has exit code 3 if there are stopped processes (which we expect),
    // so final param is 'false' here.
    // https://github.com/Supervisor/supervisor/issues/1223
    let sctl = match hbn::run_in_container(container_id, &["supervisorctl", "status"], false).await
    {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Failed to get supervisorctl status for HBN health check"
            );
            failed(
                hr,
                probe_ids::SupervisorctlStatus.clone(),
                None,
                err.to_string(),
            );
            return;
        }
    };
    let st = match parse_status(&sctl) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Failed to parse supervisorctl status for HBN health check"
            );
            failed(
                hr,
                probe_ids::SupervisorctlStatus.clone(),
                None,
                err.to_string(),
            );
            return;
        }
    };
    passed(hr, probe_ids::SupervisorctlStatus.clone(), None);

    for service in expected_services {
        record_hbn_service_health(hr, service, st.status_of(service));
    }
}

/// Reports one HBN service's Supervisor state.
///
/// HBN restarts `rsyslog` during log rotation. It is outside the network data
/// path, so its `STARTING` state emits neither a success nor an alert. Supervisor
/// bounds that transition through `startsecs`; the next sample reports `RUNNING`
/// or a failure state.
fn record_hbn_service_health(
    hr: &mut health_report::HealthReport,
    service: &str,
    status: SctlState,
) {
    match status {
        SctlState::Running => passed(
            hr,
            probe_ids::ServiceRunning.clone(),
            Some(service.to_string()),
        ),
        SctlState::Starting if service == RSYSLOG_SERVICE => {}
        status => {
            tracing::warn!(
                service,
                service_state = %status,
                "HBN service is not running"
            );
            failed(
                hr,
                probe_ids::ServiceRunning.clone(),
                Some(service.to_string()),
                format!("{service} is {status}, need {}", SctlState::Running),
            );
        }
    }
}

// dhcp relay should be running
// Very similar to check_hbn_services_running, except it happens _after_ we start configuring.
// The other services must be up before we start configuring.
// Out of relay and dhcp server, only and only one should be up.
async fn check_dhcp_server(hr: &mut health_report::HealthReport, container_id: &str) {
    // `supervisorctl status` has exit code 3 if there are stopped processes (which we expect),
    // so final param is 'false' here.
    // https://github.com/Supervisor/supervisor/issues/1223
    let sctl = match hbn::run_in_container(container_id, &["supervisorctl", "status"], false).await
    {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Failed to get supervisorctl status for DHCP health check"
            );
            failed(
                hr,
                probe_ids::SupervisorctlStatus.clone(),
                None,
                err.to_string(),
            );
            return;
        }
    };
    let st = match parse_status(&sctl) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "Failed to parse supervisorctl status for DHCP health check"
            );
            failed(
                hr,
                probe_ids::SupervisorctlStatus.clone(),
                None,
                err.to_string(),
            );
            return;
        }
    };

    let dhcp_server_status = match st.status_of(DHCP_SERVER_SERVICE) {
        SctlState::Running => {
            passed(hr, probe_ids::DhcpServer.clone(), None);
            None
        }
        status => Some(status),
    };

    if let Some(x) = dhcp_server_status {
        tracing::warn!("check_dhcp_server: Not running.");
        failed(
            hr,
            probe_ids::DhcpServer.clone(),
            None,
            format!("Dhcp-server is not running. Status: {x}"),
        );
    }
}

// `ifreload` should exit code 0 and have no output
async fn check_ifreload(hr: &mut health_report::HealthReport, container_id: &str) {
    match hbn::run_in_container(container_id, &["ifreload", "--all", "--syntax-check"], true).await
    {
        Ok(stdout) => {
            if stdout.is_empty() {
                passed(hr, probe_ids::Ifreload.clone(), None);
            } else {
                tracing::warn!(stdout = stdout.as_str(), "ifreload syntax check failed");
                failed(hr, probe_ids::Ifreload.clone(), None, stdout);
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "ifreload syntax check failed");
            failed(hr, probe_ids::Ifreload.clone(), None, err.to_string());
        }
    }
}

// The files VPC creates should exist
fn check_files(hr: &mut health_report::HealthReport, hbn_root: &Path, expected_files: &[&str]) {
    const MIN_SIZE: u64 = 100;
    let mut dhcp_server_size = 0;
    for filename in expected_files {
        let path = hbn_root.join(filename);
        if path.exists() {
            passed(
                hr,
                probe_ids::FileExists.clone(),
                Some(path.display().to_string()),
            );
        } else {
            failed(
                hr,
                probe_ids::FileExists.clone(),
                Some(path.display().to_string()),
                "Not found".to_string(),
            );
            continue;
        }
        let stat = match std::fs::metadata(path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(filename, error = %err, "Failed to read file metadata");
                failed(
                    hr,
                    probe_ids::FileIsValid.clone(),
                    Some(filename.to_string()),
                    err.to_string(),
                );
                continue;
            }
        };
        if filename == &DHCP_SERVER_FILE {
            dhcp_server_size = stat.len();
        } else if stat.len() < MIN_SIZE {
            tracing::warn!(
                filename,
                file_size_bytes = stat.len(),
                minimum_file_size_bytes = MIN_SIZE,
                "file is too small"
            );
            failed(
                hr,
                probe_ids::FileIsValid.clone(),
                Some(filename.to_string()),
                "Too small".to_string(),
            );
        }
        passed(
            hr,
            probe_ids::FileIsValid.clone(),
            Some(filename.to_string()),
        );
    }

    if dhcp_server_size < MIN_SIZE {
        tracing::warn!(
            filename = DHCP_SERVER_FILE,
            file_size_bytes = dhcp_server_size,
            minimum_file_size_bytes = MIN_SIZE,
            "file is too small"
        );
        failed(
            hr,
            probe_ids::FileIsValid.clone(),
            Some(DHCP_SERVER_FILE.to_string()),
            "Too small".to_string(),
        );
    }
}

// A DPU should be in restricted mode
async fn check_restricted_mode(hr: &mut health_report::HealthReport) {
    const EXPECTED_PRIV_LEVEL: &str = "RESTRICTED";
    let mut cmd = TokioCommand::new("bash");
    cmd.arg("-c")
        .arg("mlxprivhost -d /dev/mst/mt*_pciconf0 query");
    cmd.kill_on_drop(true);

    let cmd_str = super::pretty_cmd(cmd.as_std());
    let Ok(cmd_res) = timeout(Duration::from_secs(10), cmd.output()).await else {
        failed(
            hr,
            probe_ids::RestrictedMode.clone(),
            None,
            format!("Timeout running '{cmd_str}'."),
        );
        return;
    };
    let out = match cmd_res {
        Ok(out) => out,
        Err(err) => {
            failed(
                hr,
                probe_ids::RestrictedMode.clone(),
                None,
                format!("Error running '{cmd_str}'. {err}"),
            );
            return;
        }
    };
    if !out.status.success() {
        tracing::debug!(
            command = %super::pretty_cmd(cmd.as_std()),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "STDERR"
        );
        failed(
            hr,
            probe_ids::RestrictedMode.clone(),
            None,
            format!(
                "{} for cmd '{}'",
                out.status,
                super::pretty_cmd(cmd.as_std())
            ),
        );
        return;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    match parse_mlxprivhost(s.as_ref()) {
        Ok(priv_level) if priv_level == EXPECTED_PRIV_LEVEL => {
            passed(hr, probe_ids::RestrictedMode.clone(), None);
        }
        Ok(priv_level) => {
            failed(
                hr,
                probe_ids::RestrictedMode.clone(),
                None,
                format!(
                    "mlxprivhost reports level '{priv_level}', expected '{EXPECTED_PRIV_LEVEL}'"
                ),
            );
        }
        Err(err) => {
            failed(
                hr,
                probe_ids::RestrictedMode.clone(),
                None,
                format!("parse_mlxprivhost: {err}"),
            );
        }
    }
}

fn parse_mlxprivhost(s: &str) -> eyre::Result<String> {
    let Some(level_line) = s.lines().find(|line| line.contains("level")) else {
        eyre::bail!("invalid mlxprivhost output, missing 'level' line:\n{s}");
    };
    // Example ouput:
    // level                         : RESTRICTED
    let Some(level) = level_line.split(':').nth(1).map(|level| level.trim()) else {
        eyre::bail!("invalid level line, needs a single colon: '{level_line}'");
    };
    Ok(level.to_string())
}

// Checks whether the disk is not full
async fn check_disk_utilization(hr: &mut health_report::HealthReport) {
    let mut cmd = TokioCommand::new("bash");
    cmd.arg("-c").arg("df -HP");
    cmd.kill_on_drop(true);

    let cmd_str = super::pretty_cmd(cmd.as_std());
    let Ok(cmd_res) = timeout(Duration::from_secs(10), cmd.output()).await else {
        failed(
            hr,
            probe_ids::DpuDiskUtilizationCheck.clone(),
            None,
            format!("Timeout running '{cmd_str}'."),
        );
        return;
    };
    let out = match cmd_res {
        Ok(out) => out,
        Err(err) => {
            failed(
                hr,
                probe_ids::DpuDiskUtilizationCheck.clone(),
                None,
                format!("Error running '{cmd_str}'. {err}"),
            );
            return;
        }
    };
    if !out.status.success() {
        tracing::debug!(
            command = %super::pretty_cmd(cmd.as_std()),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "STDERR"
        );
        failed(
            hr,
            probe_ids::DpuDiskUtilizationCheck.clone(),
            None,
            format!(
                "{} for cmd '{}'",
                out.status,
                super::pretty_cmd(cmd.as_std())
            ),
        );
        return;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    match parse_disk_utilization(s.as_ref()) {
        Ok(utilization) => match utilization.utilizations.get("/") {
            Some(u) => {
                if u.utilization > MAX_DISK_UTILIZATION {
                    failed(
                        hr,
                        probe_ids::DpuDiskUtilizationCritical.clone(),
                        None,
                        format!(
                            "Disk utilization for root path / is {} (bigger than 85%):\nTotal:{}\nUsed:{}\nAvailable:{}",
                            u.utilization, u.size, u.used, u.available
                        ),
                    );
                } else {
                    passed(hr, probe_ids::DpuDiskUtilizationCheck.clone(), None);
                }
            }
            None => {
                failed(
                    hr,
                    probe_ids::DpuDiskUtilizationCheck.clone(),
                    None,
                    format!(
                        "Disk utilization for rootfs is unknown. Gathered utilizations by mountpoint: {:?}",
                        utilization.utilizations
                    ),
                );
            }
        },
        Err(err) => {
            failed(
                hr,
                probe_ids::DpuDiskUtilizationCheck.clone(),
                None,
                format!("Failed to parse Disk utilization output: {err}"),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiskUtilization {
    device: String,
    utilization: u32,
    available: String,
    used: String,
    size: String,
}

struct DiskUtilizations {
    /// Maps from mount point name to disk utilization
    utilizations: HashMap<String, DiskUtilization>,
}

/// Parses the output of df -HP
fn parse_disk_utilization(df_out: &str) -> eyre::Result<DiskUtilizations> {
    let mut u = HashMap::new();
    for line in df_out.lines().skip(1) {
        let parts: Vec<&str> = line.split_ascii_whitespace().collect();
        if parts.len() < 6 {
            tracing::warn!(line, "df output line too short");
            continue;
        }

        let device = parts[0].to_string();
        let size = parts[1].to_string();
        let used = parts[2].to_string();
        let available = parts[3].to_string();
        let utilization = parts[4].trim_end_matches('%');
        let mount_point = parts[5].to_string();

        let utilization: u32 = match utilization.parse() {
            Ok(u) => u,
            Err(_) => {
                tracing::warn!(line, "Can not parse disk utilization in line");
                continue;
            }
        };

        u.insert(
            mount_point,
            DiskUtilization {
                utilization,
                available,
                used,
                size,
                device,
            },
        );
    }

    Ok(DiskUtilizations { utilizations: u })
}

fn parse_status(status_out: &str) -> eyre::Result<SctlStatus> {
    let mut m = HashMap::new();
    for line in status_out.lines() {
        let parts: Vec<&str> = line.split_ascii_whitespace().collect();
        if parts.len() < 2 {
            tracing::warn!(line, "supervisorctl status line too short");
            continue;
        }
        let state: SctlState = match parts[1].parse() {
            Ok(s) => s,
            Err(_err) => {
                // unreachable but future proof. SctlState::from_str is currently infallible.
                tracing::warn!(
                    supervisor_state = parts[1],
                    line,
                    "supervisorctl status invalid state in line"
                );
                continue;
            }
        };
        m.insert(parts[0].to_string(), state);
    }
    Ok(SctlStatus { m })
}

struct SctlStatus {
    m: HashMap<String, SctlState>,
}

impl SctlStatus {
    fn status_of(&self, process: &str) -> SctlState {
        *self.m.get(process).unwrap_or(&SctlState::Unknown)
    }
}

impl FromStr for SctlState {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "STOPPED" => Self::Stopped,
            "STARTING" => Self::Starting,
            "RUNNING" => Self::Running,
            "BACKOFF" => Self::Backoff,
            "STOPPING" => Self::Stopping,
            "EXITED" => Self::Exited,
            "FATAL" => Self::Fatal,
            _ => {
                tracing::warn!(supervisor_state = s, "Unknown supervisorctl status");
                Self::Unknown
            }
        })
    }
}

impl fmt::Display for SctlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Stopped => "STOPPED",
                Self::Starting => "STARTING",
                Self::Running => "RUNNING",
                Self::Backoff => "BACKOFF",
                Self::Stopping => "STOPPING",
                Self::Exited => "EXITED",
                Self::Fatal => "FATAL",
                Self::Unknown => "UNKNOWN",
            }
        )
    }
}

// Supervisorctl process states
// https://supervisord.org/subprocess.html?#process-states
#[derive(PartialEq, Debug, Copy, Clone, Default)]
enum SctlState {
    #[default]
    Unknown,
    Stopped,
    Starting,
    Running,
    Backoff,
    Stopping,
    Exited,
    Fatal,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;

    // Should these gaps be tabs? Yes. Are they tabs? No. `supervisorctl` outputs spaces.
    const SUPERVISORCTL_STATUS_OUT: &str = r#"
cron                             RUNNING   pid 15, uptime 4:18:47
decrypt-user-add                 EXITED    Mar 06 06:24 PM
frr                              RUNNING   pid 17, uptime 4:18:47
frr-reload                       STOPPED   Not started
ifreload                         EXITED    Mar 06 06:24 PM
isc-dhcp-relay-default           RUNNING   pid 19, uptime 4:18:47
neighmgr                         RUNNING   pid 21, uptime 4:18:47
nginx                            RUNNING   pid 22, uptime 4:18:47
nl2doca                          RUNNING   pid 23, uptime 4:18:47
nvued                            STOPPED   Mar 06 06:24 PM
nvued-startup                    EXITED    Mar 06 06:24 PM
rsyslog                          RUNNING   pid 27, uptime 4:18:47
sysctl-apply                     EXITED    Mar 06 06:24 PM
"#;

    const MLXPRIVHOST_OUT: &str = r#"Host configurations
-------------------
level                         : RESTRICTED

Port functions status:
-----------------------
disable_rshim                 : TRUE
disable_tracer                : TRUE
disable_port_owner            : TRUE
disable_counter_rd            : TRUE

"#;

    const DISKUTIL_OUT: &str = r#"Filesystem      Size  Used Avail Use% Mounted on
tmpfs            17G  4.1k   17G   1% /dev/shm
tmpfs           6.8G   17M  6.7G   1% /run
tmpfs           5.3M  8.2k  5.3M   1% /run/lock
/dev/mmcblk0p2   41G   12G   27G  31% /
/dev/mmcblk0p1   52M  9.0M   43M  18% /boot/efi
shm              68M     0   68M   0% /run/containerd/io.containerd.grpc.v1.cri/sandboxes/3c9db06a2021f14b6cb98d0583ae43909f282c6d670dd9da071bddf333caaa4e/shm
overlay          41G   12G   27G  31% /run/containerd/io.containerd.runtime.v2.task/k8s.io/3c9db06a2021f14b6cb98d0583ae43909f282c6d670dd9da071bddf333caaa4e/rootfs
shm              68M  8.2k   68M   1% /run/containerd/io.containerd.grpc.v1.cri/sandboxes/5e38cefc8507fcf3b872fa12f21bdbcc09244832c24a34871ef9d8d519fa37b9/shm
overlay          41G   12G   27G  31% /run/containerd/io.containerd.runtime.v2.task/k8s.io/5e38cefc8507fcf3b872fa12f21bdbcc09244832c24a34871ef9d8d519fa37b9/rootfs
tmpfs           3.4G     0  3.4G   0% /run/user/1002
"#;

    /// Builds the critical `ServiceRunning` alert expected for a failed HBN service.
    fn expected_service_running_alert(
        service: &str,
        status: SctlState,
    ) -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert {
            id: probe_ids::ServiceRunning.clone(),
            target: Some(service.to_string()),
            in_alert_since: None,
            message: format!("{service} is {status}, need {}", SctlState::Running),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::prevent_host_state_changes(),
            ],
        }
    }

    #[test]
    fn hbn_service_health_defers_only_rsyslog_starting() {
        check_values(
            [
                Check {
                    scenario: "running rsyslog reports success",
                    input: (RSYSLOG_SERVICE, SctlState::Running),
                    expect: (
                        vec![health_report::HealthProbeSuccess {
                            id: probe_ids::ServiceRunning.clone(),
                            target: Some(RSYSLOG_SERVICE.to_string()),
                        }],
                        vec![],
                    ),
                },
                Check {
                    scenario: "starting rsyslog is omitted",
                    input: (RSYSLOG_SERVICE, SctlState::Starting),
                    expect: (vec![], vec![]),
                },
                Check {
                    scenario: "failed rsyslog remains critical",
                    input: (RSYSLOG_SERVICE, SctlState::Fatal),
                    expect: (
                        vec![],
                        vec![expected_service_running_alert(
                            RSYSLOG_SERVICE,
                            SctlState::Fatal,
                        )],
                    ),
                },
                Check {
                    scenario: "another starting service remains critical",
                    input: ("frr", SctlState::Starting),
                    expect: (
                        vec![],
                        vec![expected_service_running_alert("frr", SctlState::Starting)],
                    ),
                },
            ],
            |(service, status)| {
                let mut report = health_report::HealthReport::empty(
                    health_report::HealthReport::DPU_AGENT_SOURCE.to_string(),
                );
                record_hbn_service_health(&mut report, service, status);
                (report.successes, report.alerts)
            },
        );
    }

    #[test]
    fn post_config_wait_alert_tracks_wait_signal() {
        check_values(
            [
                Check {
                    scenario: "wait signal is clear",
                    input: false,
                    expect: vec![],
                },
                Check {
                    scenario: "wait signal adds one critical alert",
                    input: true,
                    expect: vec![health_report::HealthProbeAlert {
                        id: probe_ids::PostConfigCheckWait.clone(),
                        target: None,
                        in_alert_since: None,
                        message: "Post-config waiting period".to_string(),
                        tenant_message: None,
                        classifications: vec![
                            health_report::HealthAlertClassification::prevent_allocations(),
                            health_report::HealthAlertClassification::prevent_host_state_changes(),
                        ],
                    }],
                },
            ],
            |should_wait_for_post_config_check| {
                let mut report = health_report::HealthReport::empty("forge-dpu-agent".to_string());
                add_post_config_wait_alert(&mut report, should_wait_for_post_config_check);
                report.alerts
            },
        );
    }

    /// Builds the expected `DiskUtilization` for one `df -HP` row, in the column
    /// order the output lists them: device, size, used, available, then the
    /// utilization percentage.
    fn disk(
        device: &str,
        size: &str,
        used: &str,
        available: &str,
        utilization: u32,
    ) -> DiskUtilization {
        DiskUtilization {
            device: device.to_string(),
            size: size.to_string(),
            used: used.to_string(),
            available: available.to_string(),
            utilization,
        }
    }

    #[test]
    fn test_parse_disk_utilization() {
        let parsed = super::parse_disk_utilization(DISKUTIL_OUT).unwrap();
        // Every mountpoint in the fixture is accounted for, so a stray or missing
        // row is caught alongside the per-row field checks below.
        assert_eq!(parsed.utilizations.len(), 10);

        check_values(
            [
                Check {
                    scenario: "/dev/shm",
                    input: "/dev/shm",
                    expect: Some(disk("tmpfs", "17G", "4.1k", "17G", 1)),
                },
                Check {
                    scenario: "/run",
                    input: "/run",
                    expect: Some(disk("tmpfs", "6.8G", "17M", "6.7G", 1)),
                },
                Check {
                    scenario: "/run/lock",
                    input: "/run/lock",
                    expect: Some(disk("tmpfs", "5.3M", "8.2k", "5.3M", 1)),
                },
                Check {
                    scenario: "rootfs",
                    input: "/",
                    expect: Some(disk("/dev/mmcblk0p2", "41G", "12G", "27G", 31)),
                },
                Check {
                    scenario: "/boot/efi",
                    input: "/boot/efi",
                    expect: Some(disk("/dev/mmcblk0p1", "52M", "9.0M", "43M", 18)),
                },
                Check {
                    scenario: "containerd sandbox shm",
                    input: "/run/containerd/io.containerd.grpc.v1.cri/sandboxes/3c9db06a2021f14b6cb98d0583ae43909f282c6d670dd9da071bddf333caaa4e/shm",
                    expect: Some(disk("shm", "68M", "0", "68M", 0)),
                },
                Check {
                    scenario: "containerd task rootfs",
                    input: "/run/containerd/io.containerd.runtime.v2.task/k8s.io/3c9db06a2021f14b6cb98d0583ae43909f282c6d670dd9da071bddf333caaa4e/rootfs",
                    expect: Some(disk("overlay", "41G", "12G", "27G", 31)),
                },
                Check {
                    scenario: "second containerd sandbox shm",
                    input: "/run/containerd/io.containerd.grpc.v1.cri/sandboxes/5e38cefc8507fcf3b872fa12f21bdbcc09244832c24a34871ef9d8d519fa37b9/shm",
                    expect: Some(disk("shm", "68M", "8.2k", "68M", 1)),
                },
                Check {
                    scenario: "second containerd task rootfs",
                    input: "/run/containerd/io.containerd.runtime.v2.task/k8s.io/5e38cefc8507fcf3b872fa12f21bdbcc09244832c24a34871ef9d8d519fa37b9/rootfs",
                    expect: Some(disk("overlay", "41G", "12G", "27G", 31)),
                },
                Check {
                    scenario: "/run/user/1002",
                    input: "/run/user/1002",
                    expect: Some(disk("tmpfs", "3.4G", "0", "3.4G", 0)),
                },
                Check {
                    scenario: "unlisted mountpoint is absent",
                    input: "/nope",
                    expect: None,
                },
            ],
            |mount_point| parsed.utilizations.get(mount_point).cloned(),
        );
    }

    #[test]
    fn test_parse_supervisorctl_status() {
        let st = parse_status(SUPERVISORCTL_STATUS_OUT).unwrap();
        check_values(
            [
                Check {
                    scenario: "running service",
                    input: "frr",
                    expect: SctlState::Running,
                },
                Check {
                    scenario: "exited service",
                    input: "ifreload",
                    expect: SctlState::Exited,
                },
                Check {
                    scenario: "stopped service",
                    input: "nvued",
                    expect: SctlState::Stopped,
                },
                Check {
                    scenario: "absent service reports Unknown",
                    input: "not-a-service",
                    expect: SctlState::Unknown,
                },
            ],
            |service| st.status_of(service),
        );
    }

    #[test]
    fn test_parse_mlxprivhost() {
        scenarios!(run = |s| parse_mlxprivhost(s).map_err(|e| e.to_string());
            "well-formed output yields the level" {
                MLXPRIVHOST_OUT => Yields("RESTRICTED".to_string()),
            }

            "malformed output is rejected" {
                // No `level` line at all, and a level line without the `:` separator.
                "Host configurations\n-------------------\n" => Fails,
                "level RESTRICTED" => Fails,
            }
        );
    }

    #[test]
    fn test_sctl_state_from_str() {
        value_scenarios!(run = |s: &str| s.parse::<SctlState>().expect("from_str is infallible");
            "each supervisorctl state name maps to its variant" {
                "STOPPED" => SctlState::Stopped,
                "STARTING" => SctlState::Starting,
                "RUNNING" => SctlState::Running,
                "BACKOFF" => SctlState::Backoff,
                "STOPPING" => SctlState::Stopping,
                "EXITED" => SctlState::Exited,
                "FATAL" => SctlState::Fatal,
            }

            "an unrecognized state falls through to Unknown" {
                "UNKNOWN" => SctlState::Unknown,
                "wat" => SctlState::Unknown,
                "" => SctlState::Unknown,
            }
        );
    }

    #[test]
    fn test_sctl_state_display() {
        value_scenarios!(run = |state: SctlState| state.to_string();
            "every variant renders its supervisorctl name" {
                SctlState::Stopped => "STOPPED".to_string(),
                SctlState::Starting => "STARTING".to_string(),
                SctlState::Running => "RUNNING".to_string(),
                SctlState::Backoff => "BACKOFF".to_string(),
                SctlState::Stopping => "STOPPING".to_string(),
                SctlState::Exited => "EXITED".to_string(),
                SctlState::Fatal => "FATAL".to_string(),
                SctlState::Unknown => "UNKNOWN".to_string(),
            }
        );
    }
}
