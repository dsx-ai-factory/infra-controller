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

//! Debug Bundle Module
//!
//! This module contains all functionality related to creating debug bundles
//! for troubleshooting managed hosts and Carbide API issues.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Formatter;
use std::fs::File;
use std::io::Write;
use std::str::FromStr;

use ::rpc::forge::BmcEndpointRequest;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zip::CompressionMethod;
use zip::write::{FileOptions, ZipWriter};

use crate::errors::CarbideCliError::InvalidDateTimeFromUserInput;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::managed_host::DebugBundle;
use crate::rpc::ApiClient;

const MAX_BATCH_SIZE: u32 = 5000;
const CARBIDE_API_CONTAINER_NAME: &str = "carbide-api";
const K8S_CONTAINER_NAME_LABEL: &str = "k8s_container_name";

// 🔗 Grafana link generation
#[derive(Serialize)]
struct GrafanaConfig {
    datasource: String,
    queries: Vec<GrafanaQuery>,
    range: GrafanaTimeRange,
}

#[derive(Serialize)]
struct GrafanaQuery {
    expr: String,
    #[serde(rename = "refId")]
    ref_id: String,
}

#[derive(Serialize)]
struct GrafanaTimeRange {
    from: String,
    to: String,
}

// LogType enum for log categorization
#[derive(Debug, Clone, Copy)]
enum LogType {
    CarbideApi,
    HostSpecific,
    DpuAgent,
}

impl LogType {
    fn batch_label(&self, batch_number: usize) -> String {
        match self {
            LogType::CarbideApi => format!("Carbide-API Batch {batch_number}"),
            LogType::HostSpecific => format!("Host Batch {batch_number}"),
            LogType::DpuAgent => format!("DPU-Agent Batch {batch_number}"),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            LogType::CarbideApi => "carbide-api",
            LogType::HostSpecific => "host-specific",
            LogType::DpuAgent => "dpu-agent",
        }
    }
}

// TimeRange struct to group related time parameters
#[derive(Debug, Copy, Clone)]
struct TimeRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    utc: bool,
}

impl TimeRange {
    fn to_grafana_format(self) -> (i64, i64) {
        (self.start.timestamp_millis(), self.end.timestamp_millis())
    }

    fn with_new_start_time(self, new_start_time: DateTime<Utc>) -> Self {
        let orig_duration = self.end - self.start;
        Self {
            start: new_start_time,
            end: new_start_time + orig_duration,
            utc: self.utc,
        }
    }

    fn display_start(&self) -> DisplayDateTime {
        DisplayDateTime {
            date_time: self.start,
            utc: self.utc,
        }
    }

    // function to format end display timestamp
    fn format_end_display(&self, end_ms: i64) -> String {
        format!(
            "{} ({})",
            DisplayDateTime {
                date_time: self.end,
                utc: self.utc,
            },
            end_ms
        )
    }
}

// LogBatch struct for batch management
#[derive(Debug)]
struct LogBatch {
    batch_number: usize,
    log_type: LogType,
    time_range: TimeRange,
    grafana_link: Option<String>,
}

impl LogBatch {
    fn new(batch_number: usize, log_type: LogType, time_range: TimeRange) -> Self {
        Self {
            batch_number,
            log_type,
            time_range,
            grafana_link: None,
        }
    }

    fn set_grafana_link(
        &mut self,
        grafana_base_url: &str,
        loki_uid: &str,
        expr: &str,
    ) -> CarbideCliResult<()> {
        let (start_ms, end_ms) = self.time_range.to_grafana_format();
        let link = generate_grafana_link(grafana_base_url, loki_uid, expr, start_ms, end_ms)?;
        self.grafana_link = Some(link);
        Ok(())
    }

    fn label(&self) -> String {
        self.log_type.batch_label(self.batch_number)
    }

    fn needs_pagination(batch_count: usize, batch_size: u32) -> bool {
        batch_count >= batch_size as usize
    }

    fn next_time_range(
        previous_time_range: TimeRange,
        previous_entry_count: usize,
        newest_timestamp: Option<i64>,
        batch_size: u32,
    ) -> CarbideCliResult<Option<TimeRange>> {
        if let Some(next_start_time) =
            handle_pagination(previous_entry_count, newest_timestamp, batch_size as usize)?
        {
            Ok(Some(
                previous_time_range.with_new_start_time(next_start_time),
            ))
        } else {
            Ok(None)
        }
    }
}

// LogCollector struct to encapsulate state and behavior
#[derive(Debug)]
struct LogCollector<'a> {
    grafana_base_url: Cow<'a, str>,
    loki_uid: Cow<'a, str>,
    unique_log_ids: HashSet<String>,
    all_entries: Vec<LogEntry>,
    batch_size: u32,
    batch_links: Vec<(String, String, usize, String)>, // (batch_label, grafana_link, log_count, time_range_display)
    grafana_client: GrafanaClient<'a>,                 // Reuse client across batches
}

struct DisplayDateTime {
    date_time: DateTime<Utc>,
    utc: bool,
}

impl std::fmt::Display for DisplayDateTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let fmt = "%Y-%m-%d %H:%M:%S%Z";
        let s = if self.utc {
            self.date_time.format(fmt)
        } else {
            self.date_time.with_timezone(&Local).format(fmt)
        };
        write!(f, "{s}")
    }
}

impl<'a> LogCollector<'a> {
    fn new(
        grafana_base_url: Cow<'a, str>,
        loki_uid: Cow<'a, str>,
        batch_size: u32,
    ) -> CarbideCliResult<Self> {
        // Validate and cap batch size
        let capped_batch_size = batch_size.min(MAX_BATCH_SIZE);
        if batch_size > MAX_BATCH_SIZE {
            println!(
                "   WARNING: Batch size {batch_size} exceeds maximum {MAX_BATCH_SIZE}, using {capped_batch_size}"
            );
        }

        // Create GrafanaClient once and reuse
        let grafana_client = GrafanaClient::new(grafana_base_url.clone())?;

        Ok(Self {
            grafana_base_url,
            loki_uid,
            unique_log_ids: HashSet::new(),
            all_entries: Vec::new(),
            batch_size: capped_batch_size,
            batch_links: Vec::new(),
            grafana_client,
        })
    }

    async fn into_logs_and_batch_links(
        mut self,
        expr: &str,
        log_type: LogType,
        mut time_range: TimeRange,
    ) -> CarbideCliResult<(Vec<LogEntry>, Vec<(String, String, usize, String)>)> {
        let mut batch_number = 1;

        loop {
            let mut batch = LogBatch::new(batch_number, log_type, time_range);
            let (start_ms, end_ms) = batch.time_range.to_grafana_format();
            let end_display = batch.time_range.format_end_display(end_ms);

            println!(
                "   {}: Fetching logs from {} ({}) to {}",
                batch.label(),
                batch.time_range.display_start(),
                start_ms,
                end_display
            );

            let batch_result = self.process_batch(expr, start_ms, end_ms).await?;

            // Generate Grafana link for this batch
            batch.set_grafana_link(&self.grafana_base_url, &self.loki_uid, expr)?;

            // Store batch info with link and time range
            let batch_label = batch.label();
            let grafana_link = batch.grafana_link.unwrap_or_default();
            let log_count = batch_result.entries.len();
            let time_range_display = format!(
                "{} ({}) to {}",
                batch.time_range.start, start_ms, end_display
            );
            self.batch_links
                .push((batch_label, grafana_link, log_count, time_range_display));

            // Update collections for next batch
            self.unique_log_ids.extend(
                batch_result
                    .entries
                    .iter()
                    .map(|entry| entry.unique_id.clone()),
            );
            self.all_entries.extend(batch_result.entries);

            if !LogBatch::needs_pagination(batch_result.original_batch_count, self.batch_size) {
                break;
            }

            if let Some(next_time_range) = LogBatch::next_time_range(
                batch.time_range,
                log_count,
                batch_result.newest_timestamp,
                self.batch_size,
            )? {
                time_range = next_time_range;
                batch_number += 1;
            } else {
                break;
            }
        }

        self.finalize_and_validate_logs(&log_type)?;
        Ok((self.all_entries, self.batch_links))
    }

    async fn process_batch(
        &self,
        expr: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> CarbideCliResult<BatchResult> {
        let query_request =
            build_grafana_query_request(expr, start_ms, end_ms, &self.loki_uid, self.batch_size);

        // 2. Execute HTTP request using reusable function and stored client
        let response_body = execute_grafana_query(&query_request, &self.grafana_client).await?;

        // 3. Parse response using reusable function
        let (batch_entries, newest_timestamp) = parse_grafana_logs(response_body)?;

        let original_batch_count = batch_entries.len();
        let new_entries = remove_duplicates_from_end(batch_entries, &self.unique_log_ids);

        Ok(BatchResult {
            entries: new_entries,
            newest_timestamp,
            original_batch_count,
        })
    }

    fn finalize_and_validate_logs(&self, log_type: &LogType) -> CarbideCliResult<()> {
        let log_type_upper = log_type.as_str().to_uppercase();
        println!(
            "   TOTAL {} LOGS COLLECTED: {}",
            log_type_upper,
            self.all_entries.len()
        );

        let logs_count = self.all_entries.len();
        let unique_ids_count = self.unique_log_ids.len();

        if logs_count != unique_ids_count {
            println!(
                "   Validation FAILED for {}: {} logs but {} unique IDs (some logs missing unique IDs)",
                log_type.as_str(),
                logs_count,
                unique_ids_count
            );
            return Err(CarbideCliError::GenericError(format!(
                "Log validation failed for {}: {logs_count} logs but {unique_ids_count} unique IDs",
                log_type.as_str()
            )));
        }

        println!(
            "   Validation PASSED for {}: {} logs = {} unique IDs",
            log_type.as_str(),
            logs_count,
            unique_ids_count
        );
        Ok(())
    }
}

// GrafanaClient struct for API interactions
#[derive(Debug)]
struct GrafanaClient<'a> {
    client: reqwest::Client,
    base_url: Cow<'a, str>,
    auth_token: String,
}

impl<'a> GrafanaClient<'a> {
    fn new(grafana_url: Cow<'a, str>) -> CarbideCliResult<Self> {
        let auth_token = std::env::var("GRAFANA_AUTH_TOKEN")
            .map_err(|_| CarbideCliError::GenericError(
                "GRAFANA_AUTH_TOKEN environment variable not set. Please set it with your Grafana bearer token.".to_string()
            ))?;

        // Build HTTP client with optional proxy support from environment variables
        let mut client_builder = reqwest::Client::builder();

        // Check for proxy configuration in environment variables
        // Standard proxy env vars: HTTPS_PROXY, https_proxy, HTTP_PROXY, http_proxy
        if let Ok(proxy_url) = std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("http_proxy"))
        {
            println!("   Using proxy: {}", proxy_url);
            let proxy = reqwest::Proxy::all(&proxy_url).map_err(|e| {
                CarbideCliError::GenericError(format!("Failed to configure proxy: {}", e))
            })?;
            client_builder = client_builder.proxy(proxy);
        } else {
            println!("   No proxy configured - connecting directly");
        }

        let client = client_builder.build().map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to build HTTP client: {}", e))
        })?;

        Ok(Self {
            client,
            base_url: grafana_url,
            auth_token,
        })
    }

    async fn get_loki_datasource_uid(&self) -> CarbideCliResult<String> {
        println!(
            "   Fetching Loki datasource UID from Grafana: {}",
            self.base_url
        );

        let datasources_url = format!("{}/api/datasources/", self.base_url);

        let response = self
            .client
            .get(&datasources_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                println!("   Datasources API Response Status: {status}");

                if status.is_success() {
                    let datasources: Vec<GrafanaDatasource> = match resp.json().await {
                        Ok(data) => data,
                        Err(e) => {
                            return Err(CarbideCliError::GenericError(format!(
                                "Failed to parse datasources JSON: {e}"
                            )));
                        }
                    };

                    for ds in datasources {
                        if ds.datasource_type == "loki" {
                            println!("   Found Loki datasource: {} (UID: {})", ds.name, ds.uid);
                            return Ok(ds.uid);
                        }
                    }

                    Err(CarbideCliError::GenericError(
                        "Loki datasource not found in the response".to_string(),
                    ))
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    Err(CarbideCliError::GenericError(format!(
                        "HTTP Error {status}: {body}"
                    )))
                }
            }
            Err(e) => Err(CarbideCliError::GenericError(format!(
                "Failed to fetch datasources: {e}"
            ))),
        }
    }
}

// LogEntry struct for log entries
#[derive(Debug, Clone)]
struct LogEntry {
    message: String,
    timestamp_ms: i64,
    unique_id: String,
    nanosecond_timestamp: u64,
}

impl LogEntry {
    fn format_header(&self) -> String {
        format_timestamp_header(self.timestamp_ms)
    }

    fn is_duplicate(&self, existing_ids: &std::collections::HashSet<String>) -> bool {
        existing_ids.contains(&self.unique_id)
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct GrafanaResponse {
    results: GrafanaResults,
}

#[derive(Serialize, Deserialize, Debug)]
struct GrafanaResults {
    #[serde(rename = "A")]
    a: GrafanaFrameResult,
}

#[derive(Serialize, Deserialize, Debug)]
struct GrafanaFrameResult {
    status: u16,
    frames: Vec<GrafanaFrame>,
}

#[derive(Serialize, Deserialize, Debug)]
struct GrafanaFrame {
    data: GrafanaFrameData,
}

#[derive(Serialize, Deserialize, Debug)]
struct GrafanaFrameData {
    values: Vec<Vec<GrafanaValue>>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum GrafanaValue {
    Int(i64),       // For timestamps (values[1])
    String(String), // For log messages (values[2]) and nanosecond timestamps (values[3])
    Object(serde_json::Value),
}

// Strongly typed structs for Grafana query requests
#[derive(Serialize)]
struct GrafanaQueryRequest {
    queries: Vec<LokiQuery>,
    from: String,
    to: String,
    limit: u32,
}

#[derive(Serialize)]
struct LokiQuery {
    #[serde(rename = "refId")]
    ref_id: String,
    datasource: LokiDatasource,
    #[serde(rename = "queryType")]
    query_type: String,
    expr: String,
    #[serde(rename = "maxLines")]
    max_lines: u32,
}

#[derive(Serialize)]
struct LokiDatasource {
    #[serde(rename = "type")]
    datasource_type: String,
    uid: String,
}

// Grafana Datasource API Response Structs
#[derive(Deserialize, Debug)]
struct GrafanaDatasource {
    uid: String,
    name: String,
    #[serde(rename = "type")]
    datasource_type: String,
}

// Site Controller Details - Holds BMC endpoint exploration data
struct SiteControllerAnalysis {
    exploration_report: ::rpc::site_explorer::EndpointExplorationReport,
    credential_status: ::rpc::forge::BmcCredentialStatusResponse,
    bmc_ip: String,
    bmc_mac: Option<String>,
}

// Machine Info - Holds machine state machine data
struct MachineAnalysis {
    machine: ::rpc::forge::Machine,
    validation_results: Vec<::rpc::forge::MachineValidationResult>,
}

/// Helper function to get BMC IP and MAC address from machine_id
async fn get_bmc_ip_from_host_id(
    api_client: &ApiClient,
    host_id: &str,
) -> CarbideCliResult<(String, Option<String>)> {
    // Parse machine ID
    let machine_id = MachineId::from_str(host_id).map_err(|e| {
        CarbideCliError::GenericError(format!("Invalid machine ID '{}': {}", host_id, e))
    })?;

    // Get machine details from API
    let machine = api_client.get_machine(machine_id).await?;

    // Extract BMC info
    let bmc_info = machine.bmc_info.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "Machine {} does not have BMC info available",
            host_id
        ))
    })?;

    // Extract BMC IP (required)
    let bmc_ip = bmc_info.ip.ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "Machine {} does not have BMC IP address available",
            host_id
        ))
    })?;

    // Extract BMC MAC (optional)
    let bmc_mac = bmc_info.mac;

    Ok((bmc_ip, bmc_mac))
}

/// Fetch Site Controller Details (Redfish exploration + credentials)
async fn get_site_controller_analysis(
    api_client: &ApiClient,
    host_id: &str,
) -> CarbideCliResult<SiteControllerAnalysis> {
    println!("   Fetching BMC information for machine {}...", host_id);

    // Step 1: Get BMC IP and MAC from machine_id
    let (bmc_ip, bmc_mac) = get_bmc_ip_from_host_id(api_client, host_id).await?;

    println!("   BMC IP: {}", bmc_ip);
    if let Some(ref mac) = bmc_mac {
        println!("   BMC MAC: {}", mac);
    }

    // Parse MAC address if available
    let mac_address = if let Some(ref mac_str) = bmc_mac {
        use mac_address::MacAddress;
        Some(MacAddress::from_str(mac_str).map_err(|e| {
            CarbideCliError::GenericError(format!("Invalid MAC address '{}': {:?}", mac_str, e))
        })?)
    } else {
        None
    };

    println!("   Exploring BMC endpoint via Redfish...");

    // Step 2: Call Explore RPC (fetches Redfish data)
    let exploration_report = api_client
        .0
        .explore(BmcEndpointRequest {
            ip_address: bmc_ip.clone(),
            mac_address: mac_address.map(|m| m.to_string()),
        })
        .await?;

    println!("   Systems: {} found", exploration_report.systems.len());
    println!("   Managers: {} found", exploration_report.managers.len());
    println!("   Chassis: {} found", exploration_report.chassis.len());

    // Step 3: Call BmcCredentialStatus RPC
    let credential_status = api_client
        .0
        .bmc_credential_status(BmcEndpointRequest {
            ip_address: bmc_ip.clone(),
            mac_address: mac_address.map(|m| m.to_string()),
        })
        .await?;

    println!(
        "   Credentials: Available = {}",
        credential_status.have_credentials
    );

    Ok(SiteControllerAnalysis {
        exploration_report,
        credential_status,
        bmc_ip,
        bmc_mac,
    })
}

/// Fetch machine info (state machine information)
async fn get_machine_analysis(
    api_client: &ApiClient,
    machine_id: &MachineId,
) -> CarbideCliResult<MachineAnalysis> {
    println!("   Fetching machine state and metadata...");

    // Get machine details (state, SLA, controller outcome, reboot info, errors)
    let machine = api_client.get_machine(*machine_id).await?;
    println!("   Current State: {}", machine.state);

    // Get validation results
    println!("   Fetching validation test failures...");
    let validation_list = api_client
        .get_machine_validation_results(Some(*machine_id), true, None)
        .await?;

    // Filter: Keep ONLY failed tests (exit_code != 0)
    let failed_tests: Vec<_> = validation_list
        .results
        .into_iter()
        .filter(|test| test.exit_code != 0)
        .collect();

    println!(
        "   Validation Failures: {} failed tests found",
        failed_tests.len()
    );

    Ok(MachineAnalysis {
        machine,
        validation_results: failed_tests,
    })
}

/// Creates a comprehensive debug bundle for a specific machine.
///
/// This function collects diagnostic information from multiple sources and packages
/// them into a ZIP file for debugging and troubleshooting purposes.
///
/// # Data Collected
///
/// The debug bundle includes the following components:
///
/// 1. **Host-Specific Logs**: Machine-specific logs from Loki (filtered by `host_machine_id`)
/// 2. **Carbide-API Logs**: API server logs from Loki (filtered by `k8s_container_name`)
/// 3. **DPU Agent Logs**: DPU agent service logs from Loki (filtered by `systemd_unit` and `host_machine_id`)
/// 4. **Health Alerts**: Historical health alerts for the machine within the specified time range
/// 5. **Health Report Entries**: Current health report entries configured for the machine
/// 6. **Site Controller Details**: BMC/Redfish exploration data including:
///    - BMC IP and MAC addresses
///    - Systems, Managers, and Chassis information
///    - Firmware inventory
///    - Credential availability status
/// 7. **Machine Info**: State machine information including:
///    - Current state and state version
///    - SLA status and controller outcome
///    - Validation test failures
///    - Reboot history and failure details
/// 8. **Metadata**: Summary file with batch information and Grafana links
///
/// # Arguments
///
/// * `debug_bundle` - Configuration containing:
///   - `host_id`: The machine ID to collect data for
///   - `start_time`/`end_time`: Time range for log collection (HH:MM:SS format)
///   - `output_path`: Directory where the ZIP file will be created
///   - `site`: Site name (e.g., "dev3", "prod")
///   - `batch_size`: Maximum logs per batch (default: 5000)
///
/// * `api_client` - Authenticated API client for making RPC calls to Carbide API
///
/// # Output
///
/// Creates a ZIP file with the following structure:
/// - `host_logs_<machine_id>.txt` - Host-specific logs
/// - `carbide_api_logs.txt` - API server logs
/// - `dpu_agent_logs_<machine_id>.txt` - DPU agent service logs
/// - `health_alerts.json` - Health alerts history
/// - `health_alert_overrides.json` - Active health report entries
/// - `site_controller_details.json` - BMC/Redfish exploration data
/// - `machine_info.json` - Machine state and validation data
/// - `metadata.txt` - Summary and Grafana links
///
/// # Returns
///
/// Returns `Ok(())` on successful bundle creation, or a `CarbideCliError` if any step fails.
///
/// # Example
///
/// ```no_run
/// use crate::managed_host::DebugBundle;
/// use crate::rpc::ApiClient;
///
/// let bundle_config = DebugBundle {
///     host_id: "fm100ht...".to_string(),
///     start_time: "06:00:00".to_string(),
///     end_time: Some("06:10:00".to_string()),
///     utc: false,
///     output_path: "/tmp".to_string(),
///     grafana_url: Some("https://grafana.example.com".to_string()),
///     batch_size: 5000,
/// };
///
/// let api_client = ApiClient::new(config).await?;
/// handle_debug_bundle(bundle_config, &api_client).await?;
/// ```
pub(crate) async fn handle_debug_bundle(
    debug_bundle: DebugBundle,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    println!(
        "   Creating debug bundle for host: {}",
        debug_bundle.host_id
    );

    // Parse flexible date/time inputs
    let start = parse_datetime_input(&debug_bundle.start_time, debug_bundle.utc)?;

    // Handle optional end_time (default to "now")
    let end = if let Some(ref end_time_str) = debug_bundle.end_time {
        parse_datetime_input(end_time_str, debug_bundle.utc)?
    } else {
        // Use current time as default
        chrono::Utc::now()
    };

    // Create TimeRange struct with parsed values
    let time_range = TimeRange {
        start,
        end,
        utc: debug_bundle.utc,
    };

    // Conditionally collect logs based on --grafana-url presence
    let (
        host_logs,
        host_batch_links,
        carbide_api_logs,
        carbide_batch_links,
        dpu_agent_logs,
        dpu_batch_links,
        loki_uid,
    ) = if let Some(grafana_url) = &debug_bundle.grafana_url {
        // Use new GrafanaClient struct
        let grafana_client = GrafanaClient::new(Cow::Borrowed(grafana_url))?;

        println!("\nFetching Loki datasource UID...");
        let loki_uid = grafana_client.get_loki_datasource_uid().await?;

        println!("\nDownloading host-specific logs...");
        let (host_logs, host_batch_links) = get_host_logs(
            &debug_bundle.host_id,
            time_range,
            grafana_url,
            &loki_uid,
            debug_bundle.batch_size,
        )
        .await?;

        println!("\nDownloading carbide-api logs...");
        let (carbide_api_logs, carbide_batch_links) =
            get_carbide_api_logs(time_range, grafana_url, &loki_uid, debug_bundle.batch_size)
                .await?;

        println!("\nDownloading DPU agent logs...");
        let (dpu_agent_logs, dpu_batch_links) = get_dpu_agent_logs(
            &debug_bundle.host_id,
            time_range,
            grafana_url,
            &loki_uid,
            debug_bundle.batch_size,
        )
        .await?;

        (
            host_logs,
            host_batch_links,
            carbide_api_logs,
            carbide_batch_links,
            dpu_agent_logs,
            dpu_batch_links,
            Some(loki_uid),
        )
    } else {
        println!("\nSkipping log collection (--grafana-url not provided)");
        (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        )
    };

    println!("\nFetching health alerts...");
    let health_alerts = get_health_alerts(api_client, &debug_bundle.host_id, &time_range).await?;
    let alert_count = health_alerts
        .histories
        .get(&debug_bundle.host_id)
        .map(|h| h.records.len())
        .unwrap_or(0);
    println!("   Alerts: {} records collected", alert_count);

    println!("\nFetching health report entries...");
    let alert_entries = get_alert_entries(api_client, &debug_bundle.host_id).await?;
    println!(
        "   Entries: {} entries collected",
        alert_entries.health_report_entries.len()
    );

    println!("\nFetching site controller details...");
    let site_controller_analysis =
        get_site_controller_analysis(api_client, &debug_bundle.host_id).await?;

    // Machine Info
    println!("\nFetching machine info...");
    let machine_id = MachineId::from_str(&debug_bundle.host_id).map_err(|e| {
        CarbideCliError::GenericError(format!(
            "Invalid machine ID '{}': {}",
            debug_bundle.host_id, e
        ))
    })?;
    let machine_analysis = get_machine_analysis(api_client, &machine_id).await?;

    println!("\nDebug Bundle Summary:");
    println!("   Host Logs: {} logs collected", host_logs.len());
    println!(
        "   Carbide-API Logs: {} logs collected",
        carbide_api_logs.len()
    );
    println!("   DPU Agent Logs: {} logs collected", dpu_agent_logs.len());
    println!(
        "   Health Alerts: {} records",
        health_alerts
            .histories
            .get(&debug_bundle.host_id)
            .map(|h| h.records.len())
            .unwrap_or(0)
    );
    println!(
        "   Health Report Entries: {} entries",
        alert_entries.health_report_entries.len()
    );
    println!("   Site Controller Details: Collected");
    println!("   Machine State Information: Collected");
    println!(
        "   Total Logs: {}",
        host_logs.len() + carbide_api_logs.len() + dpu_agent_logs.len()
    );

    // Create ZIP file with logs, health alerts, health report entries, site controller details, and machine info
    println!("\nCreating ZIP file...");
    create_debug_bundle_zip(
        &debug_bundle,
        &host_logs,
        &carbide_api_logs,
        &dpu_agent_logs,
        &host_batch_links,
        &carbide_batch_links,
        &dpu_batch_links,
        loki_uid.as_deref(),
        &health_alerts,
        &alert_entries,
        &site_controller_analysis,
        &machine_analysis,
    )?;

    println!("\nDebug bundle creation completed!");

    Ok(())
}

async fn get_host_logs(
    host_id: &str,
    time_range: TimeRange,
    grafana_url: &str,
    loki_uid: &str,
    batch_size: u32,
) -> CarbideCliResult<(Vec<LogEntry>, Vec<(String, String, usize, String)>)> {
    let expr = format!("{{host_machine_id=\"{host_id}\"}} |= ``");
    let log_type = LogType::HostSpecific;

    // NEW() NOW RETURNS RESULT
    let collector = LogCollector::new(grafana_url.into(), loki_uid.into(), batch_size)?;
    collector
        .into_logs_and_batch_links(&expr, log_type, time_range)
        .await
}

async fn get_carbide_api_logs(
    time_range: TimeRange,
    grafana_url: &str,
    loki_uid: &str,
    batch_size: u32,
) -> CarbideCliResult<(Vec<LogEntry>, Vec<(String, String, usize, String)>)> {
    let expr = format!("{{{K8S_CONTAINER_NAME_LABEL}=\"{CARBIDE_API_CONTAINER_NAME}\"}} |= ``");
    let log_type = LogType::CarbideApi;

    // NEW() NOW RETURNS RESULT
    let collector = LogCollector::new(grafana_url.into(), loki_uid.into(), batch_size)?;
    collector
        .into_logs_and_batch_links(&expr, log_type, time_range)
        .await
}

async fn get_dpu_agent_logs(
    host_id: &str,
    time_range: TimeRange,
    grafana_url: &str,
    loki_uid: &str,
    batch_size: u32,
) -> CarbideCliResult<(Vec<LogEntry>, Vec<(String, String, usize, String)>)> {
    let expr = format!(
        "{{systemd_unit=\"forge-dpu-agent.service\", host_machine_id=\"{host_id}\"}} |= ``"
    );
    let log_type = LogType::DpuAgent;

    let collector = LogCollector::new(grafana_url.into(), loki_uid.into(), batch_size)?;
    collector
        .into_logs_and_batch_links(&expr, log_type, time_range)
        .await
}

/// Collect health alerts for a machine within a time range
async fn get_health_alerts(
    api_client: &ApiClient,
    host_id: &str,
    time_range: &TimeRange,
) -> CarbideCliResult<::rpc::forge::HealthHistories> {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;

    // Parse machine ID
    let machine_id = MachineId::from_str(host_id).map_err(|e| {
        CarbideCliError::GenericError(format!("Invalid machine ID '{}': {}", host_id, e))
    })?;

    // Get DateTime objects from TimeRange
    let start_dt = time_range.start;
    let end_dt = time_range.end;

    // Convert DateTime → Protobuf Timestamp (using rpc::Timestamp's From implementation)
    let start_time_proto: ::rpc::Timestamp = start_dt.into();
    let end_time_proto: ::rpc::Timestamp = end_dt.into();

    // Build request with time filtering
    let request = ::rpc::forge::MachineHealthHistoriesRequest {
        machine_ids: vec![machine_id],
        start_time: Some(start_time_proto),
        end_time: Some(end_time_proto),
    };

    // Call unified API with time filtering
    let response = api_client
        .0
        .find_machine_health_histories(request)
        .await
        .map_err(CarbideCliError::ApiInvocationError)?;

    Ok(response)
}

/// Collect health report entries for a machine (current state)
async fn get_alert_entries(
    api_client: &ApiClient,
    host_id: &str,
) -> CarbideCliResult<::rpc::forge::ListHealthReportResponse> {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;

    // Parse machine ID
    let machine_id = MachineId::from_str(host_id).map_err(|e| {
        CarbideCliError::GenericError(format!("Invalid machine ID '{}': {}", host_id, e))
    })?;

    // Call API to get current health report entries
    let response = api_client.machine_list_health_reports(machine_id).await?;

    Ok(response)
}

// Step 1: Reusable request builder
fn build_grafana_query_request(
    expr: &str,
    start_ms: i64,
    end_ms: i64,
    loki_uid: &str,
    batch_size: u32,
) -> GrafanaQueryRequest {
    GrafanaQueryRequest {
        queries: vec![LokiQuery {
            ref_id: "A".to_string(),
            datasource: LokiDatasource {
                datasource_type: "loki".to_string(),
                uid: loki_uid.to_string(),
            },
            query_type: "range".to_string(),
            expr: expr.to_string(),
            max_lines: batch_size,
        }],
        from: start_ms.to_string(),
        to: end_ms.to_string(),
        limit: batch_size,
    }
}

// Step 2: Reusable HTTP executor
async fn execute_grafana_query(
    query_request: &GrafanaQueryRequest,
    grafana_client: &GrafanaClient<'_>,
) -> CarbideCliResult<String> {
    let response = grafana_client
        .client
        .post(format!("{}/api/ds/query", grafana_client.base_url))
        .header("X-Scope-OrgID", "forge")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header(
            "Authorization",
            format!("Bearer {}", grafana_client.auth_token),
        )
        .json(query_request)
        .send()
        .await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            println!("   Response Status: {status}");

            if status.is_success() {
                let body = resp.text().await.map_err(|e| {
                    CarbideCliError::GenericError(format!("Failed to read response body: {e}"))
                })?;
                Ok(body)
            } else {
                let body = resp.text().await.unwrap_or_default();
                Err(CarbideCliError::GenericError(format!(
                    "HTTP Error {status}: {body}"
                )))
            }
        }
        Err(e) => Err(CarbideCliError::GenericError(format!(
            "Connection failed: {e}"
        ))),
    }
}

// Structure to hold batch processing results
struct BatchResult {
    entries: Vec<LogEntry>,
    newest_timestamp: Option<i64>,
    original_batch_count: usize, // Count before deduplication
}

// Helper function to handle pagination logic
fn handle_pagination(
    entry_count: usize,
    newest_timestamp: Option<i64>,
    batch_size: usize,
) -> CarbideCliResult<Option<DateTime<Utc>>> {
    // Parse response to check if we need pagination
    if entry_count < batch_size {
        return Ok(None);
    }

    if let Some(newest_ts) = newest_timestamp {
        let next_end_ms = newest_ts + 1;
        Ok(DateTime::from_timestamp_millis(next_end_ms))
    } else {
        Ok(None)
    }
}

// Parse Grafana JSON response using strongly typed structs
fn parse_grafana_logs(json_response: String) -> CarbideCliResult<(Vec<LogEntry>, Option<i64>)> {
    let response: GrafanaResponse = serde_json::from_str(&json_response)
        .map_err(|e| CarbideCliError::GenericError(format!("Failed to parse JSON: {e}")))?;

    let Some(frame) = response.results.a.frames.into_iter().next() else {
        return Err(CarbideCliError::GenericError(
            "No frames found in grafana results".to_string(),
        ));
    };

    // TODO: Where is this assumption that there are 5 values coming from?
    // This code should be rewritten make fewer assumptions about the data we're getting.
    let [_, value1, value2, value3, value4] = frame
        .data
        .values
        .into_iter()
        .take(5)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|v: Vec<_>| {
            CarbideCliError::GenericError(format!(
                "Invalid grafana frame: Expected at least 5 values, got {}",
                v.len()
            ))
        })?;

    // Extract newest timestamp from values[1] for pagination
    let newest_timestamp = value1
        .iter()
        .filter_map(|val| match val {
            GrafanaValue::String(s) => s.parse::<i64>().ok(),
            GrafanaValue::Int(ts) => Some(*ts),
            _ => None,
        })
        .max();

    // Extract nanosecond timestamps from values[3] for sorting
    let mut timestamps = value3.into_iter().map(|val| match val {
        GrafanaValue::String(s) => s.parse::<u64>().ok(),
        GrafanaValue::Int(ts) => ts.try_into().ok(),
        _ => None,
    });

    // Extract log messages from values[2]
    let logs = value2.into_iter().map(|val| match val {
        GrafanaValue::String(s) => Some(s),
        _ => None,
    });

    // Extract unique IDs from values[4] for deduplication
    let mut unique_ids = value4.into_iter().map(|val| match val {
        GrafanaValue::String(s) => Some(s),
        _ => None,
    });

    // Extract millisecond timestamps from values[1] for headers
    let mut ms_timestamps = value1.into_iter().map(|val| match val {
        GrafanaValue::Int(n) => Some(n),
        GrafanaValue::String(n) => n.parse().ok(),
        _ => None,
    });

    // Create LogEntry structs using direct indexing
    let mut log_entries: Vec<LogEntry> = logs
        .filter_map(|log| {
            let log = log?;
            let ns_timestamp = timestamps.next()??;
            let id = unique_ids.next()??;
            let ms_timestamp: i64 = ms_timestamps.next()??;
            Some(LogEntry {
                message: log,
                timestamp_ms: ms_timestamp,
                unique_id: id,
                nanosecond_timestamp: ns_timestamp,
            })
        })
        .collect();

    // Sort by nanosecond timestamp for perfect chronological order
    log_entries.sort_by_key(|entry| entry.nanosecond_timestamp);

    Ok((log_entries, newest_timestamp))
}

// Helper function to remove duplicates from the end of batch (optimized for timestamp-sorted logs)
fn remove_duplicates_from_end(
    mut entries: Vec<LogEntry>,
    existing_unique_ids: &std::collections::HashSet<String>,
) -> Vec<LogEntry> {
    while let Some(last_entry) = entries.last() {
        if last_entry.is_duplicate(existing_unique_ids) {
            entries.pop();
        } else {
            break;
        }
    }

    entries
}

//  function to format timestamp as "2025-08-28 06:06:55.281" for ZIP file headers
fn format_timestamp_header(timestamp_ms: i64) -> String {
    if let Some(datetime) = DateTime::from_timestamp_millis(timestamp_ms) {
        let local_time: DateTime<Local> = datetime.with_timezone(&Local);
        local_time.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
    } else {
        "Unknown Time".to_string()
    }
}

// datetime parsing function
fn parse_datetime_input(input: &str, use_utc: bool) -> CarbideCliResult<DateTime<Utc>> {
    let dash_count = input.chars().filter(|&c| c == '-').count();
    let colon_count = input.chars().filter(|&c| c == ':').count();

    let naive_datetime = if dash_count == 2 && colon_count == 2 {
        // Format: "2025-09-02 06:00:00" (full datetime)
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.len() == 2 {
            NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S")
                .map_err(|_| CarbideCliError::InvalidDateTimeFromUserInput(input.to_string()))?
        } else {
            return Err(CarbideCliError::InvalidDateTimeFromUserInput(
                input.to_string(),
            ));
        }
    } else if dash_count == 0 && colon_count == 2 {
        // Format: "06:00:00" (time only - use today's date)
        let today = chrono::Local::now().date_naive();
        let time = NaiveTime::parse_from_str(input, "%H:%M:%S")
            .map_err(|_| InvalidDateTimeFromUserInput(input.to_string()))?;
        NaiveDateTime::new(today, time)
    } else {
        return Err(CarbideCliError::InvalidDateTimeFromUserInput(
            input.to_string(),
        ));
    };

    if use_utc {
        Ok(naive_datetime.and_utc())
    } else {
        let local = naive_datetime
            .and_local_timezone(Local)
            .single()
            .ok_or_else(|| CarbideCliError::GenericError(format!("Invalid or ambiguous time '{input}'. This may occur during daylight saving time transitions. Please use a different time or use --utc flag.")))?;
        Ok(local.into())
    }
}

fn generate_grafana_link(
    grafana_base_url: &str,
    loki_uid: &str,
    expr: &str,
    start_ms: i64,
    end_ms: i64,
) -> CarbideCliResult<String> {
    let config = GrafanaConfig {
        datasource: loki_uid.to_string(),
        queries: vec![GrafanaQuery {
            expr: expr.to_string(),
            ref_id: "A".to_string(),
        }],
        range: GrafanaTimeRange {
            from: start_ms.to_string(),
            to: end_ms.to_string(),
        },
    };

    let json_str = serde_json::to_string(&config).map_err(|e| {
        CarbideCliError::GenericError(format!("Failed to serialize Grafana config: {e}"))
    })?;

    let encoded = urlencoding::encode(&json_str);
    let grafana_url = format!("{grafana_base_url}/explore?left={encoded}");

    Ok(grafana_url)
}

// NEW ZIP CREATOR STRUCT
struct ZipBundleCreator<'a> {
    config: &'a DebugBundle,
    timestamp: String,
}

impl<'a> ZipBundleCreator<'a> {
    fn new(config: &'a DebugBundle) -> Self {
        Self {
            timestamp: chrono::Local::now().format("%Y%m%d%H%M%S").to_string(),
            config,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_bundle(
        &self,
        host_logs: &[LogEntry],
        carbide_logs: &[LogEntry],
        dpu_agent_logs: &[LogEntry],
        host_batch_links: &[(String, String, usize, String)],
        carbide_batch_links: &[(String, String, usize, String)],
        dpu_batch_links: &[(String, String, usize, String)],
        loki_uid: Option<&str>,
        health_alerts: &::rpc::forge::HealthHistories,
        alert_entries: &::rpc::forge::ListHealthReportResponse,
        site_controller_analysis: &SiteControllerAnalysis,
        machine_analysis: &MachineAnalysis,
    ) -> CarbideCliResult<String> {
        let filename = format!("{}_{}.zip", self.timestamp, self.config.host_id);
        let output_path = self.config.output_path.trim_end_matches('/');
        let filepath = format!("{}/{}", output_path, filename);
        let mut zip = ZipWriter::new(File::create(&filepath).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to create ZIP file: {e}"))
        })?);
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);

        // Add all files using helper method
        self.add_file(
            &mut zip,
            &format!("host_logs_{}.txt", self.config.host_id),
            host_logs,
            options,
        )?;
        self.add_file(&mut zip, "carbide_api_logs.txt", carbide_logs, options)?;
        self.add_file(
            &mut zip,
            &format!("dpu_agent_logs_{}.txt", self.config.host_id),
            dpu_agent_logs,
            options,
        )?;
        self.add_alerts_json(&mut zip, health_alerts, options)?;
        self.add_alert_entries_json(&mut zip, alert_entries, options)?;
        self.add_site_controller_analysis_json(&mut zip, site_controller_analysis, options)?;
        self.add_machine_analysis_json(&mut zip, machine_analysis, options)?;
        self.add_metadata(
            &mut zip,
            host_logs.len(),
            carbide_logs.len(),
            dpu_agent_logs.len(),
            host_batch_links,
            carbide_batch_links,
            dpu_batch_links,
            loki_uid,
            health_alerts,
            alert_entries,
            site_controller_analysis,
            machine_analysis,
            options,
        )?;

        zip.finish()
            .map_err(|e| CarbideCliError::GenericError(format!("Failed to finish ZIP: {e}")))?;

        println!("ZIP created: {filepath}");
        println!(
            "Files: host_logs_{}.txt ({} logs), carbide_api_logs.txt ({} logs), dpu_agent_logs_{}.txt ({} logs), health_alerts.json ({} records), health_alert_overrides.json ({} entries), site_controller_details.json, machine_info.json, metadata.txt",
            self.config.host_id,
            host_logs.len(),
            carbide_logs.len(),
            self.config.host_id,
            dpu_agent_logs.len(),
            health_alerts
                .histories
                .get(&self.config.host_id)
                .map(|h| h.records.len())
                .unwrap_or(0),
            alert_entries.health_report_entries.len()
        );

        Ok(filepath)
    }

    fn add_file(
        &self,
        zip: &mut ZipWriter<File>,
        filename: &str,
        logs: &[LogEntry],
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file(filename, options).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to create file {filename}: {e}"))
        })?;
        for entry in logs {
            writeln!(zip, "{} {}", entry.format_header(), entry.message)?;
        }
        Ok(())
    }

    fn add_alerts_json(
        &self,
        zip: &mut ZipWriter<File>,
        health_alerts: &::rpc::forge::HealthHistories,
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file("health_alerts.json", options).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to create health_alerts.json: {e}"))
        })?;

        // Build JSON, extracting ONLY alerts from each HealthReport
        let json_records: Vec<_> = health_alerts
            .histories
            .get(&self.config.host_id)
            .map(|h| {
                h.records
                    .iter()
                    .filter_map(|record| {
                        record.health.as_ref().map(|health| {
                            serde_json::json!({
                                "alert_count": health.alerts.len(),
                                "timestamp": record.time.as_ref().map(|t|
                                    chrono::DateTime::from_timestamp(t.seconds, t.nanos as u32)
                                        .map(|dt| dt.to_rfc3339())
                                        .unwrap_or_else(|| "invalid".to_string())
                                ),
                                "source": &health.source,
                                "alerts": health.alerts.iter().map(|alert| {
                                    serde_json::json!({
                                        "id": &alert.id,
                                        "target": alert.target.as_ref(),
                                        "in_alert_since": alert.in_alert_since.as_ref().map(|t|
                                            chrono::DateTime::from_timestamp(t.seconds, t.nanos as u32)
                                                .map(|dt| dt.to_rfc3339())
                                                .unwrap_or_else(|| "invalid".to_string())
                                        ),
                                        "message": &alert.message
                                    })
                                }).collect::<Vec<_>>()
                            })
                        })
                })
            .collect()
        })
        .unwrap_or_default();

        let total_alerts: usize = json_records
            .iter()
            .filter_map(|r| r.get("alert_count"))
            .filter_map(|v| v.as_u64())
            .map(|v| v as usize)
            .sum();

        let json_output = serde_json::json!({
            "summary": {
                "total_records": json_records.len(),
                "total_alerts": total_alerts
            },
            "records": json_records
        });

        // Write pretty-formatted JSON to ZIP
        let json_string = serde_json::to_string_pretty(&json_output).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to serialize health alerts to JSON: {e}"))
        })?;

        write!(zip, "{}", json_string)?;
        Ok(())
    }

    fn add_alert_entries_json(
        &self,
        zip: &mut ZipWriter<File>,
        alert_entries: &::rpc::forge::ListHealthReportResponse,
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file("health_alert_overrides.json", options)
            .map_err(|e| {
                CarbideCliError::GenericError(format!(
                    "Failed to create health_alert_overrides.json: {e}"
                ))
            })?;

        // Build JSON using serde_json::json! macro
        let json_output = serde_json::json!({
            "summary": {
                "total_overrides": alert_entries.health_report_entries.len()
            },
            "overrides": alert_entries.health_report_entries.iter().map(|entry| {
                let mode_str = match entry.mode {
                    1 => "Merge",
                    2 => "Replace",
                    _ => "Unknown"
                };

                serde_json::json!({
                    "mode": mode_str,
                    "report": entry.report.as_ref().map(|report| {
                        serde_json::json!({
                            "source": &report.source,
                            "alerts": report.alerts.iter().map(|alert| {
                                serde_json::json!({
                                    "id": &alert.id,
                                    "target": alert.target.as_ref(),
                                    "message": &alert.message,
                                    "tenant_message": alert.tenant_message.as_ref(),
                                })
                            }).collect::<Vec<_>>(),
                            "successes": report.successes.iter().map(|success| {
                                serde_json::json!({
                                    "id": &success.id,
                                    "target": success.target.as_ref(),
                                })
                            }).collect::<Vec<_>>(),
                        })
                    })
                })
            }).collect::<Vec<_>>()
        });

        // Write pretty-formatted JSON to ZIP
        let json_string = serde_json::to_string_pretty(&json_output).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to serialize entries to JSON: {e}"))
        })?;

        write!(zip, "{}", json_string)?;
        Ok(())
    }

    fn add_site_controller_analysis_json(
        &self,
        zip: &mut ZipWriter<File>,
        analysis: &SiteControllerAnalysis,
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file("site_controller_details.json", options)
            .map_err(|e| {
                CarbideCliError::GenericError(format!(
                    "Failed to create site_controller_details.json: {e}"
                ))
            })?;

        let report = &analysis.exploration_report;

        // Format BMC information
        let bmc_info = json!({
            "ip": analysis.bmc_ip,
            "mac": analysis.bmc_mac,
        });

        // Format credentials information
        let credentials_info = json!({
            "have_credentials": analysis.credential_status.have_credentials,
        });

        // Format systems information
        let systems_info = report
            .systems
            .iter()
            .map(|system| {
                json!({
                    "id": system.id,
                    "manufacturer": system.manufacturer,
                    "model": system.model,
                    "serial_number": system.serial_number,
                    "power_state": system.power_state,
                    "ethernet_interfaces_count": system.ethernet_interfaces.len(),
                    "pcie_devices_count": system.pcie_devices.len(),
                })
            })
            .collect::<Vec<_>>();

        // Format managers information
        let managers_info = report
            .managers
            .iter()
            .map(|manager| {
                json!({
                    "id": manager.id,
                    "ethernet_interfaces_count": manager.ethernet_interfaces.len(),
                })
            })
            .collect::<Vec<_>>();

        // Format chassis information
        let chassis_info = report
            .chassis
            .iter()
            .map(|chassis| {
                json!({
                    "id": chassis.id,
                    "manufacturer": chassis.manufacturer,
                    "model": chassis.model,
                    "serial_number": chassis.serial_number,
                    "part_number": chassis.part_number,
                    "network_adapters_count": chassis.network_adapters.len(),
                })
            })
            .collect::<Vec<_>>();

        // Format firmware inventory
        let firmware_inventory_info = report
            .service
            .iter()
            .flat_map(|service| {
                service.inventories.iter().map(|inv| {
                    json!({
                        "service_id": &service.id,
                        "inventory_id": &inv.id,
                        "description": inv.description,
                        "version": inv.version,
                        "release_date": inv.release_date,
                    })
                })
            })
            .collect::<Vec<_>>();

        // Format machine setup status
        let machine_setup_status_info = report.machine_setup_status.as_ref().map(|status| {
            json!({
                "is_done": status.is_done,
                "diffs_count": status.diffs.len(),
            })
        });

        // Format redfish exploration
        let redfish_exploration_info = json!({
            "endpoint_type": report.endpoint_type,
            "vendor": report.vendor,
            "systems": systems_info,
            "managers": managers_info,
            "chassis": chassis_info,
            "firmware_inventory": firmware_inventory_info,
            "machine_setup_status": machine_setup_status_info,
        });

        // Create final JSON structure
        let json_output = json!({
            "host_id": self.config.host_id,
            "bmc": bmc_info,
            "credentials": credentials_info,
            "redfish_exploration": redfish_exploration_info,
        });

        // Write pretty-formatted JSON to ZIP
        let json_string = serde_json::to_string_pretty(&json_output).map_err(|e| {
            CarbideCliError::GenericError(format!(
                "Failed to serialize site controller analysis to JSON: {e}"
            ))
        })?;

        write!(zip, "{}", json_string)?;
        Ok(())
    }

    fn add_machine_analysis_json(
        &self,
        zip: &mut ZipWriter<File>,
        analysis: &MachineAnalysis,
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file("machine_info.json", options).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to create machine_info.json: {e}"))
        })?;

        let machine = &analysis.machine;
        let status = machine.status.as_ref();

        // Format SLA information
        let sla_info = machine.state_sla.as_ref().map(|sla| {
            json!({
                "sla_duration_seconds": sla.sla.as_ref().map(|d| d.seconds),
                "time_in_state_above_sla": sla.time_in_state_above_sla,
                "status": if sla.time_in_state_above_sla { "BREACHED" } else { "OK" }
            })
        });

        // Format controller state reason
        let controller_state = machine.state_reason.as_ref().map(|reason| {
            json!({
                "outcome": format!("{:?}", reason.outcome()),
                "message": &reason.outcome_msg,
                "source": reason.source_ref.as_ref().map(|src| {
                    format!("{}:{}", src.file, src.line)
                }),
            })
        });

        // Format reboot information
        let reboot_info = json!({
            "last_reboot_time": status.and_then(|status| status.last_reboot_time.as_ref()).map(|ts| {
                DateTime::<Utc>::from_timestamp(ts.seconds, ts.nanos as u32)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| ts.seconds.to_string())
            }),
            "last_reboot_requested": {
                "time": status.and_then(|status| status.last_reboot_requested_time.as_ref()).map(|ts| {
                    DateTime::<Utc>::from_timestamp(ts.seconds, ts.nanos as u32)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_else(|| ts.seconds.to_string())
                }),
                "mode": status.and_then(|status| status.last_reboot_requested_mode.as_ref()),
            }
        });

        // Format validation results (only failures)
        let validation_info = json!({
            "failed_tests": analysis.validation_results.len(),
            "tests": analysis.validation_results.iter().map(|result| {
                json!({
                    "name": result.name,
                    "description": result.description,
                    "exit_code": result.exit_code,
                    "passed": false,
                    "start_time": result.start_time.as_ref().map(|ts| {
                        DateTime::<Utc>::from_timestamp(ts.seconds, ts.nanos as u32)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_else(|| ts.seconds.to_string())
                    }),
                    "end_time": result.end_time.as_ref().map(|ts| {
                        DateTime::<Utc>::from_timestamp(ts.seconds, ts.nanos as u32)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_else(|| ts.seconds.to_string())
                    }),
                    "command": result.command,
                    "args": result.args,
                    "stdout": result.std_out,
                    "stderr": result.std_err,
                    "context": result.context,
                })
            }).collect::<Vec<_>>()
        });

        // Create final JSON structure
        let json_output = json!({
            "machine_id": machine.id.as_ref().map(|id| id.to_string()),
            "state_information": {
                "current_state": machine.state,
                "state_version": machine.state_version,
            },
            "sla": sla_info,
            "controller_state": controller_state,
            "failure_details": status.and_then(|status| status.failure_details.as_ref()),
            "reboot_information": reboot_info,
            "validation_results": validation_info,
        });

        // Write pretty-formatted JSON to ZIP
        let json_string = serde_json::to_string_pretty(&json_output).map_err(|e| {
            CarbideCliError::GenericError(format!(
                "Failed to serialize machine analysis to JSON: {e}"
            ))
        })?;

        write!(zip, "{}", json_string)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn add_metadata(
        &self,
        zip: &mut ZipWriter<File>,
        host_count: usize,
        carbide_count: usize,
        dpu_agent_count: usize,
        host_batch_links: &[(String, String, usize, String)],
        carbide_batch_links: &[(String, String, usize, String)],
        dpu_batch_links: &[(String, String, usize, String)],
        loki_uid: Option<&str>,
        health_alerts: &::rpc::forge::HealthHistories,
        alert_entries: &::rpc::forge::ListHealthReportResponse,
        site_controller_analysis: &SiteControllerAnalysis,
        machine_analysis: &MachineAnalysis,
        options: FileOptions<()>,
    ) -> CarbideCliResult<()> {
        zip.start_file("metadata.txt", options).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to create metadata file: {e}"))
        })?;
        writeln!(zip, "Debug Bundle: {}", self.config.host_id)?;
        let end_time_display = self
            .config
            .end_time
            .as_deref()
            .unwrap_or("now (current time)");
        writeln!(
            zip,
            "Time Range: {} to {}",
            self.config.start_time, end_time_display
        )?;
        writeln!(
            zip,
            "Grafana URL: {}",
            self.config
                .grafana_url
                .as_deref()
                .unwrap_or("N/A (logs not collected)")
        )?;
        writeln!(zip, "Host Logs: {host_count}")?;
        writeln!(zip, "Carbide-API Logs: {carbide_count}")?;
        writeln!(zip, "DPU Agent Logs: {dpu_agent_count}")?;
        writeln!(
            zip,
            "Total: {}",
            host_count + carbide_count + dpu_agent_count
        )?;
        writeln!(zip)?;

        // Add Health Alerts Info
        writeln!(zip, "Health Alerts:")?;
        let (record_count, total_alerts) = health_alerts
            .histories
            .get(&self.config.host_id)
            .map(|h| {
                let count = h.records.len();
                let alerts: usize = h
                    .records
                    .iter()
                    .filter_map(|r| r.health.as_ref())
                    .map(|h| h.alerts.len())
                    .sum();
                (count, alerts)
            })
            .unwrap_or((0, 0));
        writeln!(zip, "  Total Records: {}", record_count)?;
        writeln!(zip, "  Total Alerts: {}", total_alerts)?;
        writeln!(zip)?;

        // Add Health Report Entries Info
        writeln!(zip, "Health Report Entries:")?;
        writeln!(
            zip,
            "  Total Entries: {}",
            alert_entries.health_report_entries.len()
        )?;
        let active_entries = alert_entries
            .health_report_entries
            .iter()
            .filter(|o| {
                if let Some(ref report) = o.report {
                    report.alerts.iter().any(|a| a.in_alert_since.is_some())
                } else {
                    false
                }
            })
            .count();
        writeln!(zip, "  Active Entries: {}", active_entries)?;
        writeln!(zip)?;

        // Add Site Controller Details
        writeln!(zip, "Site Controller Details:")?;
        writeln!(zip, "  BMC IP: {}", site_controller_analysis.bmc_ip)?;
        if let Some(ref mac) = site_controller_analysis.bmc_mac {
            writeln!(zip, "  BMC MAC: {}", mac)?;
        }
        writeln!(
            zip,
            "  Credentials Available: {}",
            site_controller_analysis.credential_status.have_credentials
        )?;
        writeln!(
            zip,
            "  Systems Found: {}",
            site_controller_analysis.exploration_report.systems.len()
        )?;
        writeln!(
            zip,
            "  Managers Found: {}",
            site_controller_analysis.exploration_report.managers.len()
        )?;
        writeln!(
            zip,
            "  Chassis Found: {}",
            site_controller_analysis.exploration_report.chassis.len()
        )?;
        writeln!(
            zip,
            "  Firmware Services: {}",
            site_controller_analysis.exploration_report.service.len()
        )?;
        writeln!(zip)?;

        // Add Machine Info
        writeln!(zip, "Machine Info:")?;
        writeln!(
            zip,
            "  Machine ID: {}",
            machine_analysis
                .machine
                .id
                .as_ref()
                .map(|id| id.to_string())
                .as_deref()
                .unwrap_or("N/A")
        )?;
        writeln!(zip, "  Current State: {}", machine_analysis.machine.state)?;
        writeln!(
            zip,
            "  State Version: {}",
            machine_analysis.machine.state_version
        )?;

        if let Some(ref sla) = machine_analysis.machine.state_sla {
            let status = if sla.time_in_state_above_sla {
                "BREACHED"
            } else {
                "✓ OK"
            };
            writeln!(zip, "  SLA Status: {}", status)?;
        }

        if let Some(ref reason) = machine_analysis.machine.state_reason {
            writeln!(zip, "  Controller Outcome: {:?}", reason.outcome())?;
            if let Some(ref msg) = reason.outcome_msg {
                writeln!(zip, "  Controller Message: {}", msg)?;
            }
        }

        if machine_analysis
            .machine
            .status
            .as_ref()
            .is_some_and(|status| status.failure_details.is_some())
        {
            writeln!(zip, "  WARNING: Has Failure Details: Yes")?;
        }

        writeln!(
            zip,
            "  Validation Failures: {} failed tests",
            machine_analysis.validation_results.len()
        )?;
        writeln!(zip)?;

        // Generate overall Grafana links only if logs were collected
        if let (Some(loki_uid), Some(grafana_url)) = (loki_uid, &self.config.grafana_url) {
            let start = parse_datetime_input(&self.config.start_time, self.config.utc)?;

            // Handle optional end_time (default to "now")
            let end = if let Some(ref end_time_str) = self.config.end_time {
                parse_datetime_input(end_time_str, self.config.utc)?
            } else {
                chrono::Utc::now()
            };

            let time_range = TimeRange {
                start,
                end,
                utc: self.config.utc,
            };
            let (start_ms, end_ms) = time_range.to_grafana_format();

            let host_expr = format!("{{host_machine_id=\"{}\"}} |= ``", self.config.host_id);
            let host_overall_link =
                generate_grafana_link(grafana_url, loki_uid, &host_expr, start_ms, end_ms)?;

            let carbide_expr =
                format!("{{{K8S_CONTAINER_NAME_LABEL}=\"{CARBIDE_API_CONTAINER_NAME}\"}} |= ``");
            let carbide_overall_link =
                generate_grafana_link(grafana_url, loki_uid, &carbide_expr, start_ms, end_ms)?;

            let dpu_agent_expr = format!(
                "{{systemd_unit=\"forge-dpu-agent.service\", host_machine_id=\"{}\"}} |= ``",
                self.config.host_id
            );
            let dpu_agent_overall_link =
                generate_grafana_link(grafana_url, loki_uid, &dpu_agent_expr, start_ms, end_ms)?;

            // Host Logs - Overall Link and Batches
            writeln!(zip, "Host Logs Grafana Link (Complete Time Range):")?;
            writeln!(zip, "  {}", host_overall_link)?;
            writeln!(zip)?;

            if !host_batch_links.is_empty() {
                writeln!(zip, "Host Logs Batches:")?;
                for (batch_label, grafana_link, log_count, time_range_display) in host_batch_links {
                    writeln!(zip, "  {batch_label} ({log_count} logs):")?;
                    writeln!(zip, "    Time Range: {time_range_display}")?;
                    writeln!(zip, "    {grafana_link}")?;
                    writeln!(zip)?;
                }
            }

            // Carbide-API Logs - Overall Link and Batches
            writeln!(zip, "Carbide-API Logs Grafana Link (Complete Time Range):")?;
            writeln!(zip, "  {}", carbide_overall_link)?;
            writeln!(zip)?;

            if !carbide_batch_links.is_empty() {
                writeln!(zip, "Carbide-API Logs Batches:")?;
                for (batch_label, grafana_link, log_count, time_range_display) in
                    carbide_batch_links
                {
                    writeln!(zip, "  {batch_label} ({log_count} logs):")?;
                    writeln!(zip, "    Time Range: {time_range_display}")?;
                    writeln!(zip, "    {grafana_link}")?;
                    writeln!(zip)?;
                }
            }

            // DPU Agent Logs - Overall Link and Batches
            writeln!(zip, "DPU Agent Logs Grafana Link (Complete Time Range):")?;
            writeln!(zip, "  {}", dpu_agent_overall_link)?;
            writeln!(zip)?;

            if !dpu_batch_links.is_empty() {
                writeln!(zip, "DPU Agent Logs Batches:")?;
                for (batch_label, grafana_link, log_count, time_range_display) in dpu_batch_links {
                    writeln!(zip, "  {batch_label} ({log_count} logs):")?;
                    writeln!(zip, "    Time Range: {time_range_display}")?;
                    writeln!(zip, "    {grafana_link}")?;
                    writeln!(zip)?;
                }
            }
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn create_debug_bundle_zip(
    debug_bundle: &DebugBundle,
    host_logs: &[LogEntry],
    carbide_api_logs: &[LogEntry],
    dpu_agent_logs: &[LogEntry],
    host_batch_links: &[(String, String, usize, String)],
    carbide_batch_links: &[(String, String, usize, String)],
    dpu_batch_links: &[(String, String, usize, String)],
    loki_uid: Option<&str>,
    health_alerts: &::rpc::forge::HealthHistories,
    alert_entries: &::rpc::forge::ListHealthReportResponse,
    site_controller_analysis: &SiteControllerAnalysis,
    machine_analysis: &MachineAnalysis,
) -> CarbideCliResult<()> {
    ZipBundleCreator::new(debug_bundle).create_bundle(
        host_logs,
        carbide_api_logs,
        dpu_agent_logs,
        host_batch_links,
        carbide_batch_links,
        dpu_batch_links,
        loki_uid,
        health_alerts,
        alert_entries,
        site_controller_analysis,
        machine_analysis,
    )?;
    Ok(())
}
