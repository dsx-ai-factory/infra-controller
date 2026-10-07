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

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use prometheus::{Gauge, GaugeVec, Opts, Registry};

use crate::HealthError;
use crate::endpoint::{ComponentInventory, EndpointMetadata, RackInventory};

const COMPONENT_LABELS: [&str; 11] = [
    "rack_id",
    "session_id",
    "subsystem",
    "component_type",
    "component_uid",
    "bmc_mac",
    "nvl_domain",
    "nmxc_enabled",
    "nmxc_primary",
    "slot_number",
    "tray_index",
];
const RACK_LABELS: [&str; 2] = ["rack_id", "session_id"];
const RACK_DOMAIN_LABELS: [&str; 3] = ["rack_id", "session_id", "nvl_domain"];

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "hardware_inventory_refresh_failed",
    metric_name = "carbide_hardware_health_inventory_refresh_failures_total",
    component = "nico-hardware-health",
    log = warn,
    metric = counter,
    message = "authoritative hardware inventory refresh failed; retaining previous snapshot",
    describe = "Number of authoritative hardware inventory refreshes that failed."
)]
pub(crate) struct InventoryRefreshFailed {
    #[context]
    pub(crate) error: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RackSeries {
    rack_id: String,
    session_id: String,
    created: Option<(i64, i32)>,
}

impl RackSeries {
    fn from_inventory(rack: &RackInventory) -> Self {
        let rack_id = rack.rack_id.to_string();
        let created = match (rack.created_seconds, rack.created_nanos) {
            (Some(seconds), Some(nanos)) if (0..1_000_000_000).contains(&nanos) => {
                Some((seconds, nanos))
            }
            _ => {
                tracing::warn!(
                    rack_id = %rack.rack_id,
                    created_seconds = ?rack.created_seconds,
                    created_nanos = ?rack.created_nanos,
                    "Using degraded rack inventory session identity because the creation timestamp is unavailable or invalid"
                );
                None
            }
        };
        let session_id = created.map_or_else(
            || format!("{rack_id}:unknown"),
            |(seconds, nanos)| format!("{rack_id}:{seconds}.{nanos:09}"),
        );

        Self {
            rack_id,
            session_id,
            created,
        }
    }

    fn label_values(&self) -> [&str; 2] {
        [&self.rack_id, &self.session_id]
    }

    fn start_time_seconds(&self) -> Option<f64> {
        self.created
            .map(|(seconds, nanos)| seconds as f64 + f64::from(nanos) / 1_000_000_000.0)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RackDomainSeries {
    rack_id: String,
    session_id: String,
    nvl_domain: String,
}

impl RackDomainSeries {
    fn label_values(&self) -> [&str; 3] {
        [&self.rack_id, &self.session_id, &self.nvl_domain]
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ComponentSeries {
    rack_id: String,
    session_id: String,
    subsystem: &'static str,
    component_type: &'static str,
    component_uid: String,
    bmc_mac: String,
    nvl_domain: String,
    nmxc_enabled: bool,
    nmxc_primary: bool,
    slot_number: String,
    tray_index: String,
}

impl ComponentSeries {
    fn from_inventory(component: &ComponentInventory, rack: &RackSeries) -> Option<Self> {
        let metadata = &component.metadata;
        let (
            subsystem,
            component_uid,
            bmc_mac,
            nvl_domain,
            nmxc_enabled,
            nmxc_primary,
            slot_number,
            tray_index,
        ) = match metadata {
            EndpointMetadata::Machine(machine) => (
                "compute",
                machine.machine_id.as_ref()?.to_string(),
                component
                    .bmc_mac
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                machine
                    .nvlink_domain_uuid
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                false,
                false,
                machine
                    .slot_number
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                machine
                    .tray_index
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            EndpointMetadata::Switch(switch) => (
                "switch",
                switch
                    .id
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| switch.serial.clone()),
                component
                    .bmc_mac
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                switch
                    .nvlink_domain_uuid
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                switch.nmxc_enabled,
                switch.is_primary,
                switch
                    .slot_number
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                switch
                    .tray_index
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            EndpointMetadata::PowerShelf(power_shelf) => (
                "power",
                power_shelf
                    .id
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| power_shelf.serial.clone())?,
                component
                    .bmc_mac
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                power_shelf
                    .nvlink_domain_uuid
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                false,
                false,
                String::new(),
                String::new(),
            ),
        };

        Some(Self {
            rack_id: rack.rack_id.clone(),
            session_id: rack.session_id.clone(),
            subsystem,
            component_type: metadata.component_type(),
            component_uid,
            bmc_mac,
            nvl_domain,
            nmxc_enabled,
            nmxc_primary,
            slot_number,
            tray_index,
        })
    }

    fn identity(&self) -> (String, String, &'static str, String) {
        (
            self.rack_id.clone(),
            self.session_id.clone(),
            self.component_type,
            self.component_uid.clone(),
        )
    }

    /// Combines duplicate authoritative observations of the same component.
    fn merge(&mut self, other: &Self) {
        debug_assert_eq!(self.identity(), other.identity());

        if self.bmc_mac.is_empty() {
            self.bmc_mac.clone_from(&other.bmc_mac);
        }
        if self.nvl_domain.is_empty() {
            self.nvl_domain.clone_from(&other.nvl_domain);
        }
        if self.slot_number.is_empty() {
            self.slot_number.clone_from(&other.slot_number);
        }
        if self.tray_index.is_empty() {
            self.tray_index.clone_from(&other.tray_index);
        }
        self.nmxc_enabled |= other.nmxc_enabled;
        self.nmxc_primary |= other.nmxc_primary;
    }

    fn label_values(&self) -> [&str; 11] {
        [
            &self.rack_id,
            &self.session_id,
            self.subsystem,
            self.component_type,
            &self.component_uid,
            &self.bmc_mac,
            &self.nvl_domain,
            if self.nmxc_enabled { "true" } else { "false" },
            if self.nmxc_primary { "true" } else { "false" },
            &self.slot_number,
            &self.tray_index,
        ]
    }
}

/// Reconciles the latest successful NICo inventory snapshot into bounded
/// Prometheus info series.
pub(crate) struct InventoryMetrics {
    component_info: GaugeVec,
    rack_nvlink_domain_info: GaugeVec,
    rack_session_start_time_seconds: GaugeVec,
    last_success_time_seconds: Gauge,
    current_components: BTreeSet<ComponentSeries>,
    current_rack_domains: BTreeSet<RackDomainSeries>,
    current_racks: BTreeSet<RackSeries>,
}

impl InventoryMetrics {
    pub(crate) fn new(registry: &Registry, metrics_prefix: &str) -> Result<Self, HealthError> {
        let component_info = GaugeVec::new(
            Opts::new(
                format!("{metrics_prefix}_component_inventory_info"),
                "Authoritative NICo component inventory for the current rack-ingestion session",
            ),
            &COMPONENT_LABELS,
        )?;
        registry.register(Box::new(component_info.clone()))?;

        let rack_nvlink_domain_info = GaugeVec::new(
            Opts::new(
                format!("{metrics_prefix}_rack_nvlink_domain_info"),
                "Authoritative NICo rack-to-NVLink-domain assignments for current rack-ingestion sessions",
            ),
            &RACK_DOMAIN_LABELS,
        )?;
        registry.register(Box::new(rack_nvlink_domain_info.clone()))?;

        let rack_session_start_time_seconds = GaugeVec::new(
            Opts::new(
                format!("{metrics_prefix}_rack_session_start_time_seconds"),
                "NICo rack creation time in Unix seconds, labeled by its ingestion session",
            ),
            &RACK_LABELS,
        )?;
        registry.register(Box::new(rack_session_start_time_seconds.clone()))?;

        let last_success_time_seconds = Gauge::new(
            format!("{metrics_prefix}_inventory_last_success_time_seconds"),
            "Unix timestamp of the last successful NICo inventory reconciliation",
        )?;
        registry.register(Box::new(last_success_time_seconds.clone()))?;

        Ok(Self {
            component_info,
            rack_nvlink_domain_info,
            rack_session_start_time_seconds,
            last_success_time_seconds,
            current_components: BTreeSet::new(),
            current_rack_domains: BTreeSet::new(),
            current_racks: BTreeSet::new(),
        })
    }

    pub(crate) fn reconcile(&mut self, racks: &[RackInventory], components: &[ComponentInventory]) {
        self.reconcile_at(racks, components, unix_now_seconds());
    }

    fn reconcile_at(
        &mut self,
        racks: &[RackInventory],
        components: &[ComponentInventory],
        observed_at_seconds: f64,
    ) {
        let desired_racks = racks
            .iter()
            .map(RackSeries::from_inventory)
            .collect::<BTreeSet<_>>();
        let racks_by_id = desired_racks
            .iter()
            .map(|rack| (rack.rack_id.as_str(), rack))
            .collect::<BTreeMap<_, _>>();

        // Key by component identity so duplicate source observations produce
        // one inventory series.
        let mut desired_by_identity: BTreeMap<_, ComponentSeries> = BTreeMap::new();
        for inventory_component in components {
            let rack_id = inventory_component.rack_id.to_string();
            let Some(rack) = racks_by_id.get(rack_id.as_str()) else {
                tracing::warn!(
                    %rack_id,
                    component = ?inventory_component.metadata,
                    "Skipped authoritative component whose rack is absent from the inventory snapshot"
                );
                continue;
            };
            let Some(component) = ComponentSeries::from_inventory(inventory_component, rack) else {
                continue;
            };
            desired_by_identity
                .entry(component.identity())
                .and_modify(|existing| existing.merge(&component))
                .or_insert(component);
        }
        let desired_components = desired_by_identity.into_values().collect::<BTreeSet<_>>();

        // NICo persists a rack-scoped NVLink domain on every active switch in
        // that rack. Collapse matching switch observations into one explicit
        // rack-to-domain relation. Conflicting non-empty domains are a NICo
        // inventory inconsistency, so do not publish an arbitrary assignment.
        let mut domains_by_rack: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for component in &desired_components {
            if component.subsystem == "switch" && !component.nvl_domain.is_empty() {
                domains_by_rack
                    .entry((component.rack_id.clone(), component.session_id.clone()))
                    .or_default()
                    .insert(component.nvl_domain.clone());
            }
        }
        let desired_rack_domains = domains_by_rack
            .into_iter()
            .filter_map(|((rack_id, session_id), domains)| {
                if domains.len() != 1 {
                    tracing::warn!(
                        %rack_id,
                        ?domains,
                        "NICo inventory reports conflicting NVLink domains for one rack"
                    );
                    return None;
                }

                Some(RackDomainSeries {
                    rack_id,
                    session_id,
                    nvl_domain: domains.into_iter().next()?,
                })
            })
            .collect::<BTreeSet<_>>();

        for stale in self.current_components.difference(&desired_components) {
            if let Err(error) = self
                .component_info
                .remove_label_values(&stale.label_values())
            {
                tracing::warn!(?error, "Could not remove stale component inventory metric");
            }
        }
        for component in &desired_components {
            self.component_info
                .with_label_values(&component.label_values())
                .set(1.0);
        }

        for stale in self.current_rack_domains.difference(&desired_rack_domains) {
            if let Err(error) = self
                .rack_nvlink_domain_info
                .remove_label_values(&stale.label_values())
            {
                tracing::warn!(
                    ?error,
                    "Could not remove stale rack-domain inventory metric"
                );
            }
        }
        for rack_domain in &desired_rack_domains {
            self.rack_nvlink_domain_info
                .with_label_values(&rack_domain.label_values())
                .set(1.0);
        }

        for stale in self.current_racks.difference(&desired_racks) {
            if stale.created.is_some()
                && let Err(error) = self
                    .rack_session_start_time_seconds
                    .remove_label_values(&stale.label_values())
            {
                tracing::warn!(?error, "Could not remove stale rack-session metric");
            }
        }
        for rack in &desired_racks {
            if let Some(start_time_seconds) = rack.start_time_seconds() {
                self.rack_session_start_time_seconds
                    .with_label_values(&rack.label_values())
                    .set(start_time_seconds);
            }
        }

        self.current_components = desired_components;
        self.current_rack_domains = desired_rack_domains;
        self.current_racks = desired_racks;
        self.last_success_time_seconds.set(observed_at_seconds);
    }
}

fn unix_now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
    use mac_address::MacAddress;
    use prometheus::{Encoder, TextEncoder};

    use super::*;
    use crate::endpoint::{EndpointMetadata, PowerShelfData, SwitchData, SwitchEndpointRole};

    fn test_switch_id(seed: u8) -> SwitchId {
        SwitchId::new(SwitchIdSource::Tpm, [seed; 32], SwitchType::NvLink)
    }

    fn test_power_shelf_id() -> PowerShelfId {
        PowerShelfId::new(
            PowerShelfIdSource::ProductBoardChassisSerial,
            [8; 32],
            PowerShelfType::Rack,
        )
    }

    fn switch_component(role: SwitchEndpointRole, mac: &str) -> ComponentInventory {
        switch_component_in_rack(role, mac, "D09", 7, "11111111-1111-1111-1111-111111111111")
    }

    fn switch_component_in_rack(
        role: SwitchEndpointRole,
        mac: &str,
        rack_id: &str,
        switch_seed: u8,
        nvl_domain: &str,
    ) -> ComponentInventory {
        ComponentInventory {
            rack_id: RackId::new(rack_id),
            bmc_mac: Some(MacAddress::from_str(mac).unwrap()),
            metadata: EndpointMetadata::Switch(SwitchData {
                id: Some(test_switch_id(switch_seed)),
                serial: format!("switch-serial-{switch_seed}"),
                slot_number: Some(9),
                tray_index: Some(3),
                nvlink_domain_uuid: Some(NvLinkDomainId::from_str(nvl_domain).unwrap()),
                endpoint_role: role,
                is_primary: role == SwitchEndpointRole::Host,
                nmxc_enabled: role == SwitchEndpointRole::Host,
                nmxt_enabled: false,
            }),
        }
    }

    fn rack() -> RackInventory {
        rack_with_id("D09", 1_725_000_000)
    }

    fn rack_with_id(rack_id: &str, created_seconds: i64) -> RackInventory {
        RackInventory {
            rack_id: RackId::new(rack_id),
            created_seconds: Some(created_seconds),
            created_nanos: Some(123_000_000),
        }
    }

    fn exposition(registry: &Registry) -> String {
        let mut bytes = Vec::new();
        TextEncoder::new()
            .encode(&registry.gather(), &mut bytes)
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn rack_timestamp_quality_controls_only_session_start_metric() {
        struct Case {
            scenario: &'static str,
            created_seconds: Option<i64>,
            created_nanos: Option<i32>,
            expected_session_id: &'static str,
            expects_session_start: bool,
        }

        for case in [
            Case {
                scenario: "valid timestamp",
                created_seconds: Some(1_725_000_000),
                created_nanos: Some(123_000_000),
                expected_session_id: "D09:1725000000.123000000",
                expects_session_start: true,
            },
            Case {
                scenario: "missing timestamp",
                created_seconds: None,
                created_nanos: None,
                expected_session_id: "D09:unknown",
                expects_session_start: false,
            },
            Case {
                scenario: "invalid nanoseconds",
                created_seconds: Some(1_725_000_000),
                created_nanos: Some(1_000_000_000),
                expected_session_id: "D09:unknown",
                expects_session_start: false,
            },
        ] {
            let registry = Registry::new();
            let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
            let rack = RackInventory {
                rack_id: RackId::new("D09"),
                created_seconds: case.created_seconds,
                created_nanos: case.created_nanos,
            };
            metrics.reconcile_at(
                &[rack],
                &[switch_component(
                    SwitchEndpointRole::Bmc,
                    "02:00:00:00:00:01",
                )],
                1_800_000_000.0,
            );

            let output = exposition(&registry);
            let component_line = output
                .lines()
                .find(|line| line.starts_with("carbide_hardware_health_component_inventory_info{"))
                .unwrap_or_else(|| panic!("{}: component inventory missing", case.scenario));
            assert!(
                component_line.contains(&format!("session_id=\"{}\"", case.expected_session_id)),
                "{}: unexpected component session: {component_line}",
                case.scenario
            );

            let has_session_start = output.lines().any(|line| {
                line.starts_with("carbide_hardware_health_rack_session_start_time_seconds{")
            });
            assert_eq!(
                has_session_start, case.expects_session_start,
                "{}: unexpected rack session start metric",
                case.scenario
            );
        }
    }

    #[test]
    fn power_shelf_inventory_preserves_nvlink_domain() {
        let domain = NvLinkDomainId::from_str("77777777-7777-7777-7777-777777777777").unwrap();
        let component = ComponentInventory {
            rack_id: RackId::new("D09"),
            bmc_mac: None,
            metadata: EndpointMetadata::PowerShelf(PowerShelfData {
                id: Some(test_power_shelf_id()),
                serial: None,
                nvlink_domain_uuid: Some(domain),
            }),
        };
        let rack = RackSeries::from_inventory(&rack());

        let series = ComponentSeries::from_inventory(&component, &rack).unwrap();

        assert_eq!(series.nvl_domain, domain.to_string());
    }

    #[test]
    fn reconciles_duplicate_component_observations() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let mut host_observation = switch_component(SwitchEndpointRole::Host, "02:00:00:00:00:02");
        host_observation.bmc_mac = None;
        let components = vec![
            host_observation,
            switch_component(SwitchEndpointRole::Bmc, "02:00:00:00:00:01"),
        ];

        metrics.reconcile_at(&[rack()], &components, 1_800_000_000.0);

        let output = exposition(&registry);
        for (name, help) in [
            (
                "carbide_hardware_health_component_inventory_info",
                "Authoritative NICo component inventory for the current rack-ingestion session",
            ),
            (
                "carbide_hardware_health_inventory_last_success_time_seconds",
                "Unix timestamp of the last successful NICo inventory reconciliation",
            ),
            (
                "carbide_hardware_health_rack_nvlink_domain_info",
                "Authoritative NICo rack-to-NVLink-domain assignments for current rack-ingestion sessions",
            ),
            (
                "carbide_hardware_health_rack_session_start_time_seconds",
                "NICo rack creation time in Unix seconds, labeled by its ingestion session",
            ),
        ] {
            assert!(output.contains(&format!("# HELP {name} {help}")));
            assert!(output.contains(&format!("# TYPE {name} gauge")));
        }
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("carbide_hardware_health_component_inventory_info{"))
                .count(),
            1
        );
        assert!(output.contains("component_type=\"nvlink_switch\""));
        assert!(output.contains("subsystem=\"switch\""));
        assert!(output.contains("bmc_mac=\"02:00:00:00:00:01\""));
        assert!(!output.contains("bmc_mac=\"02:00:00:00:00:02\""));
        assert!(output.contains("nmxc_enabled=\"true\""));
        assert!(output.contains("nmxc_primary=\"true\""));
        assert!(output.contains(
            "carbide_hardware_health_rack_nvlink_domain_info{nvl_domain=\"11111111-1111-1111-1111-111111111111\",rack_id=\"D09\",session_id=\"D09:1725000000.123000000\"} 1"
        ));
        assert!(output.contains("slot_number=\"9\""));
        assert!(output.contains("tray_index=\"3\""));
        assert!(output.contains("session_id=\"D09:1725000000.123000000\""));
        assert!(output.contains(
            "carbide_hardware_health_rack_session_start_time_seconds{rack_id=\"D09\",session_id=\"D09:1725000000.123000000\"} 1725000000.123"
        ));
        assert!(
            output
                .contains("carbide_hardware_health_inventory_last_success_time_seconds 1800000000")
        );
    }

    #[test]
    fn publishes_one_relation_per_rack_for_a_shared_nvlink_domain() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let domain = "22222222-2222-2222-2222-222222222222";
        let racks = [
            rack_with_id("D09", 1_725_000_000),
            rack_with_id("D10", 1_725_000_100),
        ];
        let components = [
            switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:09",
                "D09",
                9,
                domain,
            ),
            switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:10",
                "D10",
                10,
                domain,
            ),
        ];

        metrics.reconcile_at(&racks, &components, 1_800_000_000.0);

        let output = exposition(&registry);
        let rack_domains = output
            .lines()
            .filter(|line| line.starts_with("carbide_hardware_health_rack_nvlink_domain_info{"))
            .collect::<Vec<_>>();
        assert_eq!(rack_domains.len(), 2);
        assert!(rack_domains.iter().all(|line| line.contains(domain)));
        assert!(
            rack_domains
                .iter()
                .any(|line| line.contains("rack_id=\"D09\""))
        );
        assert!(
            rack_domains
                .iter()
                .any(|line| line.contains("rack_id=\"D10\""))
        );
    }

    #[test]
    fn replaces_stale_rack_domain_assignment() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let first_domain = "33333333-3333-3333-3333-333333333333";
        let replacement_domain = "44444444-4444-4444-4444-444444444444";

        metrics.reconcile_at(
            &[rack()],
            &[switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:01",
                "D09",
                7,
                first_domain,
            )],
            1_800_000_000.0,
        );
        metrics.reconcile_at(
            &[rack()],
            &[switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:01",
                "D09",
                7,
                replacement_domain,
            )],
            1_800_000_030.0,
        );

        let output = exposition(&registry);
        assert!(!output.contains(first_domain));
        assert!(output.contains(replacement_domain));
    }

    #[test]
    fn suppresses_conflicting_rack_domain_assignments() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let components = [
            switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:01",
                "D09",
                1,
                "55555555-5555-5555-5555-555555555555",
            ),
            switch_component_in_rack(
                SwitchEndpointRole::Bmc,
                "02:00:00:00:00:02",
                "D09",
                2,
                "66666666-6666-6666-6666-666666666666",
            ),
        ];

        metrics.reconcile_at(&[rack()], &components, 1_800_000_000.0);

        let output = exposition(&registry);
        assert!(!output.contains("carbide_hardware_health_rack_nvlink_domain_info{"));
    }

    #[test]
    fn successful_reconciliation_removes_deleted_components() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let component = switch_component(SwitchEndpointRole::Bmc, "02:00:00:00:00:01");

        metrics.reconcile_at(&[rack()], &[component], 1_800_000_000.0);
        metrics.reconcile_at(&[rack()], &[], 1_800_000_030.0);

        let output = exposition(&registry);
        assert!(!output.contains("carbide_hardware_health_component_inventory_info{"));
        assert!(
            output
                .contains("carbide_hardware_health_inventory_last_success_time_seconds 1800000030")
        );
    }

    #[test]
    fn publishes_component_without_bmc_identity() {
        let registry = Registry::new();
        let mut metrics = InventoryMetrics::new(&registry, "carbide_hardware_health").unwrap();
        let mut component = switch_component(SwitchEndpointRole::Bmc, "02:00:00:00:00:01");
        component.bmc_mac = None;

        metrics.reconcile_at(&[rack()], &[component], 1_800_000_000.0);

        let output = exposition(&registry);
        let component_line = output
            .lines()
            .find(|line| line.starts_with("carbide_hardware_health_component_inventory_info{"))
            .expect("component inventory series");
        assert!(component_line.contains("bmc_mac=\"\""));
    }

    #[test]
    fn refresh_failure_emits_correlated_metric_and_warning() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(InventoryRefreshFailed {
                error: "simulated inventory failure".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(
            logs[0].message,
            "authoritative hardware inventory refresh failed; retaining previous snapshot"
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_hardware_health_inventory_refresh_failures_total",
                &[],
            ),
            1.0
        );
    }
}
