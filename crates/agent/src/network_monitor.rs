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
use std::cmp::max;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use ::rpc::forge::{self as rpc};
use ::rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, ForgeTlsClient};
use carbide_uuid::machine::{DpuMachineId, MachineId};
use chrono::Utc;
use clap::ValueEnum;
use eyre::{Context, Result};
use futures::future::join_all;
use futures::{StreamExt, stream};
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use surge_ping::{Client, Config, PingIdentifier, PingSequence};
use tokio::sync::{mpsc, watch};
use tokio::task;
use tokio::time::{self, Duration, Instant};
use tonic::async_trait;

use crate::hbn;
use crate::instrumentation::NetworkMonitorMetricsState;

// @TODO: this should be able to be configured
const MAX_PINGS_PER_DPU: u32 = 5; // Number of pings for each DPU in each check cycle
const DPU_LIST_FETCH_INTERVAL: u64 = 30 * 60; // Interval in seconds for fetching DPU list from API

/// Structure to store peer DPU information
#[derive(Debug, Eq, PartialEq, Hash, Clone, Serialize)]
pub struct DpuInfo {
    pub id: DpuMachineId,
    pub ip: IpAddr,
}

impl fmt::Display for DpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DpuInfo {{ id: {}, ip: {} }}", self.id, self.ip)
    }
}

impl TryFrom<rpc::DpuInfo> for DpuInfo {
    type Error = eyre::Error;

    fn try_from(rpc_info: rpc::DpuInfo) -> Result<Self, Self::Error> {
        let ip = IpAddr::from_str(&rpc_info.loopback_ip)?;
        // Note: DpuInfo uses a string for machine_id, not a real MachineId, which is wrong.
        Ok(DpuInfo {
            id: rpc_info.id.parse()?,
            ip,
        })
    }
}

/// Structure to store ping results for one DPU in one cycle
pub struct DpuPingResult {
    pub dpu_info: DpuInfo,
    pub success_count: u32, // Number of successful pings, <= MAX_PINGS_PER_DPU
    pub average_latency: Option<Duration>, // None if ping not successful, i.e. success_count = 0
}

impl DpuPingResult {
    pub fn loss_percent(&self) -> f64 {
        let max_pings = MAX_PINGS_PER_DPU as f64;
        (max_pings - (self.success_count as f64)) / max_pings
    }

    pub fn reachable(&self) -> bool {
        self.success_count > 0
    }
}

/// Network monitor struct handles network connectivity checks
pub struct NetworkMonitor {
    machine_id: DpuMachineId,                         // DPU id
    metrics: Option<Arc<NetworkMonitorMetricsState>>, // Metrics for monitoring
    pinger: Arc<dyn Ping>,                            // Pinger that help ping DPUs and get results
}

impl NetworkMonitor {
    pub fn new(
        machine_id: DpuMachineId,
        metrics: Option<Arc<NetworkMonitorMetricsState>>,
        pinger: Arc<dyn Ping>,
    ) -> Self {
        Self {
            machine_id,
            metrics,
            pinger,
        }
    }

    /// Runs in a loop to check network connection with peer DPUs and
    /// fetch updated peer dpus from API
    pub async fn run(
        &mut self,
        forge_api: &str,
        client_config: Arc<ForgeClientConfig>,
        close_receiver: &mut watch::Receiver<bool>,
    ) {
        // Initial fetch peer dpu list from API
        let mut peer_dpus = Vec::new();
        let mut loopback_ip: Option<IpAddr> = None;

        match self
            .find_all_dpu_info(&self.machine_id, forge_api, &client_config)
            .await
        {
            Ok((dpu_info, new_peer_dpus)) => {
                peer_dpus = new_peer_dpus;
                loopback_ip = Some(dpu_info.ip);
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "Network monitor failed to get dpu info list from API"
                );
            }
        }

        let mut peer_dpus_fetch_interval =
            tokio::time::interval(Duration::from_secs(DPU_LIST_FETCH_INTERVAL));
        let mut next_monitor_time = Instant::now();

        loop {
            tokio::select! {
                _ = close_receiver.changed() => {
                    tracing::info!("Network monitor stopped");
                    break;
                }
                _ = peer_dpus_fetch_interval.tick() => {
                    match self.find_all_dpu_info(&self.machine_id, forge_api, &client_config).await {
                        Ok((dpu_info, new_peer_dpus)) => {
                            peer_dpus = new_peer_dpus;
                            loopback_ip = Some(dpu_info.ip);
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                "Network monitor failed to get dpu info list from API"
                            );
                            peer_dpus = Vec::new();
                            loopback_ip = None;
                        }
                    }
                }
                _ = time::sleep_until(next_monitor_time) => {
                    // Run the monitoring task and dynamically adjust the interval
                    let elapsed_time = self.run_monitor(&peer_dpus, loopback_ip).await;
                    let interval = self.set_loop_interval(&elapsed_time);
                    next_monitor_time = Instant::now() + interval;
                }
            }
        }
    }

    /// Run network monitor for all peer_dpus, export results as metrics
    /// Returns total time elapsed to complete
    pub async fn run_monitor(
        &mut self,
        peer_dpus: &Vec<DpuInfo>,
        loopback_ip: Option<IpAddr>,
    ) -> Duration {
        let mut elapsed_time = Duration::from_secs(0);
        if let (Some(ip), false) = (loopback_ip, peer_dpus.is_empty()) {
            let start_time = Instant::now();
            match self.monitor_concurrent(peer_dpus, ip).await {
                Ok(results) => {
                    // Export metrics for the results
                    if let Some(metrics) = self.metrics.clone() {
                        let mut reachable_map = HashMap::new();
                        for result in results {
                            reachable_map.insert(result.dpu_info.id, result.reachable());
                            if let Some(latency) = result.average_latency {
                                metrics.record_network_latency(
                                    latency,
                                    self.machine_id,
                                    result.dpu_info.id,
                                );
                                metrics.record_network_loss_percent(
                                    result.loss_percent(),
                                    self.machine_id,
                                    result.dpu_info.id,
                                );
                            }
                        }
                        metrics.update_network_reachable_map(reachable_map);
                    }
                }
                Err(e) => tracing::error!(error = %e, "Failed to run network check"),
            }
            elapsed_time = start_time.elapsed();
        }

        elapsed_time
    }

    /// Adjust loop period based on check duration, cap to next multiple of 30 seconds
    pub fn set_loop_interval(&self, elapsed_time: &Duration) -> Duration {
        Duration::from_secs(max(elapsed_time.as_secs().div_ceil(30) * 30, 30))
    }

    /// Handle one time network check request from commandline
    /// Fetches new list from
    pub async fn run_onetime(&mut self, forge_api: &str, client_config: &ForgeClientConfig) {
        let (loopback_ip, peer_dpus) = match self
            .find_all_dpu_info(&self.machine_id, forge_api, client_config)
            .await
        {
            Ok((dpu_info, new_peer_dpus)) => (dpu_info.ip, new_peer_dpus),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Network monitor failed to get dpu info list from API"
                );
                return;
            }
        };

        match self.monitor_concurrent(&peer_dpus, loopback_ip).await {
            Ok(results) => self.format_results(&results, loopback_ip.to_string()),
            Err(e) => tracing::error!(error = %e, "Failed to run network check"),
        }
    }

    /// Concurrently ping and record result for monitoring network status to peer DPUs
    /// Use a channel to handle recording ping results from concurrent ping tasks
    pub async fn monitor_concurrent(
        &self,
        peer_dpus: &Vec<DpuInfo>,
        loopback_ip: IpAddr,
    ) -> Result<Vec<DpuPingResult>, eyre::Report> {
        let concurrent_limit = 20; // Important for not overwhelming hbn container exec

        let (tx, mut rx) = mpsc::channel(100);

        // Use a channel to collect ping results from concurrent tasks
        let recv_task = task::spawn(async move {
            let mut results = Vec::new();
            while let Some(result) = rx.recv().await {
                results.push(result);
            }
            results
        });

        // Concurrent jobs to ping DPUs and get results
        stream::iter(peer_dpus)
            .for_each_concurrent(concurrent_limit, |peer_dpu| {
                let peer_dpu_id = peer_dpu.id;
                let tx_clone = tx.clone();
                async move {
                    match self.pinger.ping_dpu(peer_dpu.clone(), loopback_ip).await {
                        Ok(ping_result) => {
                            // Send result to the channel
                            if (tx_clone.send(ping_result).await).is_err() {
                                self.record_error_metrics(
                                    NetworkMonitorError::ResultRecordChannelSendError,
                                    None,
                                );
                            }
                        }
                        Err((error_type, _report)) => {
                            self.record_error_metrics(error_type, Some(peer_dpu_id));
                        }
                    }
                }
            })
            .await;

        drop(tx);

        // Get results from the the channel receiving side task
        let results = recv_task.await.map_err(|err| {
            self.record_error_metrics(NetworkMonitorError::TaskJoinError, None);
            tracing::error!(
                error = %err,
                "Failed to join task spawned for collecting ping result"
            );
            err
        })?;

        Ok(results)
    }

    /// Format check results into JSON format
    /// Average latency outputed as seconds
    fn format_results(&self, results: &[DpuPingResult], loopback_ip: String) {
        let mut formatted_results: Vec<_> = results
            .iter()
            .map(|result| {
                let mut json_result = json!({
                    "peer_dpu_id": result.dpu_info.id.clone(),
                    "loopback_ip": result.dpu_info.ip.clone(),
                    "reachable": result.reachable(),
                    "loss_percent": result.loss_percent(),
                });

                if let Some(latency) = result.average_latency {
                    json_result["average_latency"] = json!(latency.as_secs_f64());
                } else {
                    json_result["average_latency"] = json!("N/A".to_string());
                }
                json_result
            })
            .collect();

        // Sort the result based on peer_dpu_id lexicographically
        formatted_results.sort_by(|a, b| a["peer_dpu_id"].as_str().cmp(&b["peer_dpu_id"].as_str()));

        let final_result = json!({
            "dpu_id": self.machine_id,
            "loopback_ip": if loopback_ip.is_empty() { "Unknown".to_string() } else { loopback_ip} ,
            "results": formatted_results,
            "timestamp": Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        });

        match serde_json::to_string_pretty(&final_result) {
            Ok(json) => println!("{json}"),
            Err(e) => tracing::error!(error = %e, "Failed to serialize results to JSON"),
        }
    }

    /// Finds id and loopback IP of this DPU and list of peer dpus using gRPC call to API
    pub async fn find_all_dpu_info(
        &self,
        dpu_machine_id: &MachineId,
        forge_api: &str,
        client_config: &ForgeClientConfig,
    ) -> Result<(DpuInfo, Vec<DpuInfo>), eyre::Report> {
        // Get list of DPU information from API
        let dpu_info_list = fetch_dpu_info_list(forge_api, client_config)
            .await
            .inspect_err(|_| {
                self.record_error_metrics(NetworkMonitorError::ApiRpcCallError, None);
            })?;

        // Get this DPU information and list of peer DPU information
        let mut dpu_info: Option<DpuInfo> = None;
        let mut peer_dpus: Vec<DpuInfo> = Vec::new();
        for dpu in dpu_info_list.dpu_list {
            if dpu.id == dpu_machine_id.to_string() {
                dpu_info = Some(dpu.clone().try_into().inspect_err(|_| {
                    self.record_error_metrics(NetworkMonitorError::DpuNotFound, None);
                })?);
            } else if let Ok(peer_dpu) = dpu.try_into() {
                peer_dpus.push(peer_dpu);
            }
        }

        // Must be able to find information about this DPU
        let dpu_info = dpu_info.ok_or_else(|| {
            self.record_error_metrics(NetworkMonitorError::DpuNotFound, None);
            eyre::eyre!("DPU with id {} not found", dpu_machine_id)
        })?;

        Ok((dpu_info, peer_dpus))
    }

    /// Helper function for recording different types of error metrics
    fn record_error_metrics(
        &self,
        error_type: NetworkMonitorError,
        dest_dpu_id: Option<DpuMachineId>,
    ) {
        if let Some(metrics) = &self.metrics.clone() {
            match dest_dpu_id {
                Some(dest_dpu_id) => metrics.record_communication_error(
                    self.machine_id,
                    dest_dpu_id,
                    error_type.to_string(),
                ),
                None => metrics.record_monitor_error(self.machine_id, error_type.to_string()),
            };
        }
    }
}

/// Fetches the list of DPU information from the API
pub(crate) async fn fetch_dpu_info_list(
    forge_api: &str,
    client_config: &ForgeClientConfig,
) -> Result<rpc::GetDpuInfoListResponse, eyre::Report> {
    let api_config = ApiConfig::new(forge_api, client_config);
    let mut client = ForgeTlsClient::retry_build(&api_config)
        .await
        .map_err(|err| {
            eyre::Report::new(err).wrap_err(format!(
                "could not connect to forge API server at {forge_api}"
            ))
        })?;

    let request = tonic::Request::new(rpc::GetDpuInfoListRequest {});
    let response: tonic::Response<rpc::GetDpuInfoListResponse> =
        client.get_dpu_info_list(request).await.map_err(|err| {
            eyre::Report::new(err)
                .wrap_err(format!("forge_api: {forge_api}"))
                .wrap_err("error while executing the GetDpuInfoList gRPC call")
        })?;

    Ok(response.into_inner())
}

#[async_trait]
pub trait Ping: Send + Sync {
    /// Ping a DPU and return the ping result
    async fn ping_dpu(
        &self,
        dpu_info: DpuInfo,
        loopback_ip: IpAddr,
    ) -> Result<DpuPingResult, (NetworkMonitorError, eyre::Report)>;
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum NetworkPingerType {
    HbnExec,
    OobNetBind,
}

impl fmt::Display for NetworkPingerType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone)]
pub struct ParseNetworkPingerTypeError;

impl TryFrom<i32> for NetworkPingerType {
    type Error = ParseNetworkPingerTypeError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(NetworkPingerType::HbnExec),
            1 => Ok(NetworkPingerType::OobNetBind),
            _ => Err(ParseNetworkPingerTypeError),
        }
    }
}

impl FromStr for NetworkPingerType {
    type Err = ParseNetworkPingerTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "HbnExec" => Ok(NetworkPingerType::HbnExec),
            "OobNetBind" => Ok(NetworkPingerType::OobNetBind),
            _ => Err(ParseNetworkPingerTypeError),
        }
    }
}

impl From<NetworkPingerType> for Arc<dyn Ping> {
    fn from(ping_type: NetworkPingerType) -> Self {
        match ping_type {
            NetworkPingerType::HbnExec => Arc::new(HbnExecPinger),
            NetworkPingerType::OobNetBind => Arc::new(OobNetBindPinger),
        }
    }
}

/// Pinger that binds to the oob_net0 interface
pub struct OobNetBindPinger;

#[async_trait]
impl Ping for OobNetBindPinger {
    /// Pings a dpu from oob_net0 interface
    ///
    /// # Parameters
    /// - `dpu_info`: the peer dpu that is pinged
    /// - `_interface`: not used
    ///
    /// # Returns
    /// - `Ok(DpuPingResult)`: If is successful or if all pings fail with a timeout but no other errors.
    /// - `Err(eyre::Report)`: If fails with an unexpected error.
    async fn ping_dpu(
        &self,
        dpu_info: DpuInfo,
        _loopback_ip: IpAddr,
    ) -> Result<DpuPingResult, (NetworkMonitorError, eyre::Report)> {
        let interface = "oob_net0";
        let config = Config::builder().interface(interface).build();
        let client = Client::new(&config).map_err(|e| {
            let error_message = format!("Unable to build pinger with interface {interface}: {e}");
            (
                NetworkMonitorError::PingInterfaceError,
                eyre::eyre!(error_message),
            )
        })?;

        // For each IP, ping MAX_PINGS_PER_DPU times
        let ping_futures = (0..MAX_PINGS_PER_DPU)
            .map(|seq_num| {
                let client_clone = client.clone();
                let ip_inner = dpu_info.ip;
                task::spawn(async move {
                    let mut pinger = client_clone
                        .pinger(ip_inner, PingIdentifier(rand::random()))
                        .await;
                    // Set each ping to have timeout of 1 second
                    pinger.timeout(Duration::from_secs(1));
                    pinger
                        .ping(PingSequence(seq_num.try_into().unwrap()), &[])
                        .await
                })
            })
            .collect::<Vec<_>>();

        // Get averaged result over all pings
        let results = join_all(ping_futures).await;
        let mut total_duration = Duration::new(0, 0);
        let mut success_count = 0;
        for (_packet, duration) in results.into_iter().flatten().flatten() {
            total_duration += duration;
            success_count += 1;
        }

        let average_latency = (success_count > 0).then(|| total_duration / success_count);

        let ping_result: DpuPingResult = DpuPingResult {
            dpu_info,
            success_count,
            average_latency,
        };

        Ok(ping_result)
    }
}

/// Pinger that uses crictl to execute ping command inside HBN container
/// from the loopback interface.
pub struct HbnExecPinger;
#[async_trait]
impl Ping for HbnExecPinger {
    /// Pings a dpu from loopback interface inside HBN container.
    ///
    /// # Parameters
    /// - `dpu_info`: the peer dpu that is pinged
    /// - `interface`: IP address of loopback interface of HBN container that we are pinging from
    ///
    /// # Returns
    /// - `Ok(DpuPingResult)`: If is successful or if all pings fail with a timeout but no other errors.
    /// - `Err(eyre::Report)`: If fails with an unexpected error.
    async fn ping_dpu(
        &self,
        dpu_info: DpuInfo,
        loopback_ip: IpAddr,
    ) -> Result<DpuPingResult, (NetworkMonitorError, eyre::Report)> {
        let container_id: String = hbn::get_hbn_container_id()
            .await
            .wrap_err("failed to get hbn container id")
            .map_err(|e| (NetworkMonitorError::HbnContainerIdNotFound, e))?;

        match hbn::run_in_container(
            &container_id,
            &[
                "ping",
                "-W",
                "1",
                "-c",
                &MAX_PINGS_PER_DPU.to_string(),
                "-I",
                &loopback_ip.to_string(),
                &dpu_info.ip.to_string(),
            ],
            true,
        )
        .await
        {
            Ok(stdout) => parse_ping_stdout(dpu_info, &stdout)
                .map_err(|e| (NetworkMonitorError::PingOutputParseError, e)),
            Err(err) => {
                // Ping fail could be 100% loss or error, 100% loss is treated as unreachable but not error
                let err_string = format!("{err}");
                let err_re = Regex::new(
                    r"(?s)cmd \'(.+)\' failed with status: (.+), stderr: (.+), stdout: (.+)",
                )
                .map_err(|regex_err| {
                    (
                        NetworkMonitorError::PingOutputParseError,
                        eyre::eyre!(
                            "unexpected parse error for container ping result: {}",
                            regex_err.to_string()
                        ),
                    )
                })?;

                let stdout = err_re
                    .captures(&err_string)
                    .and_then(|caps| caps.get(4).map(|m| m.as_str()))
                    .ok_or_else(|| {
                        (
                            NetworkMonitorError::HbnContainerCommandExecError,
                            eyre::eyre!("error running ping in container: {}", err),
                        )
                    })?;

                parse_ping_stdout(dpu_info, stdout)
                    .map_err(|e| (NetworkMonitorError::PingOutputParseError, e))
            }
        }
    }
}

/// Parse ping standard output to valid dpu ping result,
/// including number of successful pings and average latency.
pub fn parse_ping_stdout(dpu_info: DpuInfo, stdout: &str) -> Result<DpuPingResult, eyre::Report> {
    let summary_re = Regex::new(
        r"(\d+) packets transmitted, (\d+) received,(?:\s*\+\d+ errors,)? (\d+)% packet loss",
    )?;
    let rtt_re = Regex::new(r"rtt min/avg/max/mdev = [\d\.]+/([\d\.]+)/[\d\.]+/[\d\.]+ ms")?;

    let success_count = summary_re
        .captures(stdout)
        .and_then(|caps| caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok()))
        .ok_or_else(|| eyre::eyre!("failed to parse number of success packets"))?;

    if success_count == 0 {
        return Ok(DpuPingResult {
            dpu_info,
            success_count,
            average_latency: None,
        });
    }

    let latency = rtt_re
        .captures(stdout)
        .and_then(|caps| caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()))
        .ok_or_else(|| eyre::eyre!("failed to average latency"))?;

    Ok(DpuPingResult {
        dpu_info,
        success_count,
        average_latency: Some(Duration::from_secs_f64(latency / 1000.0)),
    })
}

#[cfg(test)]
mod parse_ping_stdout_tests {
    use std::time::Duration;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::{DpuInfo, parse_ping_stdout};

    const DPU_ID: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";

    #[test]
    fn parses_ping_results() {
        scenarios!(run = |stdout| {
            let result = parse_ping_stdout(
                DpuInfo {
                    id: DPU_ID.parse().expect("valid machine id"),
                    ip: "172.20.0.200".parse().expect("valid IP address"),
                },
                stdout,
            )
            .map_err(|_| ())?;

            Ok::<_, ()>((result.success_count, result.average_latency))
        };
            "successful ping" {
                "5 packets transmitted, 5 received, 0% packet loss, time 4006ms\n\
                 rtt min/avg/max/mdev = 0.084/0.102/0.127/0.014 ms" =>
                    Yields((5, Some(Duration::from_secs_f64(0.102 / 1000.0)))),
            }

            "complete packet loss without RTT line" {
                "PING 172.20.0.200 (172.20.0.200) 56(84) bytes of data.\n\n\
                 --- 172.20.0.200 ping statistics ---\n\
                 5 packets transmitted, 0 received, 100% packet loss, time 4092ms" =>
                    Yields((0, None)),
            }

            "complete packet loss with errors field" {
                "PING 172.20.0.200 (172.20.0.200) 56(84) bytes of data.\n\n\
                 --- 172.20.0.200 ping statistics ---\n\
                 5 packets transmitted, 0 received, +5 errors, 100% packet loss, time 4092ms" =>
                    Yields((0, None)),
            }

            "missing packet summary" {
                "PING 172.20.0.200 (172.20.0.200) 56(84) bytes of data." => Fails,
            }
        );
    }
}

#[derive(Debug)]
pub enum NetworkMonitorError {
    ApiRpcCallError,
    DpuNotFound,
    HbnContainerIdNotFound,
    HbnContainerCommandExecError,
    PingError,
    PingInterfaceError,
    PingOutputParseError,
    ResultRecordChannelSendError,
    TaskJoinError,
    UnknownError,
}

impl fmt::Display for NetworkMonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
