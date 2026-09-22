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
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::future::join_all;

use super::context::{CollectorKind, DiscoveryLoopContext};
use super::spawn::collector_eligibility;
use crate::HealthError;
use crate::collectors::{
    Collector, CollectorStartContext, NMXT_PORT, REACHABILITY_COLLECTOR_TYPE,
    ReachabilityCollector, ReachabilityCollectorStartConfig, ReachabilityService,
    ReachabilityTarget,
};
use crate::endpoint::BmcEndpoint;
use crate::limiter::NoopLimiter;
use crate::sink::{DataSink, EventContext};

const DEFAULT_HTTPS_PORT: u16 = 443;

/// Desired targets and event context for one endpoint.
#[derive(Clone, PartialEq)]
pub(super) struct ReachabilitySpec {
    targets: BTreeMap<ReachabilityService, ReachabilityTarget>,
    event_context: EventContext,
}

/// Builds one order-independent reachability specification per endpoint key.
///
/// When sources repeat a key, the first eligible target for each service wins.
fn desired_reachability_specs(
    ctx: &DiscoveryLoopContext,
    endpoints: &[Arc<BmcEndpoint>],
) -> HashMap<Cow<'static, str>, (Arc<BmcEndpoint>, ReachabilitySpec)> {
    let mut desired = HashMap::new();

    for endpoint in endpoints {
        let targets = resolve_targets(endpoint, ctx, true);

        if targets.is_empty() {
            continue;
        }

        let key: Cow<'static, str> = Cow::Owned(endpoint.key());

        let (_, spec) = desired.entry(key).or_insert_with(|| {
            (
                endpoint.clone(),
                ReachabilitySpec {
                    targets: BTreeMap::new(),
                    event_context: EventContext::from_endpoint(
                        endpoint,
                        REACHABILITY_COLLECTOR_TYPE,
                    ),
                },
            )
        });

        for target in targets {
            spec.targets.entry(target.service).or_insert(target);
        }
    }

    desired
}

/// Reconciles reachability collectors against the latest discovered endpoints.
///
/// Duplicate source records may contribute different eligible services for the
/// same key. Replacements await shutdown before starting so stale metric
/// cleanup cannot remove new series.
pub(super) async fn reconcile_reachability_collectors(
    ctx: &mut DiscoveryLoopContext,
    endpoints: &[Arc<BmcEndpoint>],
    sink: Option<Arc<dyn DataSink>>,
    metrics_prefix: &str,
) -> Result<(), HealthError> {
    let config = ctx.reachability_config.clone();

    let desired = if config.is_some() && sink.is_some() {
        desired_reachability_specs(ctx, endpoints)
    } else {
        HashMap::new()
    };

    let stale_keys = ctx
        .collectors
        .reachability_specs
        .iter()
        .filter_map(|(key, current)| match desired.get(key) {
            Some((_, next)) if next == current => None,
            _ => Some(key.clone()),
        })
        .collect::<Vec<_>>();

    let stale_collectors = {
        let collectors = ctx.collectors.map_mut(CollectorKind::Reachability);

        stale_keys
            .iter()
            .filter_map(|key| collectors.remove(key))
            .collect::<Vec<_>>()
    };

    for key in &stale_keys {
        ctx.collectors.reachability_specs.remove(key);
    }

    join_all(stale_collectors.into_iter().map(Collector::stop)).await;

    let (Some(config), Some(sink)) = (config, sink) else {
        return Ok(());
    };

    for (key, (endpoint, spec)) in desired {
        if ctx.collectors.contains(CollectorKind::Reachability, &key) {
            continue;
        }

        let registry_key = endpoint.addr.registry_key();
        let collector_registry = Arc::new(ctx.metrics_manager.create_collector_registry(
            format!("reachability_collector_{registry_key}"),
            metrics_prefix,
        )?);

        match Collector::start::<ReachabilityCollector>(
            endpoint.clone(),
            endpoint.bmc().clone(),
            ReachabilityCollectorStartConfig {
                targets: spec.targets.values().cloned().collect(),
                timeout: config.timeout,
                log_mode: ctx.log_event_sink_enabled.then_some(config.log_mode),
                sink: sink.clone(),
            },
            CollectorStartContext {
                // TCP probes use their configured cadence independently of
                // protocol collector rate limits.
                limiter: Arc::new(NoopLimiter),
                iteration_interval: config.interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(collector) => {
                ctx.collectors.reachability_specs.insert(key.clone(), spec);
                ctx.collectors
                    .insert(CollectorKind::Reachability, key.clone(), collector);

                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Started TCP reachability collection"
                );
            }

            Err(error) => {
                // An absent entry is retried on the next discovery pass.
                tracing::error!(
                    ?error,
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start TCP reachability collection"
                );
            }
        }
    }

    Ok(())
}

/// Maps shared collector eligibility and effective ports to TCP targets.
fn resolve_targets(
    endpoint: &BmcEndpoint,
    ctx: &DiscoveryLoopContext,
    data_sink_present: bool,
) -> Vec<ReachabilityTarget> {
    let eligibility = collector_eligibility(ctx, endpoint, data_sink_present);

    if eligibility.redfish {
        // Probe the discovered BMC directly, not its configured proxy.
        return vec![ReachabilityTarget {
            service: ReachabilityService::Redfish,
            address: (
                endpoint.addr.ip,
                endpoint.addr.port.unwrap_or(DEFAULT_HTTPS_PORT),
            )
                .into(),
        }];
    }

    let mut targets = Vec::new();

    if eligibility.nvue_rest {
        targets.push(ReachabilityTarget {
            service: ReachabilityService::NvueRest,
            address: (
                endpoint.addr.ip,
                endpoint.addr.port.unwrap_or(DEFAULT_HTTPS_PORT),
            )
                .into(),
        });
    }

    if eligibility.nvue_gnmi
        && let Some(gnmi) = ctx
            .nvue_config
            .as_option()
            .and_then(|config| config.gnmi.as_option())
    {
        targets.push(ReachabilityTarget {
            service: ReachabilityService::Gnmi,
            address: (endpoint.addr.ip, gnmi.gnmi_port).into(),
        });
    }

    if eligibility.nmxt {
        targets.push(ReachabilityTarget {
            service: ReachabilityService::Nmxt,
            address: (endpoint.addr.ip, NMXT_PORT).into(),
        });
    }

    if eligibility.nmxc
        && let Some(nmxc) = ctx.nmxc_config.as_option()
    {
        targets.push(ReachabilityTarget {
            service: ReachabilityService::Nmxc,
            address: (endpoint.addr.ip, nmxc.grpc_port).into(),
        });
    }

    targets
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;
    use crate::config::{
        Config, Configurable, NmxcCollectorConfig, NvueCollectorConfig, ReachabilityCollectorConfig,
    };
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{EndpointMetadata, SwitchData, SwitchEndpointRole};
    use crate::metrics::MetricsManager;
    use crate::sink::{CollectorEvent, EventContext};

    #[derive(Default)]
    struct NotifyingSink(tokio::sync::Notify);

    impl DataSink for NotifyingSink {
        fn sink_type(&self) -> &'static str {
            "notifying"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            self.0.notify_one();
            Ok(())
        }
    }

    fn switch_endpoint(role: SwitchEndpointRole) -> BmcEndpoint {
        let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        endpoint.addr.ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        endpoint.addr.port = Some(9443);

        endpoint.metadata = Some(EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: "switch-1".to_string(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role: role,
            is_primary: true,
            nmxc_enabled: true,
            nmxt_enabled: true,
        }));

        endpoint
    }

    fn config_without_target_collectors() -> Config {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.metrics = Configurable::Disabled;
        config.collectors.telemetry = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.gpu_inventory = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Disabled;
        config.endpoint_sources.carbide_api = Configurable::Disabled;
        config
    }

    fn targets_for_config(
        endpoint: &BmcEndpoint,
        config: &Config,
        data_sink_present: bool,
    ) -> Vec<ReachabilityTarget> {
        let ctx = DiscoveryLoopContext::new(
            Arc::new(NoopLimiter),
            Arc::new(MetricsManager::new("reachability_targets").expect("metrics manager")),
            Arc::new(config.clone()),
        )
        .expect("discovery context");

        resolve_targets(endpoint, &ctx, data_sink_present)
    }

    #[test]
    fn redfish_targets_follow_collector_spawn_gates() {
        let mut bmc = test_endpoint(mac("00:11:22:33:44:54"));
        bmc.addr.port = Some(8443);

        let base = config_without_target_collectors();

        let expected = vec![ReachabilityTarget {
            service: ReachabilityService::Redfish,
            address: (bmc.addr.ip, 8443).into(),
        }];

        assert!(targets_for_config(&bmc, &base, true).is_empty());

        let mut sensor_config = base;
        sensor_config.collectors.sensors = Configurable::Enabled(Default::default());

        assert_eq!(targets_for_config(&bmc, &sensor_config, true), expected);

        let mut telemetry_config = config_without_target_collectors();
        telemetry_config.collectors.telemetry = Configurable::Enabled(Default::default());

        assert_eq!(targets_for_config(&bmc, &telemetry_config, true), expected);

        let mut log_config = config_without_target_collectors();
        log_config.collectors.logs = Configurable::Enabled(Default::default());

        assert!(
            targets_for_config(&bmc, &log_config, false).is_empty(),
            "auto log collection cannot start without a data sink",
        );

        let Configurable::Enabled(logs) = &mut log_config.collectors.logs else {
            panic!("logs should be enabled");
        };

        logs.mode = crate::config::LogCollectionMode::Periodic;

        assert_eq!(
            targets_for_config(&bmc, &log_config, false),
            expected,
            "periodic log collection starts without a data sink",
        );
    }

    #[test]
    fn switch_targets_follow_spawn_gates_and_effective_ports() {
        let mut config = config_without_target_collectors();
        config.collectors.sensors = Configurable::Enabled(Default::default());

        config.collectors.nvue = Configurable::Enabled(NvueCollectorConfig {
            rest: Configurable::Enabled(Default::default()),
            gnmi: Configurable::Enabled(crate::config::NvueGnmiConfig {
                gnmi_port: 19_339,
                ..Default::default()
            }),
        });

        config.collectors.nmxt = Configurable::Enabled(Default::default());

        config.collectors.nmxc = Configurable::Enabled(NmxcCollectorConfig {
            grpc_port: 19_370,
            ..Default::default()
        });

        config.sinks.tracing = Configurable::Enabled(Default::default());

        let host = switch_endpoint(SwitchEndpointRole::Host);

        assert_eq!(
            targets_for_config(&host, &config, true),
            vec![
                ReachabilityTarget {
                    service: ReachabilityService::NvueRest,
                    address: (host.addr.ip, 9443).into(),
                },
                ReachabilityTarget {
                    service: ReachabilityService::Gnmi,
                    address: (host.addr.ip, 19_339).into(),
                },
                ReachabilityTarget {
                    service: ReachabilityService::Nmxt,
                    address: (host.addr.ip, NMXT_PORT).into(),
                },
                ReachabilityTarget {
                    service: ReachabilityService::Nmxc,
                    address: (host.addr.ip, 19_370).into(),
                },
            ]
        );

        let switch_bmc = switch_endpoint(SwitchEndpointRole::Bmc);

        assert_eq!(
            targets_for_config(&switch_bmc, &config, true),
            vec![ReachabilityTarget {
                service: ReachabilityService::Redfish,
                address: (switch_bmc.addr.ip, 9443).into(),
            }],
        );
    }

    #[tokio::test]
    async fn reconciliation_bypasses_shared_limiter_and_updates_only_reachability() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("test listener should bind");

        let listener_address = listener
            .local_addr()
            .expect("test listener should have an address");

        let mut config = Config::default();

        config.collectors.reachability = Configurable::Enabled(ReachabilityCollectorConfig {
            interval: std::time::Duration::from_secs(300),
            ..Default::default()
        });

        config.collectors.nmxt = Configurable::Enabled(Default::default());

        let mut ctx = DiscoveryLoopContext::new_with_tls_config(
            Arc::new(crate::limiter::BucketLimiter::new(
                0,
                std::time::Duration::from_secs(1),
                std::time::Duration::ZERO,
            )),
            Arc::new(
                MetricsManager::new("reachability_collector_state")
                    .expect("metrics manager should start"),
            ),
            Arc::new(config),
            None,
            None,
        )
        .expect("discovery context should start");

        let mut endpoint = test_endpoint(mac("00:11:22:33:44:53"));
        endpoint.addr.ip = listener_address.ip();
        endpoint.addr.port = Some(listener_address.port());
        let endpoint = Arc::new(endpoint);
        let key = endpoint.key();
        let mut duplicate = switch_endpoint(SwitchEndpointRole::Host);
        duplicate.addr.mac = endpoint.addr.mac;
        duplicate.addr.ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        let duplicate_ip = duplicate.addr.ip;

        let sink = Arc::new(NotifyingSink::default());

        reconcile_reachability_collectors(
            &mut ctx,
            &[Arc::new(duplicate), endpoint.clone()],
            Some(sink.clone()),
            "test",
        )
        .await
        .expect("collector should start");

        tokio::time::timeout(std::time::Duration::from_secs(1), sink.0.notified())
            .await
            .expect("reachability should not wait for the shared collector limiter");

        ctx.collectors.insert(
            CollectorKind::Sensor,
            Cow::Owned(key.clone()),
            Collector::spawn_task(|_| async {}),
        );

        assert_eq!(ctx.collectors.len(CollectorKind::Reachability), 1);

        let initial = ctx
            .collectors
            .reachability_specs
            .get(key.as_str())
            .expect("initial reachability spec");

        assert_eq!(
            initial.targets.values().cloned().collect::<Vec<_>>(),
            vec![
                ReachabilityTarget {
                    service: ReachabilityService::Redfish,
                    address: listener_address,
                },
                ReachabilityTarget {
                    service: ReachabilityService::Nmxt,
                    address: (duplicate_ip, NMXT_PORT).into(),
                },
            ],
            "merged targets use stable service order and effective collector addresses",
        );

        let mut changed = endpoint.as_ref().clone();
        changed.addr.ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8));
        changed
            .labels
            .insert("location".to_string(), "rack-b".to_string());

        let changed = Arc::new(changed);

        reconcile_reachability_collectors(
            &mut ctx,
            std::slice::from_ref(&changed),
            Some(sink.clone()),
            "test",
        )
        .await
        .expect("changed endpoint should replace reachability");

        let updated = ctx
            .collectors
            .reachability_specs
            .get(key.as_str())
            .expect("updated reachability spec");

        assert_eq!(updated.event_context.addr.ip, changed.addr.ip);

        assert_eq!(
            updated
                .event_context
                .labels
                .get("location")
                .map(String::as_str),
            Some("rack-b")
        );

        assert!(ctx.collectors.contains(CollectorKind::Sensor, &key));

        reconcile_reachability_collectors(&mut ctx, &[], Some(sink), "test")
            .await
            .expect("removed endpoint should stop reachability");

        assert_eq!(ctx.collectors.len(CollectorKind::Reachability), 0);
        assert!(ctx.collectors.reachability_specs.is_empty());
        assert!(ctx.collectors.contains(CollectorKind::Sensor, &key));
    }
}
