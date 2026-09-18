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

use model::rack::{Rack, RackSearchFilter, derive_rack_aggregate_health};

use crate as rpc;
use crate::Timestamp;
use crate::forge::LifecycleStatus;

impl From<Rack> for rpc::forge::Rack {
    fn from(value: Rack) -> Self {
        let health = derive_rack_aggregate_health(&value.health_reports);
        let health_sources = value
            .health_reports
            .iter()
            .map(|(hr, m)| rpc::forge::HealthSourceOrigin {
                mode: m as i32,
                source: hr.source.clone(),
            })
            .collect();

        let lifecycle = LifecycleStatus {
            state: serde_json::to_string(&value.controller_state.value).unwrap_or_default(),
            version: value.controller_state.version.version_string(),
            state_reason: value.controller_state_outcome.map(Into::into),
            sla: Some(rpc::forge::StateSla {
                sla: None, // TODO: Calculate SLA properly
                time_in_state_above_sla: false,
            }),
        };

        rpc::forge::Rack {
            id: Some(value.id),
            rack_state: value.controller_state.value.to_string(),
            created: Some(Timestamp::from(value.created)),
            updated: Some(Timestamp::from(value.updated)),
            deleted: value.deleted.map(Timestamp::from),
            metadata: Some(value.metadata.into()),
            version: value.version.version_string(),
            config: Some(rpc::forge::RackConfig {}),
            status: Some(rpc::forge::RackStatus {
                health: Some(health.into()),
                health_sources,
                lifecycle: Some(lifecycle),
            }),
        }
    }
}

impl From<rpc::forge::RackSearchFilter> for RackSearchFilter {
    fn from(filter: rpc::forge::RackSearchFilter) -> Self {
        RackSearchFilter {
            label: filter.label.map(model::metadata::LabelFilter::from),
            deleted: model::DeletedFilter::from(filter.deleted),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use model::metadata::LabelFilter;
    use model::rack::{LABEL_CHASSIS_MANUFACTURER, LABEL_LOCATION_DATACENTER};

    use super::*;

    // `RackSearchFilter::from` maps the optional proto label onto the model's
    // optional `LabelFilter`; project to that `label` field for each input.
    #[test]
    fn rack_search_filter_from_rpc() {
        value_scenarios!(
            run = |rpc_filter| RackSearchFilter::from(rpc_filter).label;
            "label with key and value" {
                rpc::forge::RackSearchFilter {
                    label: Some(rpc::forge::Label {
                        key: LABEL_LOCATION_DATACENTER.to_string(),
                        value: Some("az01".to_string()),
                    }),
                    ..Default::default()
                } => Some(LabelFilter {
                    key: LABEL_LOCATION_DATACENTER.to_string(),
                    value: Some("az01".to_string()),
                }),
            }

            "label with key only" {
                rpc::forge::RackSearchFilter {
                    label: Some(rpc::forge::Label {
                        key: LABEL_CHASSIS_MANUFACTURER.to_string(),
                        value: None,
                    }),
                    ..Default::default()
                } => Some(LabelFilter {
                    key: LABEL_CHASSIS_MANUFACTURER.to_string(),
                    value: None,
                }),
            }

            "no label" {
                rpc::forge::RackSearchFilter::default() => None,
            }
        );
    }

    #[test]
    fn rack_search_filter_maps_deleted_filter() {
        value_scenarios!(
            run = |deleted| RackSearchFilter::from(rpc::forge::RackSearchFilter {
                deleted,
                ..Default::default()
            })
            .deleted;
            "exclude by default" {
                rpc::forge::DeletedFilter::Exclude as i32 => model::DeletedFilter::Exclude,
            }
            "deleted only" {
                rpc::forge::DeletedFilter::Only as i32 => model::DeletedFilter::Only,
            }
            "include deleted" {
                rpc::forge::DeletedFilter::Include as i32 => model::DeletedFilter::Include,
            }
        );
    }
}
