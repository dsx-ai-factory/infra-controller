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

use std::fmt;

use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::RackId;
use db::{machine as db_machine, power_shelf as db_power_shelf, switch as db_switch};
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack_type::RackCapabilitiesSet;
use state_controller::state_handler::StateHandlerError;

pub(crate) const COMPONENT_INVENTORY_HEALTH_SOURCE: &str = "rack-component-inventory";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RackComponentCounts {
    pub(crate) compute: u32,
    pub(crate) switch: u32,
    pub(crate) power_shelf: u32,
}

impl RackComponentCounts {
    pub(crate) async fn load(
        db_pool: &sqlx::PgPool,
        rack_id: &RackId,
    ) -> Result<Self, StateHandlerError> {
        let mut connection = db_pool.acquire().await?;

        let compute = db_machine::find_machine_ids::<MachineId>(
            connection.as_mut(),
            MachineSearchConfig {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await?
        .len() as u32;
        let switch = db_switch::find_ids(
            connection.as_mut(),
            model::switch::SwitchSearchFilter {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await?
        .len() as u32;
        let power_shelf = db_power_shelf::find_ids(
            connection.as_mut(),
            model::power_shelf::PowerShelfSearchFilter {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await?
        .len() as u32;

        Ok(Self {
            compute,
            switch,
            power_shelf,
        })
    }

    pub(crate) fn missing_component_report(
        self,
        capabilities: &RackCapabilitiesSet,
    ) -> Option<HealthReport> {
        let alerts = RackComponent::ALL
            .into_iter()
            .filter_map(|component| {
                let expected_count = component.expected_count(capabilities);
                let actual_count = component.actual_count(self);
                let missing_count = expected_count.saturating_sub(actual_count);

                (missing_count > 0).then(|| HealthProbeAlert {
                    id: health_report::HealthProbeId::missing_rack_component(),
                    target: Some(component.to_string()),
                    in_alert_since: None,
                    message: format!(
                        "Rack component count is below the rack profile: expected {expected_count}, found {actual_count}, missing {missing_count}",
                    ),
                    tenant_message: None,
                    classifications: vec![HealthAlertClassification::hardware()],
                })
            })
            .collect::<Vec<_>>();

        if alerts.is_empty() {
            None
        } else {
            Some(HealthReport {
                source: COMPONENT_INVENTORY_HEALTH_SOURCE.to_string(),
                triggered_by: None,
                observed_at: Some(chrono::Utc::now()),
                successes: Vec::new(),
                alerts,
            })
        }
    }
}

#[derive(Clone, Copy)]
enum RackComponent {
    Compute,
    Switch,
    PowerShelf,
}

impl RackComponent {
    const ALL: [Self; 3] = [Self::Compute, Self::Switch, Self::PowerShelf];

    fn actual_count(self, counts: RackComponentCounts) -> u32 {
        match self {
            Self::Compute => counts.compute,
            Self::Switch => counts.switch,
            Self::PowerShelf => counts.power_shelf,
        }
    }

    fn expected_count(self, capabilities: &RackCapabilitiesSet) -> u32 {
        match self {
            Self::Compute => capabilities.compute.count,
            Self::Switch => capabilities.switch.count,
            Self::PowerShelf => capabilities.power_shelf.count,
        }
    }
}

impl fmt::Display for RackComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compute => write!(f, "compute"),
            Self::Switch => write!(f, "switch"),
            Self::PowerShelf => write!(f, "power_shelf"),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::rack_type::{RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch};

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct AlertSummary {
        target: String,
        message: String,
    }

    fn capabilities(compute: u32, switch: u32, power_shelf: u32) -> RackCapabilitiesSet {
        RackCapabilitiesSet {
            compute: RackCapabilityCompute {
                count: compute,
                ..Default::default()
            },
            switch: RackCapabilitySwitch {
                count: switch,
                ..Default::default()
            },
            power_shelf: RackCapabilityPowerShelf {
                count: power_shelf,
                ..Default::default()
            },
        }
    }

    #[test]
    fn missing_component_report_aggregates_by_component_type() {
        let summarize = |(counts, capabilities): (RackComponentCounts, RackCapabilitiesSet)| {
            counts
                .missing_component_report(&capabilities)
                .map(|report| {
                    report
                        .alerts
                        .into_iter()
                        .map(|alert| {
                            assert_eq!(
                                alert.classifications,
                                vec![HealthAlertClassification::hardware()]
                            );
                            AlertSummary {
                                target: alert.target.expect("component alerts have a target"),
                                message: alert.message,
                            }
                        })
                        .collect::<Vec<_>>()
                })
        };

        check_values(
            [
                Check {
                    scenario: "all expected components present",
                    input: (
                        RackComponentCounts {
                            switch: 9,
                            power_shelf: 2,
                            ..Default::default()
                        },
                        capabilities(0, 9, 2),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "missing compute alerts",
                    input: (
                        RackComponentCounts {
                            compute: 17,
                            switch: 9,
                            power_shelf: 2,
                        },
                        capabilities(18, 9, 2),
                    ),
                    expect: Some(vec![AlertSummary {
                        target: "compute".to_string(),
                        message: "Rack component count is below the rack profile: expected 18, found 17, missing 1".to_string(),
                    }]),
                },
                Check {
                    scenario: "extra components do not alert",
                    input: (
                        RackComponentCounts {
                            compute: 19,
                            switch: 10,
                            power_shelf: 3,
                        },
                        capabilities(18, 9, 2),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "one missing component type",
                    input: (
                        RackComponentCounts {
                            switch: 8,
                            power_shelf: 2,
                            ..Default::default()
                        },
                        capabilities(0, 9, 2),
                    ),
                    expect: Some(vec![AlertSummary {
                        target: "switch".to_string(),
                        message: "Rack component count is below the rack profile: expected 9, found 8, missing 1".to_string(),
                    }]),
                },
                Check {
                    scenario: "multiple missing component types",
                    input: (
                        RackComponentCounts {
                            compute: 17,
                            switch: 7,
                            power_shelf: 0,
                        },
                        capabilities(18, 9, 2),
                    ),
                    expect: Some(vec![
                        AlertSummary {
                            target: "compute".to_string(),
                            message: "Rack component count is below the rack profile: expected 18, found 17, missing 1".to_string(),
                        },
                        AlertSummary {
                            target: "switch".to_string(),
                            message: "Rack component count is below the rack profile: expected 9, found 7, missing 2".to_string(),
                        },
                        AlertSummary {
                            target: "power_shelf".to_string(),
                            message: "Rack component count is below the rack profile: expected 2, found 0, missing 2".to_string(),
                        },
                    ]),
                },
            ],
            summarize,
        );
    }
}
