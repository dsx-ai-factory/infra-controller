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
use std::collections::HashSet;
use std::sync::Arc;

use futures::future::join_all;

use super::context::{CollectorKind, DiscoveryLoopContext};
use crate::collectors::Collector;
use crate::endpoint::BmcEndpoint;

#[derive(Clone, Copy)]
enum CollectorStopReason {
    EndpointRemoved,
    PowerShelfIdChanged,
    SwitchEndpointNoLongerEligible,
    SwitchDomainChanged,
}

impl std::fmt::Display for CollectorStopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EndpointRemoved => "endpoint removed",
            Self::PowerShelfIdChanged => "PowerShelf ID changed",
            Self::SwitchEndpointNoLongerEligible => "switch endpoint is no longer eligible",
            Self::SwitchDomainChanged => "switch NVLink domain changed",
        })
    }
}

/// Restarts switch collectors when discovery reports a different NVLink domain.
///
/// Collectors retain the endpoint metadata captured at startup. Because the
/// endpoint key does not change with the domain UUID, removed-endpoint cleanup
/// cannot refresh that metadata. This function removes affected collectors and
/// waits for their shutdown before discovery respawns them.
pub(super) async fn stop_stale_switch_collectors(
    ctx: &mut DiscoveryLoopContext,
    endpoints: &[Arc<BmcEndpoint>],
) {
    // Keep one domain observation per collector key. The active set also
    // identifies which saved observations remain valid after this pass.
    let mut active_switch_endpoints = HashSet::with_capacity(endpoints.len());
    let mut changed_endpoints = HashSet::new();

    for endpoint in endpoints {
        let Some(switch) = endpoint.switch_data() else {
            continue;
        };

        let key = Cow::Owned(endpoint.key());

        // Collector spawning uses the first endpoint for a key. Apply the same
        // precedence here so a later source cannot create a false domain change
        // for the collector that was spawned from the first endpoint.
        if active_switch_endpoints.contains(&key) {
            continue;
        }

        if ctx
            .collectors
            .observe_switch_domain(&key, switch.nvlink_domain_uuid)
        {
            changed_endpoints.insert(key.clone());
        }

        active_switch_endpoints.insert(key);
    }

    // Forget observations for switches absent from this discovery pass. If a
    // switch returns later, its current domain establishes a fresh baseline.
    ctx.collectors
        .retain_switch_domains(&active_switch_endpoints);

    // Remove every collector kind before awaiting shutdown. Same-pass spawning
    // can then create replacements with the updated endpoint metadata.
    let stale_collectors = CollectorKind::ALL
        .into_iter()
        .flat_map(|kind| {
            take_collectors_for_keys(
                ctx,
                kind,
                &changed_endpoints,
                CollectorStopReason::SwitchDomainChanged,
            )
        })
        .collect::<Vec<_>>();

    // CollectorRemoved unregisters the old Prometheus label set. Wait for that
    // cleanup before replacement collectors register the new domain UUID.
    join_all(stale_collectors.into_iter().map(Collector::stop)).await;
}

/// Restarts PowerShelf collectors when discovery reports a different NICo ID.
///
/// The endpoint key remains the BMC MAC address when an ID becomes available,
/// while collectors retain the metadata captured at startup. Restarting them
/// allows sinks to replace the old series with the updated identity labels.
pub(super) async fn stop_stale_power_shelf_collectors(
    ctx: &mut DiscoveryLoopContext,
    endpoints: &[Arc<BmcEndpoint>],
) {
    let mut active_power_shelf_endpoints = HashSet::with_capacity(endpoints.len());
    let mut changed_endpoints = HashSet::new();

    for endpoint in endpoints {
        let Some(crate::endpoint::EndpointMetadata::PowerShelf(power_shelf)) =
            endpoint.metadata.as_ref()
        else {
            continue;
        };

        let key = Cow::Owned(endpoint.key());

        // Collector spawning uses the first endpoint for a duplicate key.
        if active_power_shelf_endpoints.contains(&key) {
            continue;
        }

        if ctx.collectors.observe_power_shelf_id(&key, power_shelf.id) {
            changed_endpoints.insert(key.clone());
        }

        active_power_shelf_endpoints.insert(key);
    }

    ctx.collectors
        .retain_power_shelf_ids(&active_power_shelf_endpoints);

    let stale_collectors = CollectorKind::ALL
        .into_iter()
        .flat_map(|kind| {
            take_collectors_for_keys(
                ctx,
                kind,
                &changed_endpoints,
                CollectorStopReason::PowerShelfIdChanged,
            )
        })
        .collect::<Vec<_>>();

    // CollectorRemoved unregisters the old Prometheus label set before the
    // discovery loop starts replacements with the updated PowerShelf ID.
    join_all(stale_collectors.into_iter().map(Collector::stop)).await;
}

fn take_collectors_for_keys(
    ctx: &mut DiscoveryLoopContext,
    kind: CollectorKind,
    removed_keys: &HashSet<Cow<'static, str>>,
    stop_reason: CollectorStopReason,
) -> Vec<Collector> {
    let collectors = ctx.collectors.map_mut(kind);
    let mut removed = Vec::new();
    for key in removed_keys {
        if let Some(collector) = collectors.remove(key) {
            tracing::info!(
                endpoint_key = %key,
                collector_kind = ?kind,
                %stop_reason,
                remaining_collector_count = collectors.len(),
                "Stopping collector"
            );
            removed.push(collector);
        }
    }
    removed
}

fn stop_collectors_for_keys(
    ctx: &mut DiscoveryLoopContext,
    kind: CollectorKind,
    removed_keys: &HashSet<Cow<'static, str>>,
    stop_reason: CollectorStopReason,
) {
    for collector in take_collectors_for_keys(ctx, kind, removed_keys, stop_reason) {
        tokio::spawn(async move {
            collector.stop().await;
        });
    }
}

/// Removes collectors for absent endpoints and waits for their shutdown hooks.
///
/// Completing shutdown before the iteration returns prevents a later discovery
/// pass from starting replacements before old `CollectorRemoved` events arrive.
pub(super) async fn stop_removed_bmc_collectors(
    ctx: &mut DiscoveryLoopContext,
    active_endpoints: &HashSet<Cow<'static, str>>,
) {
    let removed_keys = ctx.collectors.removed_keys(active_endpoints);

    let removed_collectors = CollectorKind::ALL
        .into_iter()
        .flat_map(|kind| {
            take_collectors_for_keys(
                ctx,
                kind,
                &removed_keys,
                CollectorStopReason::EndpointRemoved,
            )
        })
        .collect::<Vec<_>>();

    for key in &removed_keys {
        ctx.collectors.remove_inventory(key);
    }

    join_all(removed_collectors.into_iter().map(Collector::stop)).await;

    if !removed_keys.is_empty() {
        tracing::info!(
            removed_endpoint_count = removed_keys.len(),
            remaining_sensor_collector_count = ctx.collectors.len(CollectorKind::Sensor),
            remaining_log_collector_count = ctx.collectors.len(CollectorKind::Logs),
            remaining_firmware_collector_count = ctx.collectors.len(CollectorKind::Firmware),
            remaining_leak_detector_collector_count =
                ctx.collectors.len(CollectorKind::LeakDetector),
            remaining_nmxt_collector_count = ctx.collectors.len(CollectorKind::Nmxt),
            remaining_nmxc_collector_count = ctx.collectors.len(CollectorKind::Nmxc),
            remaining_nvue_rest_collector_count = ctx.collectors.len(CollectorKind::NvueRest),
            "Cleaned up removed endpoints"
        );
    }
}

/// Stops NMX-C streams for endpoints that still exist but are no longer eligible.
///
/// Generic removed-endpoint cleanup only sees keys that disappear. NMX-C can
/// become invalid while the same key remains active, for example when primary
/// switch-host assignment or `nmxc_enabled` changes in discovery metadata.
pub(super) fn stop_ineligible_nmxc_collectors(
    ctx: &mut DiscoveryLoopContext,
    eligible_endpoints: &HashSet<Cow<'static, str>>,
) {
    let ineligible_keys: HashSet<Cow<'static, str>> = ctx
        .collectors
        .map_mut(CollectorKind::Nmxc)
        .keys()
        .filter(|key| !eligible_endpoints.contains(*key))
        .cloned()
        .collect();

    stop_collectors_for_keys(
        ctx,
        CollectorKind::Nmxc,
        &ineligible_keys,
        CollectorStopReason::SwitchEndpointNoLongerEligible,
    );

    if !ineligible_keys.is_empty() {
        tracing::info!(
            ineligible_endpoint_count = ineligible_keys.len(),
            remaining_nmxc_collector_count = ctx.collectors.len(CollectorKind::Nmxc),
            "Cleaned up ineligible NMX-C endpoints"
        );
    }
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;
    use std::sync::Arc;

    use carbide_uuid::power_shelf::PowerShelfId;

    use super::*;
    use crate::collectors::Collector;
    use crate::config::Config;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{EndpointMetadata, PowerShelfData, SwitchData, SwitchEndpointRole};
    use crate::limiter::{NoopLimiter, RateLimiter};
    use crate::metrics::MetricsManager;

    fn context(metrics_name: &str) -> DiscoveryLoopContext {
        let limiter: Arc<dyn RateLimiter> = Arc::new(NoopLimiter);
        let metrics_manager =
            Arc::new(MetricsManager::new(metrics_name).expect("metrics manager should initialize"));

        DiscoveryLoopContext::new(limiter, metrics_manager, Arc::new(Config::default()))
            .expect("context should initialize")
    }

    fn noop_collector() -> Collector {
        Collector::spawn_task(|_| async {})
    }

    #[tokio::test]
    async fn test_stop_ineligible_nmxc_collectors_only_removes_nmxc_entries() {
        let mut ctx = context("test_stop_ineligible_nmxc_collectors");

        ctx.collectors.insert(
            CollectorKind::Nmxc,
            Cow::Borrowed("eligible-switch"),
            noop_collector(),
        );

        ctx.collectors.insert(
            CollectorKind::Nmxc,
            Cow::Borrowed("ineligible-switch"),
            noop_collector(),
        );

        ctx.collectors.insert(
            CollectorKind::Nmxt,
            Cow::Borrowed("ineligible-switch"),
            noop_collector(),
        );

        let eligible_endpoints = HashSet::from([Cow::Borrowed("eligible-switch")]);

        stop_ineligible_nmxc_collectors(&mut ctx, &eligible_endpoints);

        assert!(
            ctx.collectors
                .contains(CollectorKind::Nmxc, "eligible-switch")
        );

        assert!(
            !ctx.collectors
                .contains(CollectorKind::Nmxc, "ineligible-switch")
        );

        assert!(
            ctx.collectors
                .contains(CollectorKind::Nmxt, "ineligible-switch")
        );
    }

    #[tokio::test]
    async fn switch_domain_change_restarts_collectors_for_same_endpoint_key() {
        let mut ctx = context("switch_domain_change_restarts_collectors");
        let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        endpoint.metadata = Some(EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: "switch-1".to_string(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role: SwitchEndpointRole::Host,
            is_primary: true,
            nmxc_enabled: true,
            nmxt_enabled: true,
        }));
        let key = endpoint.key();
        let mut endpoint = Arc::new(endpoint);

        ctx.collectors.insert(
            CollectorKind::NvueRest,
            Cow::Owned(key.clone()),
            noop_collector(),
        );
        stop_stale_switch_collectors(&mut ctx, std::slice::from_ref(&endpoint)).await;
        assert!(ctx.collectors.contains(CollectorKind::NvueRest, &key));

        let expected_domain = carbide_uuid::nvlink::NvLinkDomainId::new();
        let Some(EndpointMetadata::Switch(switch)) = Arc::make_mut(&mut endpoint).metadata.as_mut()
        else {
            panic!("test endpoint should contain switch metadata");
        };
        switch.nvlink_domain_uuid = Some(expected_domain);

        stop_stale_switch_collectors(&mut ctx, std::slice::from_ref(&endpoint)).await;

        assert!(!ctx.collectors.contains(CollectorKind::NvueRest, &key));

        let updated_context = crate::sink::EventContext::from_endpoint(&endpoint, "nvue_rest");
        assert_eq!(updated_context.nvlink_domain_uuid(), Some(expected_domain));

        ctx.collectors.insert(
            CollectorKind::NvueRest,
            Cow::Owned(key.clone()),
            noop_collector(),
        );
        stop_stale_switch_collectors(&mut ctx, &[endpoint]).await;
        assert!(ctx.collectors.contains(CollectorKind::NvueRest, &key));
    }

    #[tokio::test]
    async fn power_shelf_id_change_restarts_collectors_for_same_endpoint_key() {
        let mut ctx = context("power_shelf_id_change_restarts_collectors");
        let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
        endpoint.metadata = Some(EndpointMetadata::PowerShelf(PowerShelfData {
            id: None,
            serial: None,
        }));
        let key = endpoint.key();
        let mut endpoint = Arc::new(endpoint);

        ctx.collectors.insert(
            CollectorKind::Sensor,
            Cow::Owned(key.clone()),
            noop_collector(),
        );
        stop_stale_power_shelf_collectors(&mut ctx, std::slice::from_ref(&endpoint)).await;
        assert!(ctx.collectors.contains(CollectorKind::Sensor, &key));

        let expected_id =
            PowerShelfId::from_str("ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg")
                .expect("valid PowerShelf ID");
        let Some(EndpointMetadata::PowerShelf(power_shelf)) =
            Arc::make_mut(&mut endpoint).metadata.as_mut()
        else {
            panic!("test endpoint should contain PowerShelf metadata");
        };
        power_shelf.id = Some(expected_id);

        stop_stale_power_shelf_collectors(&mut ctx, std::slice::from_ref(&endpoint)).await;

        assert!(!ctx.collectors.contains(CollectorKind::Sensor, &key));

        let updated_context =
            crate::sink::EventContext::from_endpoint(&endpoint, "sensor_collector");
        assert_eq!(updated_context.power_shelf_id(), Some(expected_id));

        ctx.collectors.insert(
            CollectorKind::Sensor,
            Cow::Owned(key.clone()),
            noop_collector(),
        );
        stop_stale_power_shelf_collectors(&mut ctx, &[endpoint]).await;
        assert!(ctx.collectors.contains(CollectorKind::Sensor, &key));
    }

    #[tokio::test]
    async fn duplicate_switch_domains_use_first_source_without_restarts() {
        let mut ctx = context("duplicate_switch_domains_use_first_source");
        let mut first = test_endpoint(mac("00:11:22:33:44:55"));

        first.metadata = Some(EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: "switch-1".to_string(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role: SwitchEndpointRole::Host,
            is_primary: true,
            nmxc_enabled: true,
            nmxt_enabled: true,
        }));

        let key = first.key();
        let first = Arc::new(first);
        let mut duplicate = first.as_ref().clone();

        let Some(EndpointMetadata::Switch(switch)) = duplicate.metadata.as_mut() else {
            panic!("test endpoint should contain switch metadata");
        };

        switch.nvlink_domain_uuid = Some(carbide_uuid::nvlink::NvLinkDomainId::new());

        let endpoints = [first, Arc::new(duplicate)];
        stop_stale_switch_collectors(&mut ctx, &endpoints).await;

        ctx.collectors.insert(
            CollectorKind::NvueRest,
            Cow::Owned(key.clone()),
            noop_collector(),
        );

        stop_stale_switch_collectors(&mut ctx, &endpoints).await;

        assert!(ctx.collectors.contains(CollectorKind::NvueRest, &key));
    }
}
