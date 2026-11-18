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

use std::collections::BTreeMap;
use std::time::SystemTime;

use async_trait::async_trait;
use nv_redfish::core::Bmc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{DriverOutcome, OpCx, PlatformError};

/// BMC manager attribute names and values, such as Dell iDRAC attributes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerSettings {
    /// Manager attribute values by attribute name.
    pub attributes: BTreeMap<String, Value>,
}

/// One manager attribute whose reported value differs from the expectation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerSettingsDiff {
    /// Attribute name.
    pub key: String,
    /// Value NICo wants.
    pub expected: Value,
    /// Value the BMC reports; `None` when the BMC does not expose the attribute.
    pub actual: Option<Value>,
}

/// Whether the platform's manager settings are applied, with every difference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerSettingsStatus {
    /// True when `differences` is empty.
    pub is_applied: bool,
    /// Expected attributes whose current value differs.
    pub differences: Vec<ManagerSettingsDiff>,
}

/// The manager's time configuration as the platform reports it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeStatus {
    /// Whether NTP is enabled; `None` when the platform has no such switch.
    pub ntp_enabled: Option<bool>,
    /// The NTP servers the BMC lists, in order, without empty slots.
    pub ntp_servers: Vec<String>,
    /// The manager's clock; `None` when the BMC reports none.
    pub now: Option<SystemTime>,
    /// The manager's offset from UTC, such as `+00:00`; `None` when unreported.
    pub utc_offset: Option<String>,
}

/// BMC reset, factory defaults, time configuration, IPMI over LAN, and
/// manager settings.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait BmcControl<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn BmcControl<B>;

    async fn reset(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().reset(cx).await
    }

    async fn reset_to_factory_defaults(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().reset_to_factory_defaults(cx).await
    }

    async fn set_ntp_servers(
        &self,
        cx: &OpCx<'_, B>,
        servers: &[String],
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().set_ntp_servers(cx, servers).await
    }

    async fn set_utc_timezone(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        self.standard().set_utc_timezone(cx).await
    }

    /// The time configuration [`Self::set_ntp_servers`] and
    /// [`Self::set_utc_timezone`] write, read where the platform keeps it.
    async fn time_status(&self, cx: &OpCx<'_, B>) -> Result<TimeStatus, PlatformError> {
        self.standard().time_status(cx).await
    }

    async fn ipmi_over_lan_enabled(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        self.standard().ipmi_over_lan_enabled(cx).await
    }

    async fn set_ipmi_over_lan(
        &self,
        cx: &OpCx<'_, B>,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().set_ipmi_over_lan(cx, enabled).await
    }

    /// Writes the manager settings the platform needs for provisioning, plus
    /// `profile`, which takes precedence.
    async fn apply_settings(
        &self,
        cx: &OpCx<'_, B>,
        profile: &ManagerSettings,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().apply_settings(cx, profile).await
    }

    /// Checks the manager settings the platform needs for provisioning;
    /// profile values are written by [`Self::apply_settings`] but not checked.
    async fn settings_status(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<ManagerSettingsStatus, PlatformError> {
        self.standard().settings_status(cx).await
    }
}
