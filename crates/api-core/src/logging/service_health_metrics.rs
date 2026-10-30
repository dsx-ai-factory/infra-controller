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

use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use rpc::forge::ResourcePoolStatsStatus;

use crate::resource_pool::metrics::PoolStats;

pub(crate) struct ServiceHealthContext {
    pub(crate) meter: Meter,
    pub(crate) database_pool: sqlx::PgPool,
    pub(crate) resource_pool_stats: PoolStats,
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
                for (name, observation) in rp_stats.lock().unwrap().iter() {
                    if observation.latest_status != ResourcePoolStatsStatus::Known {
                        continue;
                    }
                    observation.last_success.if_available(|sample, attrs| {
                        if attrs.contains(&KeyValue::new("fresh", true))
                            && let Some(stats) = &sample.stats
                        {
                            observer.observe(stats.used, &[KeyValue::new("pool", name.clone())]);
                        }
                    });
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
                for (name, observation) in rp_stats.lock().unwrap().iter() {
                    if observation.latest_status != ResourcePoolStatsStatus::Known {
                        continue;
                    }
                    observation.last_success.if_available(|sample, attrs| {
                        if attrs.contains(&KeyValue::new("fresh", true))
                            && let Some(stats) = &sample.stats
                        {
                            observer.observe(stats.free, &[KeyValue::new("pool", name.clone())]);
                        }
                    });
                }
            })
            .build();
    }

    {
        let rp_stats = health_context.resource_pool_stats.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_resourcepool_stats_available")
            .with_description("Whether the last resource pool statistics poll returned known counts, regardless of age")
            .with_callback(move |observer| {
                for (name, observation) in rp_stats.lock().unwrap().iter() {
                    observer.observe(
                        u64::from(observation.latest_status == ResourcePoolStatsStatus::Known),
                        &[KeyValue::new("pool", name.clone())],
                    );
                }
            })
            .build();
    }

    {
        let rp_stats = health_context.resource_pool_stats.clone();
        health_context
            .meter
            .u64_observable_gauge("carbide_resourcepool_stats_fresh")
            .with_description("Whether the last successful resource pool statistics poll is less than 120 seconds old; zero before any success. Used/free count series are absent before success, after a failed poll, or when this gauge is zero")
            .with_callback(move |observer| {
                for (name, observation) in rp_stats.lock().unwrap().iter() {
                    let mut fresh = false;
                    observation.last_success.if_available(|_, attrs| {
                        fresh = attrs.contains(&KeyValue::new("fresh", true));
                    });
                    observer.observe(u64::from(fresh), &[KeyValue::new("pool", name.clone())]);
                }
            })
            .build();
    }

    {
        let rp_stats = health_context.resource_pool_stats;
        health_context
            .meter
            .f64_observable_gauge("carbide_resourcepool_stats_last_success_timestamp_seconds")
            .with_description("Unix timestamp of the last successful resource pool statistics poll, retained after failure and absent before any success")
            .with_callback(move |observer| {
                for (name, observation) in rp_stats.lock().unwrap().iter() {
                    observation.last_success.if_available(|sample, _| {
                        if let Some(timestamp) = &sample.observed_at {
                            observer.observe(
                                timestamp.seconds as f64 + f64::from(timestamp.nanos) / 1e9,
                                &[KeyValue::new("pool", name.clone())],
                            );
                        }
                    });
                }
            })
            .build();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use carbide_utils::metrics::SharedMetricsHolder;
    use carbide_utils::test_support::test_meter::TestMeter;
    use prometheus_text_parser::ParsedPrometheusMetrics;
    use rpc::forge as rpc;
    use sqlx::PgPool;

    use super::{ServiceHealthContext, start_export_service_health_metrics};
    use crate::resource_pool::metrics::{PoolStats, StatsObservation};

    /// Existing count labels stay intact, while failure and aging hide counts
    /// without discarding the successful observation or inventing an empty pool.
    #[crate::sqlx_test]
    async fn test_service_health_metrics(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let test_meter = TestMeter::default();
        let pool_stats: PoolStats = Arc::new(Mutex::new(HashMap::from([
            ("pool1".to_owned(), StatsObservation::default()),
            ("pool2".to_owned(), StatsObservation::default()),
        ])));
        let context = ServiceHealthContext {
            meter: test_meter.meter(),
            database_pool: pool,
            resource_pool_stats: pool_stats.clone(),
        };
        start_export_service_health_metrics(context);

        // Before the first reply, availability and freshness are false. Counts
        // and success timestamps must be absent rather than reported as zero.
        for metric in [
            "carbide_resourcepool_used_count",
            "carbide_resourcepool_free_count",
            "carbide_resourcepool_stats_last_success_timestamp_seconds",
        ] {
            assert!(test_meter.formatted_metrics(metric).is_empty(), "{metric}");
        }
        for metric in [
            "carbide_resourcepool_stats_available",
            "carbide_resourcepool_stats_fresh",
        ] {
            assert_eq!(
                test_meter.formatted_metrics(metric),
                ["{pool=\"pool1\"} 0", "{pool=\"pool2\"} 0"],
                "{metric}"
            );
        }

        let samples = [sample("pool1", 10, 20, 1000), sample("pool2", 20, 10, 2000)];
        for sample in &samples {
            let mut stats = pool_stats.lock().unwrap();
            let observation = stats.get_mut(&sample.name).unwrap();
            observation.latest_status = rpc::ResourcePoolStatsStatus::Known;
            observation.last_success.update(sample.clone());
        }
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

        // One failed backend must neither hide the healthy pool nor keep
        // advertising its own old counts as current capacity.
        pool_stats
            .lock()
            .unwrap()
            .get_mut("pool1")
            .unwrap()
            .latest_status = rpc::ResourcePoolStatsStatus::Unavailable;
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_used_count"),
            ["{pool=\"pool2\"} 20"]
        );
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_free_count"),
            ["{pool=\"pool2\"} 10"]
        );
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_stats_available"),
            ["{pool=\"pool1\"} 0", "{pool=\"pool2\"} 1"]
        );
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_stats_fresh"),
            ["{pool=\"pool1\"} 1", "{pool=\"pool2\"} 1"]
        );

        // The holder uses std::time::Instant. Give these samples a short real
        // lifetime so this covers aging without a two-minute test or a fake
        // Tokio clock that cannot advance the holder's age.
        for sample in &samples {
            let mut stats = pool_stats.lock().unwrap();
            let observation = stats.get_mut(&sample.name).unwrap();
            observation.last_success =
                SharedMetricsHolder::with_fresh_period(Duration::from_millis(1));
            observation.last_success.update(sample.clone());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        for metric in [
            "carbide_resourcepool_used_count",
            "carbide_resourcepool_free_count",
        ] {
            assert!(test_meter.formatted_metrics(metric).is_empty(), "{metric}");
        }
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_stats_fresh"),
            ["{pool=\"pool1\"} 0", "{pool=\"pool2\"} 0"]
        );
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_stats_available"),
            ["{pool=\"pool1\"} 0", "{pool=\"pool2\"} 1"],
            "availability describes the last attempt even after a sample ages"
        );
        assert_eq!(
            test_meter
                .formatted_metrics("carbide_resourcepool_stats_last_success_timestamp_seconds"),
            ["{pool=\"pool1\"} 1000", "{pool=\"pool2\"} 2000"]
        );
        for sample in &samples {
            let mut retained = None;
            pool_stats.lock().unwrap()[&sample.name]
                .last_success
                .if_available(|sample, _| retained = Some(sample.clone()));
            assert_eq!(
                retained.as_ref(),
                Some(sample),
                "failed and stale samples remain in memory"
            );
        }

        // A later success replaces the retained sample and restores the count
        // series with the same labels, rather than keeping a separate stale one.
        for mut sample in [sample("pool1", 11, 19, 3000), sample("pool2", 21, 9, 4000)] {
            sample.observed_at.as_mut().unwrap().nanos = 500_000_000;
            let mut stats = pool_stats.lock().unwrap();
            let observation = stats.get_mut(&sample.name).unwrap();
            *observation = StatsObservation::default();
            observation.latest_status = rpc::ResourcePoolStatsStatus::Known;
            observation.last_success.update(sample);
        }
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_used_count"),
            ["{pool=\"pool1\"} 11", "{pool=\"pool2\"} 21"]
        );
        assert_eq!(
            test_meter.formatted_metrics("carbide_resourcepool_free_count"),
            ["{pool=\"pool1\"} 19", "{pool=\"pool2\"} 9"]
        );
        for metric in [
            "carbide_resourcepool_stats_available",
            "carbide_resourcepool_stats_fresh",
        ] {
            assert_eq!(
                test_meter.formatted_metrics(metric),
                ["{pool=\"pool1\"} 1", "{pool=\"pool2\"} 1"],
                "{metric}"
            );
        }
        assert_eq!(
            test_meter
                .formatted_metrics("carbide_resourcepool_stats_last_success_timestamp_seconds"),
            ["{pool=\"pool1\"} 3000.5", "{pool=\"pool2\"} 4000.5"]
        );

        Ok(())
    }

    /// A known observation with explicit counts and a stable success timestamp
    /// keeps assertions independent of wall-clock time.
    fn sample(name: &str, used: u64, free: u64, seconds: i64) -> rpc::ResourcePoolInspection {
        rpc::ResourcePoolInspection {
            name: name.to_owned(),
            stats: Some(rpc::ResourcePoolStats {
                used,
                free,
                auto_assign_used: used,
                auto_assign_free: free,
                ..Default::default()
            }),
            stats_status: rpc::ResourcePoolStatsStatus::Known as i32,
            observed_at: Some(prost_types::Timestamp { seconds, nanos: 0 }.into()),
            ..Default::default()
        }
    }
}
