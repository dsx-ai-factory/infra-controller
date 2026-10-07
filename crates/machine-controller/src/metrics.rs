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

//! Defines custom metrics that are collected and emitted by the Machine State Controller

use std::collections::{HashMap, HashSet};

use carbide_health_metrics::{
    HealthIterationMetrics, HealthMetricDimension, HealthObjectMetrics,
    register_alerts_suppressed_gauge, register_health_gauges,
};
use carbide_utils::metrics::SharedMetricsHolder;
use model::hardware_info::MachineInventorySoftwareComponent;
use model::tenant::TenantOrganizationId;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use state_controller::metrics::MetricsEmitter;

#[derive(Debug, Default)]
pub struct MachineMetrics {
    pub agent_versions: HashMap<String, usize>,
    pub dpus_up: usize,
    pub dpus_healthy: usize,
    /// DPU probe alerts by Probe ID and Target
    /// For Multi-DPU, the same host could experience failures on multiple DPUs
    pub dpu_health_probe_alerts: HashMap<(health_report::HealthProbeId, Option<String>), usize>,
    pub dpu_firmware_versions: HashMap<String, usize>,
    pub machine_inventory_component_versions: HashMap<MachineInventorySoftwareComponent, usize>,
    pub client_certificate_expiry: HashMap<String, Option<i64>>,
    pub machine_reboot_attempts_in_booting_with_discovery_image: Option<u64>,
    pub machine_reboot_attempts_in_failed_during_discovery: Option<u64>,
    pub num_gpus: usize,
    pub in_use_by_tenant: Option<TenantOrganizationId>,
    /// Health related signals that are common across all entity types, e.g switch/host/rack.
    pub health: HealthObjectMetrics,
    /// The SKU that is assigned to the host
    pub sku: Option<String>,
    pub sku_device_type: Option<String>,
    /// Whether the Machine is usable as an instance for a tenant
    /// Doing so requires
    /// - the Machine to be in `Ready` state
    /// - the Machine has not yet been target of an instance creation request
    /// - no health alerts which classification `PreventAllocations` to be set
    /// - the machine not to be in Maintenance Mode
    /// - the desired boot-interface generation to have a matching observation
    pub is_usable_as_instance: bool,
    /// is the host's bios password set
    pub is_host_bios_password_set: bool,
    /// The last machine validation list ((machine_id, context), status)
    pub last_machine_validation_list: HashMap<(String, String), i32>,
    /// Machine ID if this host has a scout heartbeat timeout
    pub host_with_scout_heartbeat_timeout: Option<String>,
}

#[derive(Debug, Default)]
pub struct MachineStateControllerIterationMetrics {
    pub agent_versions: HashMap<String, usize>,
    pub dpus_up: usize,
    pub dpus_healthy: usize,
    pub unhealthy_dpus_by_probe_id: HashMap<(String, Option<String>), usize>,
    pub dpu_firmware_versions: HashMap<String, usize>,
    /// Map from Machine component (names and version string) to the count of
    /// machines which run that version combination
    pub machine_inventory_component_versions: HashMap<MachineInventorySoftwareComponent, usize>,
    pub client_certificate_expiration_times: HashMap<String, i64>,
    pub gpus_usable: usize,
    pub gpus_total: usize,
    pub gpus_in_use_by_tenant: HashMap<TenantOrganizationId, usize>,
    pub hosts_in_use_by_tenant: HashMap<TenantOrganizationId, usize>,
    pub hosts_usable: usize,
    pub hosts_total: usize,
    /// Aggregate health-related metrics for all hosts, keyed by tenant
    /// assignment status.
    pub health: HealthIterationMetrics<IsInUseByTenant>,
    /// Mapping from SKU ID to the amount of hosts which have the SKU configured
    pub hosts_by_sku: HashMap<(String, String), usize>,
    pub hosts_with_bios_password_set: usize,
    pub last_machine_validation_list: HashMap<(String, String), i32>,
    pub hosts_with_scout_heartbeat_timeout: HashSet<String>,
}

#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug, Default)]
pub struct IsInUseByTenant(pub bool);

impl HealthMetricDimension for IsInUseByTenant {
    fn key_values(&self) -> Vec<KeyValue> {
        vec![KeyValue::new("in_use", self.0.to_string())]
    }

    fn all_values() -> Vec<Self> {
        vec![IsInUseByTenant(true), IsInUseByTenant(false)]
    }
}

#[derive(Debug)]
pub struct MachineMetricsEmitter {
    machine_reboot_attempts_in_booting_with_discovery_image: Histogram<u64>,
    machine_reboot_attempts_in_failed_during_discovery: Histogram<u64>,
}

impl MetricsEmitter for MachineMetricsEmitter {
    type ObjectMetrics = MachineMetrics;
    type IterationMetrics = MachineStateControllerIterationMetrics;

    fn new(
        _object_type: &str,
        meter: &Meter,
        shared_metrics: SharedMetricsHolder<Self::IterationMetrics>,
    ) -> Self {
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_gpus_total_count")
                .with_description("Number of GPUs in the NICo deployment")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.gpus_total as u64, attrs);
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_usable_count")
                .with_description("Number of remaining hosts in the NICo deployment available for immediate instance creation")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics.hosts_usable as u64,
                            attrs,
                        );
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_gpus_usable_count")
                .with_description("Number of remaining GPUs in the NICo deployment available for immediate instance creation")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics.gpus_usable as u64,
                            attrs,
                        );
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_gpus_in_use_count")
                .with_description(
                    "Number of GPUs actively used by tenants in instances in the NICo deployment",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let total_in_use_gpus = metrics
                            .gpus_in_use_by_tenant
                            .values()
                            .copied()
                            .reduce(|a, b| a + b)
                            .unwrap_or_default();
                        observer.observe(total_in_use_gpus as u64, attrs);
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_in_use_count")
                .with_description(
                    "Number of hosts actively used by tenants as instances in the NICo deployment",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let total_in_use_hosts = metrics
                            .hosts_in_use_by_tenant
                            .values()
                            .copied()
                            .reduce(|a, b| a + b)
                            .unwrap_or_default();
                        observer.observe(total_in_use_hosts as u64, attrs);
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_gpus_in_use_by_tenant_count")
                .with_description(
                    "Number of GPUs actively used by tenants as instances - by tenant",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (org, count) in &metrics.gpus_in_use_by_tenant {
                            observer.observe(
                                *count as u64,
                                &[attrs, &[KeyValue::new("tenant_org_id", org.to_string())]]
                                    .concat(),
                            );
                        }
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_in_use_by_tenant_count")
                .with_description(
                    "Number of hosts actively used by tenants as instances - by tenant",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (org, count) in &metrics.hosts_in_use_by_tenant {
                            observer.observe(
                                *count as u64,
                                &[attrs, &[KeyValue::new("tenant_org_id", org.to_string())]]
                                    .concat(),
                            );
                        }
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_dpus_up_count")
                .with_description("Number of DPUs in the system that are up. Up means we have received a health report less than 5 minutes ago.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics.dpus_up as u64,
                            attrs,
                        );
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_dpus_healthy_count")
                .with_description("Number of DPUs in the system that have reported healthy in the last report. Healthy does not imply up - the report from the DPU might be outdated.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics.dpus_healthy as u64,
                            attrs,
                        );
                    })
                })
                .build()
        };
        register_health_gauges::<_, IsInUseByTenant, _>(
            "carbide_hosts",
            "machine_id",
            "managed hosts",
            meter,
            shared_metrics.clone(),
            |m| &m.health,
        );

        // Deprecation warning:
        // `carbide_alerts_suppressed_count` before the metric was renamed to
        // `carbide_hosts_alerts_suppressed_count`.
        // Will be remvoed in future
        register_alerts_suppressed_gauge(
            "carbide_alerts_suppressed_count",
            "machine_id",
            meter,
            shared_metrics.clone(),
            |m| &m.health.alerts_suppressed_by_object_id,
        );

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_dpu_health_check_failed_count")
                .with_description("Number of DPUs in the system that have failed a health-check.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((probe, target), count) in &metrics.unhealthy_dpus_by_probe_id {
                            let failure = match target {
                                None => probe.to_string(),
                                Some(target) => format!("{probe} [Target: {target}]"),
                            };
                            observer.observe(
                                *count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("failure", failure.clone()),
                                        KeyValue::new("probe_id", probe.clone()),
                                        KeyValue::new(
                                            "probe_target",
                                            target.clone().unwrap_or_default(),
                                        ),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_by_sku_count")
                .with_description(
                    "Number of hosts by SKU and device type ('unknown' for hosts without SKU)",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((sku, device_type), count) in metrics.hosts_by_sku.iter() {
                            observer.observe(
                                *count as u64,
                                &[
                                    attrs,
                                    &[KeyValue::new("sku", sku.clone())],
                                    &[KeyValue::new("device_type", device_type.clone())],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_dpu_agent_version_count")
                .with_description("Number of DPU agents which have reported a certain version.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (version, count) in &metrics.agent_versions {
                            // TODO: Can prometheus labels hold arbitrary strings?
                            // Since there is no `try_into()` into method for those values,
                            // we assume OpenTelemetry escapes them internally
                            observer.observe(
                                *count as u64,
                                &[attrs, &[KeyValue::new("version", version.clone())]].concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_dpu_firmware_version_count")
                .with_description("Number of DPUs which have reported a certain firmware version.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (version, count) in &metrics.dpu_firmware_versions {
                            observer.observe(
                                *count as u64,
                                &[attrs, &[KeyValue::new("firmware_version", version.clone())]]
                                    .concat(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_machine_inventory_component_version_count")
                .with_description(
                    "Number of machines that report software components with a certain version.",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (component, count) in &metrics.machine_inventory_component_versions {
                            observer.observe(
                                *count as u64,
                                [
                                    attrs,
                                    &[
                                        KeyValue::new("name", component.name.clone()),
                                        KeyValue::new("version", component.version.clone()),
                                    ],
                                ]
                                .concat()
                                .as_ref(),
                            );
                        }
                    })
                })
                .build()
        };

        {
            let metrics = shared_metrics.clone();
            meter
                .i64_observable_gauge("carbide_dpu_client_certificate_expiration_time")
                .with_description("The expiration time (epoch seconds) for the client certificate associated with a given DPU.")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        // Placeholder that is replaced in the loop in order not having to reallocate the Vec each time
                        for (id, time) in &metrics.client_certificate_expiration_times {
                            observer.observe(
                                *time,
                                &[attrs, &[
                                    KeyValue::new("dpu_machine_id", id.clone()),
                                ]].concat()
                            );
                        }
                    })
                })
                .build()
        };

        let machine_reboot_attempts_in_booting_with_discovery_image = meter
            .u64_histogram("carbide_reboot_attempts_in_booting_with_discovery_image")
            .with_description("Reboot attempts per machine in BootingWithDiscoveryImage, recorded when a machine is rebooted again after no response from the host.")
            .build();

        let machine_reboot_attempts_in_failed_during_discovery = meter
            .u64_histogram("carbide_reboot_attempts_in_failed_during_discovery")
            .with_description("Reboot attempts per machine in Failed state due to discovery failure, recorded when a machine is rebooted again after no response from the host.")
            .build();

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_with_bios_password_set")
                .with_description(
                    "Number of hosts in the system that have their BIOS password set.",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.hosts_with_bios_password_set as u64, attrs);
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_hosts_with_scout_heartbeat_timeout")
                .with_description("Scout heartbeat timeout status for hosts")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for machine_id in &metrics.hosts_with_scout_heartbeat_timeout {
                            observer.observe(
                                1u64,
                                &[
                                    attrs,
                                    &[KeyValue::new("host_machine_id", machine_id.clone())],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };
        {
            let metrics = shared_metrics;
            meter
                .u64_observable_gauge("carbide_machine_validation_tests_on_machines")
                .with_description(
                    "For a given context the count of machine validation tests failed.",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((machine_id, context), status) in
                            metrics.last_machine_validation_list.iter()
                        {
                            observer.observe(
                                *status as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("machine_id", machine_id.clone()),
                                        KeyValue::new("context", context.clone()),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build()
        };
        Self {
            machine_reboot_attempts_in_booting_with_discovery_image,
            machine_reboot_attempts_in_failed_during_discovery,
        }
    }

    fn merge_object_handling_metrics(
        iteration_metrics: &mut Self::IterationMetrics,
        object_metrics: &Self::ObjectMetrics,
    ) {
        iteration_metrics.hosts_total += 1;
        iteration_metrics.dpus_up += object_metrics.dpus_up;
        iteration_metrics.dpus_healthy += object_metrics.dpus_healthy;

        let is_assigned = IsInUseByTenant(object_metrics.in_use_by_tenant.is_some());
        iteration_metrics
            .health
            .merge(is_assigned, &object_metrics.health);

        iteration_metrics.gpus_total += object_metrics.num_gpus;
        if object_metrics.is_usable_as_instance {
            iteration_metrics.hosts_usable += 1;
            iteration_metrics.gpus_usable += object_metrics.num_gpus;
        }

        // The object_metrics.is_host_bios_password_set bool cast as usize will translate to 0 or 1
        iteration_metrics.hosts_with_bios_password_set +=
            object_metrics.is_host_bios_password_set as usize;

        if let Some(machine_id) = &object_metrics.host_with_scout_heartbeat_timeout {
            iteration_metrics
                .hosts_with_scout_heartbeat_timeout
                .insert(machine_id.clone());
        }

        if let Some(tenant) = object_metrics.in_use_by_tenant.as_ref() {
            *iteration_metrics
                .gpus_in_use_by_tenant
                .entry(tenant.clone())
                .or_default() += object_metrics.num_gpus;
            *iteration_metrics
                .hosts_in_use_by_tenant
                .entry(tenant.clone())
                .or_default() += 1;
        }

        for ((probe_id, target), count) in &object_metrics.dpu_health_probe_alerts {
            *iteration_metrics
                .unhealthy_dpus_by_probe_id
                .entry((probe_id.to_string(), target.clone()))
                .or_default() += count;
        }

        // Record SKU information for all hosts, using "unknown" for hosts without SKU
        let sku = object_metrics
            .sku
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let device_type = object_metrics
            .sku_device_type
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        *iteration_metrics
            .hosts_by_sku
            .entry((sku, device_type))
            .or_default() += 1;

        for (version, count) in object_metrics.agent_versions.iter() {
            *iteration_metrics
                .agent_versions
                .entry(version.clone())
                .or_default() += count;
        }

        for (version, count) in object_metrics.dpu_firmware_versions.iter() {
            *iteration_metrics
                .dpu_firmware_versions
                .entry(version.clone())
                .or_default() += count;
        }

        for (component, count) in object_metrics.machine_inventory_component_versions.iter() {
            *iteration_metrics
                .machine_inventory_component_versions
                .entry(component.clone())
                .or_default() += count;
        }

        for (machine_id, maybe_time) in object_metrics.client_certificate_expiry.iter() {
            if let Some(time) = maybe_time {
                iteration_metrics
                    .client_certificate_expiration_times
                    .entry(machine_id.clone())
                    .and_modify(|entry| *entry = *time)
                    .or_insert(*time);
            }
        }

        for ((machine_id, context), status) in object_metrics.last_machine_validation_list.iter() {
            iteration_metrics
                .last_machine_validation_list
                .entry((machine_id.clone(), context.clone()))
                .or_insert_with(|| *status);
        }
    }

    fn emit_object_counters_and_histograms(&self, object_metrics: &Self::ObjectMetrics) {
        if let Some(machine_reboot_attempts_in_booting_with_discovery_image) =
            object_metrics.machine_reboot_attempts_in_booting_with_discovery_image
        {
            self.machine_reboot_attempts_in_booting_with_discovery_image
                .record(machine_reboot_attempts_in_booting_with_discovery_image, &[]);
        }

        if let Some(machine_reboot_attempts_in_failed_during_discovery) =
            object_metrics.machine_reboot_attempts_in_failed_during_discovery
        {
            self.machine_reboot_attempts_in_failed_during_discovery
                .record(machine_reboot_attempts_in_failed_during_discovery, &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_machine_metrics() {
        let object_metrics = vec![
            MachineMetrics {
                agent_versions: HashMap::new(),
                num_gpus: 0,
                in_use_by_tenant: Some("a".parse().unwrap()),
                dpus_up: 1,
                dpus_healthy: 0,
                dpu_health_probe_alerts: HashMap::from_iter([(
                    ("FileExists".parse().unwrap(), Some("def.txt".to_string())),
                    1,
                )]),
                dpu_firmware_versions: HashMap::new(),
                machine_inventory_component_versions: HashMap::new(),
                client_certificate_expiry: HashMap::from_iter([("machine a".to_string(), Some(1))]),
                machine_reboot_attempts_in_booting_with_discovery_image: None,
                machine_reboot_attempts_in_failed_during_discovery: None,
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: HashSet::from_iter([(
                        "FileExists".parse().unwrap(),
                        Some("def.txt".to_string()),
                    )]),
                    health_alert_classifications: HashSet::new(),
                    alerts_suppressed: false,
                    num_merge_overrides: 0,
                    replace_override_enabled: false,
                },
                is_usable_as_instance: true,
                is_host_bios_password_set: true,
                last_machine_validation_list: HashMap::new(),
                sku: None,
                sku_device_type: None,
                host_with_scout_heartbeat_timeout: None,
            },
            MachineMetrics {
                num_gpus: 2,
                in_use_by_tenant: Some("a".parse().unwrap()),
                agent_versions: HashMap::from_iter([("v1".to_string(), 1)]),
                dpus_up: 1,
                dpus_healthy: 0,
                dpu_health_probe_alerts: HashMap::from_iter([
                    (("bgp".parse().unwrap(), None), 1),
                    (("ntp".parse().unwrap(), None), 1),
                    (
                        ("FileExists".parse().unwrap(), Some("def.txt".to_string())),
                        1,
                    ),
                    (
                        ("FileExists".parse().unwrap(), Some("abc.txt".to_string())),
                        1,
                    ),
                ]),
                dpu_firmware_versions: HashMap::new(),
                machine_inventory_component_versions: HashMap::from_iter([(
                    MachineInventorySoftwareComponent {
                        name: "doca_hbn".to_string(),
                        version: "2.0.0-doca2.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    1,
                )]),
                client_certificate_expiry: HashMap::from_iter([("machine a".to_string(), Some(2))]),
                machine_reboot_attempts_in_booting_with_discovery_image: Some(0),
                machine_reboot_attempts_in_failed_during_discovery: Some(0),
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: HashSet::from_iter([
                        ("bgp".parse().unwrap(), None),
                        ("ntp".parse().unwrap(), None),
                        ("FileExists".parse().unwrap(), Some("def.txt".to_string())),
                        ("FileExists".parse().unwrap(), Some("abc.txt".to_string())),
                    ]),
                    health_alert_classifications: [
                        "Class1".parse().unwrap(),
                        "Class3".parse().unwrap(),
                    ]
                    .into_iter()
                    .collect(),
                    alerts_suppressed: false,
                    num_merge_overrides: 0,
                    replace_override_enabled: false,
                },
                is_usable_as_instance: true,
                is_host_bios_password_set: true,
                last_machine_validation_list: HashMap::from_iter([(
                    ("machine a".to_string(), "context".to_string()),
                    1,
                )]),
                sku: Some("SkuA".to_string()),
                sku_device_type: Some("DeviceTypeA".to_string()),
                host_with_scout_heartbeat_timeout: Some("machine_b".to_string()),
            },
            MachineMetrics {
                num_gpus: 3,
                in_use_by_tenant: None,
                agent_versions: HashMap::from_iter([("v3".to_string(), 1)]),
                dpus_up: 0,
                dpus_healthy: 1,
                dpu_health_probe_alerts: HashMap::from_iter([]),
                dpu_firmware_versions: HashMap::from_iter([("v4".to_string(), 1)]),
                machine_inventory_component_versions: HashMap::from_iter([(
                    MachineInventorySoftwareComponent {
                        name: "doca_telemetry".to_string(),
                        version: "1.15.5-doca2.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    1,
                )]),
                client_certificate_expiry: HashMap::from_iter([("machine b".to_string(), Some(3))]),
                machine_reboot_attempts_in_booting_with_discovery_image: Some(1),
                machine_reboot_attempts_in_failed_during_discovery: Some(1),
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: HashSet::new(),
                    health_alert_classifications: HashSet::new(),
                    alerts_suppressed: false,
                    num_merge_overrides: 1,
                    replace_override_enabled: true,
                },
                is_usable_as_instance: false,
                is_host_bios_password_set: true,
                last_machine_validation_list: HashMap::new(),
                sku: Some("SkuA".to_string()),
                sku_device_type: Some("DeviceTypeA".to_string()),
                host_with_scout_heartbeat_timeout: None,
            },
            MachineMetrics {
                num_gpus: 1,
                in_use_by_tenant: Some("a".parse().unwrap()),
                agent_versions: HashMap::from_iter([("v3".to_string(), 1)]),
                dpus_up: 1,
                dpus_healthy: 1,
                dpu_health_probe_alerts: HashMap::from_iter([]),
                dpu_firmware_versions: HashMap::from_iter([("v2".to_string(), 1)]),
                machine_inventory_component_versions: HashMap::from_iter([
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_hbn".to_string(),
                            version: "2.0.0-doca2.5.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        1,
                    ),
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_telemetry".to_string(),
                            version: "1.15.5-doca2.5.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        1,
                    ),
                ]),
                client_certificate_expiry: HashMap::from_iter([("machine b".to_string(), None)]),
                machine_reboot_attempts_in_booting_with_discovery_image: Some(2),
                machine_reboot_attempts_in_failed_during_discovery: Some(2),
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: HashSet::new(),
                    health_alert_classifications: HashSet::new(),
                    alerts_suppressed: false,
                    num_merge_overrides: 0,
                    replace_override_enabled: false,
                },
                is_usable_as_instance: true,
                is_host_bios_password_set: true,
                last_machine_validation_list: HashMap::new(),
                sku: Some("SkuB".to_string()),
                sku_device_type: Some("DeviceTypeA".to_string()),
                host_with_scout_heartbeat_timeout: Some("machine_d".to_string()),
            },
            MachineMetrics {
                num_gpus: 2,
                in_use_by_tenant: None,
                agent_versions: HashMap::new(),
                dpus_up: 1,
                dpus_healthy: 0,
                dpu_health_probe_alerts: [
                    (("BgpStats".parse().unwrap(), None), 1),
                    (
                        (
                            "HeartbeatTimeout".parse().unwrap(),
                            Some("forge-dpu-agent".to_string()),
                        ),
                        1,
                    ),
                ]
                .into_iter()
                .collect(),
                dpu_firmware_versions: HashMap::from_iter([("v4".to_string(), 1)]),
                machine_inventory_component_versions: HashMap::from_iter([
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_hbn".to_string(),
                            version: "3.0.0-doca3.5.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        1,
                    ),
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_telemetry".to_string(),
                            version: "3.15.5-doca3.5.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        1,
                    ),
                ]),
                client_certificate_expiry: HashMap::default(),
                machine_reboot_attempts_in_booting_with_discovery_image: None,
                machine_reboot_attempts_in_failed_during_discovery: None,
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: [
                        ("BgpStats".parse().unwrap(), None),
                        (
                            "HeartbeatTimeout".parse().unwrap(),
                            Some("forge-dpu-agent".to_string()),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                    health_alert_classifications: [
                        "Class1".parse().unwrap(),
                        "Class2".parse().unwrap(),
                    ]
                    .into_iter()
                    .collect(),
                    alerts_suppressed: false,
                    num_merge_overrides: 1,
                    replace_override_enabled: false,
                },
                is_usable_as_instance: false,
                is_host_bios_password_set: true,
                last_machine_validation_list: HashMap::new(),
                sku: Some("SkuC".to_string()),
                sku_device_type: Some("DeviceTypeC".to_string()),
                host_with_scout_heartbeat_timeout: None,
            },
            MachineMetrics {
                num_gpus: 3,
                in_use_by_tenant: None,
                agent_versions: HashMap::new(),
                dpus_up: 2,
                dpus_healthy: 0,
                dpu_health_probe_alerts: HashMap::from_iter([(
                    ("BgpStats".parse().unwrap(), None),
                    2,
                )]),
                dpu_firmware_versions: HashMap::from_iter([
                    ("v4".to_string(), 1),
                    ("v5".to_string(), 1),
                ]),
                machine_inventory_component_versions: HashMap::from_iter([
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_hbn".to_string(),
                            version: "3.0.0-doca3.6.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        2,
                    ),
                    (
                        MachineInventorySoftwareComponent {
                            name: "doca_telemetry".to_string(),
                            version: "3.15.5-doca3.6.0".to_string(),
                            url: "nvcr.io/nvidia/doca".to_string(),
                        },
                        2,
                    ),
                ]),
                client_certificate_expiry: HashMap::default(),
                machine_reboot_attempts_in_booting_with_discovery_image: None,
                machine_reboot_attempts_in_failed_during_discovery: None,
                health: HealthObjectMetrics {
                    object_id: "".to_string(),
                    health_probe_alerts: [("BgpStats".parse().unwrap(), None)]
                        .into_iter()
                        .collect(),
                    health_alert_classifications: [
                        "Class1".parse().unwrap(),
                        "Class2".parse().unwrap(),
                    ]
                    .into_iter()
                    .collect(),
                    alerts_suppressed: false,
                    num_merge_overrides: 0,
                    replace_override_enabled: true,
                },
                is_usable_as_instance: false,
                is_host_bios_password_set: false,
                last_machine_validation_list: HashMap::new(),
                sku: Some("SkuC".to_string()),
                sku_device_type: Some("DeviceTypeC".to_string()),
                host_with_scout_heartbeat_timeout: Some("machine_f".to_string()),
            },
        ];

        let mut iteration_metrics = MachineStateControllerIterationMetrics::default();
        for om in &object_metrics {
            MachineMetricsEmitter::merge_object_handling_metrics(&mut iteration_metrics, om);
        }

        assert_eq!(
            iteration_metrics.agent_versions,
            HashMap::from_iter([("v1".to_string(), 1), ("v3".to_string(), 2)])
        );
        assert_eq!(
            iteration_metrics
                .last_machine_validation_list
                .get(&("machine a".to_string(), "context".to_string(),)),
            Some(&1)
        );

        assert_eq!(
            *iteration_metrics
                .gpus_in_use_by_tenant
                .get(&"a".parse().unwrap())
                .unwrap(),
            3
        );
        assert_eq!(
            *iteration_metrics
                .hosts_in_use_by_tenant
                .get(&"a".parse().unwrap())
                .unwrap(),
            3
        );
        assert_eq!(iteration_metrics.hosts_usable, 3);
        assert_eq!(iteration_metrics.hosts_with_bios_password_set, 5);
        assert_eq!(
            iteration_metrics.hosts_with_scout_heartbeat_timeout,
            HashSet::from_iter([
                "machine_b".to_string(),
                "machine_d".to_string(),
                "machine_f".to_string(),
            ])
        );
        assert_eq!(iteration_metrics.gpus_usable, 3);
        assert_eq!(iteration_metrics.gpus_total, 11);
        assert_eq!(iteration_metrics.dpus_up, 6);
        assert_eq!(iteration_metrics.dpus_healthy, 2);
        assert_eq!(
            iteration_metrics.unhealthy_dpus_by_probe_id,
            HashMap::from_iter([
                (("BgpStats".parse().unwrap(), None), 3),
                (("bgp".to_string(), None), 1),
                (("ntp".to_string(), None), 1),
                (("FileExists".to_string(), Some("abc.txt".to_string())), 1),
                (("FileExists".to_string(), Some("def.txt".to_string())), 2),
                (
                    (
                        "HeartbeatTimeout".parse().unwrap(),
                        Some("forge-dpu-agent".to_string()),
                    ),
                    1,
                ),
            ])
        );
        assert_eq!(
            iteration_metrics.dpu_firmware_versions,
            HashMap::from_iter([
                ("v2".to_string(), 1),
                ("v4".to_string(), 3),
                ("v5".to_string(), 1)
            ])
        );

        assert_eq!(iteration_metrics.hosts_total, 6);
        assert_eq!(
            iteration_metrics.health.healthy,
            HashMap::from_iter([
                ((true, IsInUseByTenant(true)), 1),
                ((false, IsInUseByTenant(true)), 2),
                ((true, IsInUseByTenant(false)), 1),
                ((false, IsInUseByTenant(false)), 2),
            ])
        );
        assert_eq!(
            iteration_metrics.health.unhealthy_by_probe_id,
            HashMap::from_iter([
                (
                    ("BgpStats".parse().unwrap(), None, IsInUseByTenant(false)),
                    2
                ),
                (("bgp".to_string(), None, IsInUseByTenant(true)), 1),
                (("ntp".to_string(), None, IsInUseByTenant(true)), 1),
                (
                    (
                        "FileExists".to_string(),
                        Some("abc.txt".to_string()),
                        IsInUseByTenant(true)
                    ),
                    1
                ),
                (
                    (
                        "FileExists".to_string(),
                        Some("def.txt".to_string()),
                        IsInUseByTenant(true)
                    ),
                    2
                ),
                (
                    (
                        "HeartbeatTimeout".parse().unwrap(),
                        Some("forge-dpu-agent".to_string()),
                        IsInUseByTenant(false)
                    ),
                    1,
                ),
            ])
        );
        assert_eq!(
            iteration_metrics.health.unhealthy_by_classification_id,
            HashMap::from_iter([
                (("Class1".parse().unwrap(), IsInUseByTenant(true)), 1),
                (("Class1".parse().unwrap(), IsInUseByTenant(false)), 2),
                (("Class2".parse().unwrap(), IsInUseByTenant(false)), 2),
                (("Class3".parse().unwrap(), IsInUseByTenant(true)), 1),
            ])
        );
        assert_eq!(
            iteration_metrics.health.num_overrides,
            HashMap::from_iter([
                (("merge", IsInUseByTenant(true)), 0),
                (("merge", IsInUseByTenant(false)), 2),
                (("replace", IsInUseByTenant(false)), 2),
            ])
        );

        assert_eq!(
            iteration_metrics.machine_inventory_component_versions,
            HashMap::from_iter([
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_hbn".to_string(),
                        version: "2.0.0-doca2.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    2
                ),
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_hbn".to_string(),
                        version: "3.0.0-doca3.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    1
                ),
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_hbn".to_string(),
                        version: "3.0.0-doca3.6.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    2
                ),
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_telemetry".to_string(),
                        version: "1.15.5-doca2.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    2
                ),
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_telemetry".to_string(),
                        version: "3.15.5-doca3.5.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    1
                ),
                (
                    MachineInventorySoftwareComponent {
                        name: "doca_telemetry".to_string(),
                        version: "3.15.5-doca3.6.0".to_string(),
                        url: "nvcr.io/nvidia/doca".to_string(),
                    },
                    2
                )
            ])
        );

        assert_eq!(
            iteration_metrics.hosts_by_sku,
            HashMap::from_iter([
                (("SkuA".to_string(), "DeviceTypeA".to_string()), 2),
                (("SkuB".to_string(), "DeviceTypeA".to_string()), 1),
                (("SkuC".to_string(), "DeviceTypeC".to_string()), 2),
                (("unknown".to_string(), "unknown".to_string()), 1),
            ])
        );

        assert_eq!(
            iteration_metrics.client_certificate_expiration_times,
            HashMap::from_iter([("machine a".to_string(), 2), ("machine b".to_string(), 3)])
        );
    }
}
