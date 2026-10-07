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

//! Shared config-drift instrumentation for the declarative config-file
//! seeding flow (see [`crate::resource_pool::reconcile_pool_defs`] and
//! [`crate::network_segment::reconcile_network_defs`]).
//!
//! Both reconcilers detect the same two conditions against the same
//! one-shot-seed-then-warn contract: a declaration that no longer matches
//! what was seeded, and a snapshot whose declaration was removed from the
//! config file entirely. `resource_kind` tells the two reconcilers apart on
//! one shared counter instead of splitting the metric per resource type.

use carbide_instrument::{DynamicMessage, Event, LabelValue, MetricFamily};

/// Which config-file-seeded resource a drift was detected on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ConfigResourceKind {
    ResourcePool,
    NetworkDefinition,
}

/// How a config-file-seeded resource's stored snapshot has drifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ConfigDriftKind {
    /// The stored snapshot no longer matches the current declaration.
    Changed,
    /// The stored snapshot's declaration was removed from the config file.
    Dropped,
}

/// The metric family `ConfigDefinitionDrifted` records: how many config-file
/// seeded definitions have drifted, by resource kind and drift kind.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_config_drift_total",
    kind = counter,
    component = "nico-api",
    describe = "Number of config-file seeded definitions that have drifted from their declaration, by resource_kind (resource_pool, network_definition) and drift_kind (changed, dropped)."
)]
pub(crate) struct ConfigDrift {
    resource_kind: ConfigResourceKind,
    drift_kind: ConfigDriftKind,
}

/// A resource pool or network definition seeded from the config file has
/// drifted from its declaration. The declaration is never re-applied
/// automatically; an operator must reconcile by hand.
#[derive(Event)]
#[event(
    event_name = "config_definition_drifted",
    metric_family = ConfigDrift,
    log = warn,
    message = dynamic
)]
pub(crate) struct ConfigDefinitionDrifted {
    /// Which kind of config-file-seeded resource drifted.
    #[label]
    pub(crate) resource_kind: ConfigResourceKind,
    /// How the stored snapshot has drifted from the declaration.
    #[label]
    pub(crate) drift_kind: ConfigDriftKind,
    /// The resource pool or network definition's name.
    #[context]
    pub(crate) name: String,
    /// Present for `ConfigDriftKind::Changed`; absent for `Dropped`, which
    /// has no current declaration to diff against.
    #[context]
    pub(crate) stored: Option<String>,
    /// The current declaration. Present for `ConfigDriftKind::Changed`;
    /// absent for `Dropped`, which has no current declaration to diff
    /// against.
    #[context]
    pub(crate) declared: Option<String>,
}

impl DynamicMessage for ConfigDefinitionDrifted {
    fn message(&self) -> &'static str {
        match self.drift_kind {
            ConfigDriftKind::Changed => {
                "Config-seeded definition has changed since it was seeded; not re-applying"
            }
            ConfigDriftKind::Dropped => {
                "Config-seeded definition exists in database but is no longer declared in any \
                 config file"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::*;

    const METRIC: &str = "carbide_config_drift_total";

    #[test]
    fn changed_drift_logs_and_counts_by_resource_and_drift_kind() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(ConfigDefinitionDrifted {
                resource_kind: ConfigResourceKind::ResourcePool,
                drift_kind: ConfigDriftKind::Changed,
                name: "vpc-vni".to_string(),
                stored: Some("stored-repr".to_string()),
                declared: Some("declared-repr".to_string()),
            });
        });

        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(log.metadata_name, "config_definition_drifted");
        assert_eq!(
            log.message,
            "Config-seeded definition has changed since it was seeded; not re-applying"
        );
        assert_eq!(log.field("name"), Some("vpc-vni"));
        assert_eq!(log.field("stored"), Some("stored-repr"));
        assert_eq!(log.field("declared"), Some("declared-repr"));
        assert_eq!(
            metrics.counter_delta(
                METRIC,
                &[
                    ("resource_kind", "resource_pool"),
                    ("drift_kind", "changed")
                ],
            ),
            1.0
        );
    }

    #[test]
    fn dropped_drift_omits_stored_and_declared_context() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(ConfigDefinitionDrifted {
                resource_kind: ConfigResourceKind::NetworkDefinition,
                drift_kind: ConfigDriftKind::Dropped,
                name: "admin-net".to_string(),
                stored: None,
                declared: None,
            });
        });

        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(
            log.message,
            "Config-seeded definition exists in database but is no longer declared in any \
             config file"
        );
        assert_eq!(log.field("name"), Some("admin-net"));
        assert_eq!(log.field("stored"), None);
        assert_eq!(log.field("declared"), None);
        assert_eq!(
            metrics.counter_delta(
                METRIC,
                &[
                    ("resource_kind", "network_definition"),
                    ("drift_kind", "dropped"),
                ],
            ),
            1.0
        );
    }
}
