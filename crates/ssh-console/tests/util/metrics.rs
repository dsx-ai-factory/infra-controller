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
use std::borrow::Cow;

use eyre::Context;
use prometheus_text_parser::ParsedPrometheusMetrics;
use ssh_console_mock_api_server::MockHost;

pub(super) async fn assert_metrics(
    metrics_str: String,
    mock_hosts: &[MockHost],
) -> eyre::Result<()> {
    let metrics: ParsedPrometheusMetrics = metrics_str.parse().context("error parsing metrics")?;
    let metrics = metrics.metrics;

    let expected_metrics = [
        (
            "ssh_console_grpc_total_machines",
            vec![ExpectedObservation {
                attribute_key_value: None,
                value: Some(mock_hosts.len() as u64),
            }],
        ),
        (
            "ssh_console_total_machines",
            vec![ExpectedObservation {
                attribute_key_value: None,
                value: Some(mock_hosts.len() as u64),
            }],
        ),
        (
            "ssh_console_healthy_machines",
            vec![ExpectedObservation {
                attribute_key_value: None,
                // Don't assert this, because one of the tests drops to BMC and the machine still may be unhealthy by the time we assert.
                value: None,
            }],
        ),
        (
            "ssh_console_failed_machines",
            vec![ExpectedObservation {
                attribute_key_value: None,
                // Don't assert this, because one of the tests drops to BMC and the machine still may be unhealthy by the time we assert.
                value: None,
            }],
        ),
        (
            "ssh_console_total_clients",
            vec![ExpectedObservation {
                attribute_key_value: None,
                value: Some(0u64),
            }],
        ),
        (
            "ssh_console_client_auth_failures_total",
            vec![ExpectedObservation {
                attribute_key_value: Some(("auth_type", Cow::Borrowed("public_key"))),
                value: Some(mock_hosts.len() as u64),
            }],
        ),
        (
            "ssh_console_auth_enforced",
            vec![ExpectedObservation {
                attribute_key_value: None,
                value: Some(1u64),
            }],
        ),
        (
            "ssh_console_include_dpus",
            vec![ExpectedObservation {
                attribute_key_value: None,
                value: Some(0u64),
            }],
        ),
        (
            "ssh_console_bmc_bytes_received_total",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    value: None,
                })
                .collect(),
        ),
        (
            "ssh_console_bmc_clients",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    value: Some(0u64),
                })
                .collect(),
        ),
        (
            "ssh_console_bmc_rx_errors_total",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    value: Some(0),
                })
                .collect(),
        ),
        (
            "ssh_console_bmc_tx_errors_total",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    value: Some(0),
                })
                .collect(),
        ),
        (
            "ssh_console_bmc_recovery_attempts",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    value: None, // Don't assert on this, as some tests simulate disconnections and the machine-ID's are unpredictable
                })
                .collect(),
        ),
        (
            "ssh_console_bmc_status",
            mock_hosts
                .iter()
                .map(|mock_host| ExpectedObservation {
                    attribute_key_value: Some((
                        "machine_id",
                        Cow::Owned(mock_host.machine_id.to_string()),
                    )),
                    // Don't assert this, because one of the tests drops to BMC and the machine still may be unhealthy by the time we assert.
                    value: None,
                })
                .collect(),
        ),
    ];

    for (expected_metric_name, expected_observations) in expected_metrics {
        let metric = metrics
            .get(expected_metric_name)
            .unwrap_or_else(|| panic!("did not find metric {expected_metric_name} in metrics"));
        if expected_observations.is_empty() {
            continue;
        }
        let observations = metric.observations().unwrap_or_else(|| {
            panic!("no observations for metric {expected_metric_name} in metrics")
        });

        assert_eq!(observations.len(), expected_observations.len());
        for expected_observation in expected_observations {
            let observation = if let Some((attribute_key, attribute_value)) =
                expected_observation.attribute_key_value
            {
                // Actual metric values have "'s around them, so emulate that
                let attribute_value = format!("{attribute_value:?}");
                observations.iter().find(|o| o.attributes.0.get(attribute_key).is_some_and(|v| v.eq(&attribute_value)))
                    .unwrap_or_else(|| panic!("no observation for metric {expected_metric_name} with key={attribute_key} and value={attribute_value}: {observations:?}"))
            } else {
                &observations[0]
            };

            if let Some(expected_value) = expected_observation.value {
                assert_eq!(
                    observation.value, expected_value,
                    "expected observation for metric {} hash wrong value (expected {}, got {})",
                    expected_metric_name, expected_value, observation.value
                );
            }
        }
    }

    Ok(())
}

struct ExpectedObservation<'a, T> {
    attribute_key_value: Option<(&'a str, Cow<'a, str>)>,
    value: Option<T>,
}
