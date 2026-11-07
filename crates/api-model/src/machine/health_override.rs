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

pub use crate::health::HealthReportSources;

pub const HARDWARE_HEALTH_OVERRIDE_PREFIX: &str = "hardware-health.";

pub struct MaintenanceOverride {
    pub maintenance_reference: String,
    pub maintenance_start_time: Option<chrono::DateTime<chrono::Utc>>,
}

/// Machine-specific methods for HealthReportSources.
impl HealthReportSources {
    /// Derive legacy Maintenance mode fields.
    /// Determined by the value of a well-known health source, that is also set
    /// via SetMaintenance API.
    pub fn maintenance_override(&self) -> Option<MaintenanceOverride> {
        let ovr = self.merges.get("maintenance")?;
        let maintenance_alert_id = "Maintenance".parse().unwrap();
        let alert = ovr
            .alerts
            .iter()
            .find(|alert| alert.id == maintenance_alert_id)?;
        Some(MaintenanceOverride {
            maintenance_reference: alert.message.clone(),
            maintenance_start_time: alert.in_alert_since,
        })
    }

    pub fn is_hardware_health_override_source(source: &str) -> bool {
        source.starts_with(HARDWARE_HEALTH_OVERRIDE_PREFIX)
    }
}
