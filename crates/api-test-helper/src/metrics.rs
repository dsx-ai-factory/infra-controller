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
use std::net::SocketAddr;
use std::process;

pub fn metrics(metrics_endpoint: &SocketAddr) -> eyre::Result<String> {
    let endpoint = format!("http://{metrics_endpoint}/metrics");
    let args = vec![endpoint.clone()];
    // We don't pass the full path to curl here and rely on the fact
    // that `Command` searches the PATH. This makes function signatures tidier.
    let out = process::Command::new("curl").args(args).output()?;
    let response = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        tracing::error!("curl {endpoint} STDOUT: {response}");
        tracing::error!(
            "curl {endpoint} STDERR: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        eyre::bail!("curl {endpoint} exit status code {}", out.status);
    }
    Ok(response.to_string())
}

pub struct MetricInfo {
    pub name: String,
    pub help: String,
    pub ty: String,
}

/// Collect metric type information exposed by prometheus endpoints
pub fn collect_metric_infos(metrics_endpoints: &[SocketAddr]) -> eyre::Result<Vec<MetricInfo>> {
    let mut metric_infos: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();

    for ep in metrics_endpoints.iter() {
        let metrics = metrics(ep)?;
        let lines: Vec<&str> = metrics
            .lines()
            .filter(|line| line.starts_with("# HELP") || line.starts_with("# TYPE"))
            .collect();

        for line in lines {
            let mut parts = line.splitn(4, " ");
            let _pound = parts.next().unwrap();
            let line_type = parts.next().unwrap();
            let name = parts.next().unwrap().to_string();
            let value = parts.next().unwrap().to_string();
            if line_type == "TYPE" {
                metric_infos.entry(name).or_default().0 = Some(value);
            } else if line_type == "HELP" {
                metric_infos.entry(name).or_default().1 = Some(value);
            } else {
                panic!("Unhandled line type {line_type}");
            }
        }
    }

    let infos: Vec<MetricInfo> = metric_infos
        .into_iter()
        .map(|(name, (ty, help))| MetricInfo {
            name,
            help: help.unwrap_or_default(),
            ty: ty.unwrap_or_default(),
        })
        .collect();

    Ok(infos)
}

/// Waits for a specific metric line to show up. Returns the metrics
pub async fn wait_for_metric_line(
    metrics_endpoints: &[SocketAddr],
    expected_line: &str,
) -> eyre::Result<String> {
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    let mut last_metrics = String::new();

    while start.elapsed() < MAX_WAIT {
        for addr in metrics_endpoints {
            last_metrics = metrics(addr)?;
            if last_metrics.contains(expected_line) {
                return Ok(last_metrics);
            }
        }

        tracing::info!("Waiting for metric line");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    panic!(
        "Even after {MAX_WAIT:?} time, Metric line {expected_line} was not visible.\n
        Last metrics: {last_metrics}"
    );
}

pub fn assert_metric_line(metrics: &str, expected: &str) {
    assert!(
        metrics.contains(expected),
        "Expected \"{expected}\" in Metrics/nActual metrics are:\n{metrics}"
    );
}

pub fn assert_not_metric_line(metrics: &str, expected: &str) {
    assert!(
        !metrics.contains(expected),
        "Expected missing \"{expected}\" in Metrics/nActual metrics are:\n{metrics}"
    );
}
