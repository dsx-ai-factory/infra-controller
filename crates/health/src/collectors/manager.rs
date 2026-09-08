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

//! Redfish `Manager` status collection.
//!
//! A power shelf's management controller (PMC) is its Redfish `Manager`. This
//! collector publishes the manager's status, power state, firmware version,
//! and last reset time so the analyzer can judge controller availability
//! separately from BMC reachability.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nv_redfish::core::{Bmc, ToSnakeCase};
use nv_redfish::schema::manager::Manager as ManagerSchema;
use nv_redfish::{Resource, ServiceRoot};

use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::metrics::MetricLabel;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

pub struct ManagerCollectorConfig {
    pub data_sink: Option<Arc<dyn DataSink>>,
}

pub struct ManagerCollector<B: Bmc> {
    bmc: Arc<B>,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,
}

impl<B> PeriodicCollector<B> for ManagerCollector<B>
where
    B: Bmc + 'static,
    B::Error: 'static,
{
    type Config = ManagerCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        Ok(Self {
            bmc,
            event_context: EventContext::from_endpoint(endpoint.as_ref(), "manager_collector"),
            data_sink: config.data_sink,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let managers = match service_root.managers().await? {
            Some(collection) => collection.members().await?,
            None => Vec::new(),
        };

        self.emit_event(CollectorEvent::MetricCollectionStart);
        for manager in &managers {
            for sample in manager_metrics(&manager.raw(), &manager.odata_id().to_string()) {
                self.emit_event(CollectorEvent::Metric(Box::new(sample)));
            }
        }
        self.emit_event(CollectorEvent::MetricCollectionEnd);

        Ok(IterationResult {
            refresh_triggered: false,
            entity_count: Some(managers.len()),
            fetch_failures: 0,
        })
    }

    fn collector_type(&self) -> &'static str {
        "manager_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B: Bmc> ManagerCollector<B> {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }
}

/// Projects one manager into `manager_status` and `manager_last_reset` samples.
///
/// `manager_status` is an informational gauge (value `1.0`) whose labels carry
/// the state, health, power state, and firmware version. `manager_last_reset`
/// is the last reset time as seconds since the Unix epoch. Absent fields are
/// omitted rather than defaulted.
fn manager_metrics(manager: &ManagerSchema, key: &str) -> Vec<MetricSample> {
    let mut labels: Vec<MetricLabel> = vec![(Cow::Borrowed("manager_id"), manager.base.id.clone())];
    if let Some(status) = &manager.status {
        if let Some(state) = status.state.flatten() {
            labels.push((
                Cow::Borrowed("manager_state"),
                state.to_snake_case().to_string(),
            ));
        }
        if let Some(health) = status.health.flatten() {
            labels.push((
                Cow::Borrowed("manager_health"),
                health.to_snake_case().to_string(),
            ));
        }
    }
    if let Some(power_state) = manager.power_state.flatten() {
        labels.push((
            Cow::Borrowed("manager_power_state"),
            power_state.to_snake_case().to_string(),
        ));
    }
    if let Some(firmware_version) = manager.firmware_version.clone().flatten() {
        labels.push((Cow::Borrowed("firmware_version"), firmware_version));
    }

    let mut samples = vec![MetricSample {
        key: format!("{key}/manager_status"),
        name: "hw".to_string(),
        metric_type: "manager_status".to_string(),
        unit: "state".to_string(),
        value: 1.0,
        labels: labels.clone(),
        context: None,
    }];

    // A reset time before the Unix epoch is not a real BMC value; skip it
    // rather than publish a negative timestamp.
    if let Some(unix_seconds) = manager
        .last_reset_time
        .and_then(|last_reset_time| SystemTime::try_from(last_reset_time).ok())
        .and_then(|reset| reset.duration_since(UNIX_EPOCH).ok())
    {
        samples.push(MetricSample {
            key: format!("{key}/manager_last_reset"),
            name: "hw".to_string(),
            metric_type: "manager_last_reset".to_string(),
            unit: "seconds".to_string(),
            value: unix_seconds.as_secs_f64(),
            labels: vec![(Cow::Borrowed("manager_id"), manager.base.id.clone())],
            context: None,
        });
    }

    samples
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[derive(Debug, PartialEq)]
    struct ObservedSample {
        key: String,
        metric_type: String,
        unit: String,
        value: f64,
        labels: Vec<(String, String)>,
    }

    fn observe(json: &str) -> Vec<ObservedSample> {
        let manager: ManagerSchema = serde_json::from_str(json).expect("valid manager");
        manager_metrics(&manager, "/redfish/v1/Managers/bmc")
            .into_iter()
            .map(|sample| ObservedSample {
                key: sample.key,
                metric_type: sample.metric_type,
                unit: sample.unit,
                value: sample.value,
                labels: sample
                    .labels
                    .into_iter()
                    .map(|(key, value)| (key.into_owned(), value))
                    .collect(),
            })
            .collect()
    }

    fn label(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    #[test]
    fn manager_metric_cases() {
        check_values(
            [
                Check {
                    scenario: "populated manager emits status and last reset",
                    input: r#"{
                        "@odata.id": "/redfish/v1/Managers/bmc",
                        "Id": "bmc",
                        "Name": "OpenBmc Manager",
                        "ManagerType": "BMC",
                        "FirmwareVersion": "r1.3.8-rc1",
                        "PowerState": "On",
                        "LastResetTime": "2026-08-06T03:43:37+00:00",
                        "Status": { "Health": "OK", "HealthRollup": "OK", "State": "StandbyOffline" }
                    }"#,
                    expect: vec![
                        ObservedSample {
                            key: "/redfish/v1/Managers/bmc/manager_status".to_string(),
                            metric_type: "manager_status".to_string(),
                            unit: "state".to_string(),
                            value: 1.0,
                            labels: vec![
                                label("manager_id", "bmc"),
                                label("manager_state", "standby_offline"),
                                label("manager_health", "ok"),
                                label("manager_power_state", "on"),
                                label("firmware_version", "r1.3.8-rc1"),
                            ],
                        },
                        ObservedSample {
                            key: "/redfish/v1/Managers/bmc/manager_last_reset".to_string(),
                            metric_type: "manager_last_reset".to_string(),
                            unit: "seconds".to_string(),
                            value: 1_785_987_817.0,
                            labels: vec![label("manager_id", "bmc")],
                        },
                    ],
                },
                Check {
                    scenario: "sparse manager emits status with identity only",
                    input: r#"{
                        "@odata.id": "/redfish/v1/Managers/bmc",
                        "Id": "bmc",
                        "Name": "OpenBmc Manager"
                    }"#,
                    expect: vec![ObservedSample {
                        key: "/redfish/v1/Managers/bmc/manager_status".to_string(),
                        metric_type: "manager_status".to_string(),
                        unit: "state".to_string(),
                        value: 1.0,
                        labels: vec![label("manager_id", "bmc")],
                    }],
                },
            ],
            observe,
        );
    }
}
