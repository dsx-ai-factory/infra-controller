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
use ::carbide_uuid::machine::MachineId;
use carbide_libmlx_model::device::info::MlxDeviceInfo;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::device::discovery::DeviceDiscoveryError;
use crate::device::filters::{DeviceFilter, DeviceFilterSet};

// MlxDeviceReport represents a complete device discovery report
// with metadata and filtered device results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlxDeviceReport {
    // hostname is the system hostname where the report was generated.
    pub hostname: String,
    // timestamp is when the report was generated.
    pub timestamp: DateTime<Utc>,
    // devices contains the discovered devices matching any applied filters.
    pub devices: Vec<MlxDeviceInfo>,
    // filters contains the filter set used to generate this report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<DeviceFilterSet>,

    pub machine_id: Option<MachineId>,
}

impl Default for MlxDeviceReport {
    fn default() -> Self {
        Self::new()
    }
}

impl MlxDeviceReport {
    // new creates a new report with current hostname and timestamp.
    pub fn new() -> Self {
        let hostname = hostname::get()
            .unwrap_or_else(|_| "unknown".into())
            .to_string_lossy()
            .to_string();

        Self {
            hostname,
            timestamp: Utc::now(),
            devices: Vec::new(),
            filters: None,
            machine_id: None,
        }
    }

    // with_filter adds a single filter to the report's filter set.
    pub fn with_filter(mut self, filter: DeviceFilter) -> Self {
        match self.filters {
            Some(ref mut filter_set) => {
                filter_set.add_filter(filter);
            }
            None => {
                let mut filter_set = DeviceFilterSet::new();
                filter_set.add_filter(filter);
                self.filters = Some(filter_set);
            }
        }
        self
    }

    // with_filter_set merges the provided filter set with the existing one.
    pub fn with_filter_set(mut self, filter_set: DeviceFilterSet) -> Self {
        match self.filters {
            Some(ref mut existing_filter_set) => {
                for filter in filter_set.filters {
                    existing_filter_set.add_filter(filter);
                }
            }
            None => {
                self.filters = Some(filter_set);
            }
        }
        self
    }

    /// Discover devices using the configured filters, returning display errors.
    /// Call [`Self::collect_with_settlement`] when process completion determines
    /// whether a caller may release hardware access.
    pub fn collect(self) -> Result<Self, String> {
        self.collect_with_settlement()
            .map_err(DeviceDiscoveryError::into_legacy_message)
    }

    /// Discover devices and apply the configured filters while preserving typed
    /// query failures, including unknown process completion. This does not
    /// acquire hardware exclusion; callers retain their claim through the query.
    /// Uses [`crate::device::discovery::discover_devices_with_settlement`],
    /// including its synchronous, unbounded query wait and exit-code policy.
    pub fn collect_with_settlement(mut self) -> Result<Self, DeviceDiscoveryError> {
        self.devices = if let Some(ref filter_set) = self.filters {
            if filter_set.has_filters() {
                // Use filtered discovery
                let all_devices = crate::device::discovery::discover_devices_with_settlement()?;
                all_devices
                    .into_iter()
                    .filter(|device| filter_set.matches(device))
                    .collect()
            } else {
                // No filters specified, get all devices
                crate::device::discovery::discover_devices_with_settlement()?
            }
        } else {
            // No filter set, get all devices
            crate::device::discovery::discover_devices_with_settlement()?
        };

        Ok(self)
    }
}
