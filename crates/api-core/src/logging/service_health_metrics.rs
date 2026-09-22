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
use std::sync::{Arc, Mutex};

use model::resource_pool::ResourcePoolStats;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

pub(crate) struct ServiceHealthContext {
    pub(crate) meter: Meter,
    pub(crate) database_pool: sqlx::PgPool,
    pub(crate) resource_pool_stats: Arc<Mutex<HashMap<String, ResourcePoolStats>>>,
}

/// Starts to export server health metrics
pub(crate) fn start_export_service_health_metrics(health_context: ServiceHealthContext) {
    health_context
        .meter
        .u64_observable_gauge("carbide_api_ready")
        .with_description("Whether the NICo API is running")
        .with_callback(|observer| {
            observer.observe(1, &[]);
        })
        .build();
    health_context
        .meter
        .u64_observable_gauge("carbide_api_version")
        .with_description("Version (git sha, build date, etc) of this service")
        .with_callback(|observer| {
            observer.observe(
                1,
                &[
                    KeyValue::new(
                        "build_version",
                        carbide_version::v!(build_version).to_string(),
                    ),
                    KeyValue::new("build_date", carbide_version::v!(build_date).to_string()),
                    KeyValue::new("git_sha", carbide_version::v!(git_sha).to_string()),
                    KeyValue::new(
                        "rust_version",
                        carbide_version::v!(rust_version).to_string(),
                    ),
                    KeyValue::new("build_user", carbide_version::v!(build_user).to_string()),
                    KeyValue::new(
                        "build_hostname",
                        carbide_version::v!(build_hostname).to_string(),
                    ),
                ],
            );
        })
        .build();

    {
        let database_pool = health_context.database_pool.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_db_pool_idle_conns")
            .with_description("Number of idle connections in the carbide database pool")
            .with_callback(move |observer| {
                observer.observe(database_pool.num_idle() as u64, &[]);
            })
            .build();
    }

    {
        let database_pool = health_context.database_pool.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_db_pool_total_conns")
            .with_description("Number of (active + idle) connections in the carbide database pool")
            .with_callback(move |observer| {
                observer.observe(database_pool.size() as u64, &[]);
            })
            .build();
    }

    {
        let rp_stats = health_context.resource_pool_stats.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_resourcepool_used_count")
            .with_description("Number of currently allocated values in the resource pool")
            .with_callback(move |observer| {
                for (name, stats) in rp_stats.lock().unwrap().iter() {
                    observer.observe(
                        stats.used as u64,
                        &[KeyValue::new("pool", name.to_string())],
                    );
                }
            })
            .build();
    }

    {
        let rp_stats = health_context.resource_pool_stats.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_resourcepool_free_count")
            .with_description(
                "Number of values in the resource pool currently available for allocation",
            )
            .with_callback(move |observer| {
                for (name, stats) in rp_stats.lock().unwrap().iter() {
                    let name_attr = KeyValue::new("pool", name.to_string());
                    observer.observe(stats.free as u64, std::slice::from_ref(&name_attr));
                }
            })
            .build();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use carbide_utils::test_support::test_meter::TestMeter;
    use model::resource_pool::ResourcePoolStats;
    use prometheus_text_parser::ParsedPrometheusMetrics;
    use sqlx::PgPool;

    use super::{ServiceHealthContext, start_export_service_health_metrics};

    #[crate::sqlx_test]
    async fn test_service_health_metrics(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let test_meter = TestMeter::default();
        let context = ServiceHealthContext {
            meter: test_meter.meter(),
            database_pool: pool,
            resource_pool_stats: Arc::new(Mutex::new(HashMap::from([
                (
                    "pool1".to_string(),
                    ResourcePoolStats {
                        used: 10,
                        free: 20,
                        auto_assign_free: 20,
                        auto_assign_used: 10,
                        non_auto_assign_free: 0,
                        non_auto_assign_used: 0,
                    },
                ),
                (
                    "pool2".to_string(),
                    ResourcePoolStats {
                        used: 20,
                        free: 10,
                        auto_assign_free: 10,
                        auto_assign_used: 20,
                        non_auto_assign_free: 0,
                        non_auto_assign_used: 0,
                    },
                ),
            ]))),
        };
        start_export_service_health_metrics(context);

        let expected_metrics = include_str!("test_data/test_service_health_metrics.txt")
            .parse::<ParsedPrometheusMetrics>()
            .unwrap()
            .scrub_build_attributes();
        let metrics = test_meter
            .export_metrics()
            .parse::<ParsedPrometheusMetrics>()
            .unwrap()
            .scrub_build_attributes();

        assert_eq!(expected_metrics, metrics);

        Ok(())
    }
}
