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

use nv_redfish::schema::computer_system::{
    BootSource, BootSourceOverrideEnabled, BootSourceOverrideMode,
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::BootOptionKind;

/// Complete boot-source override state, independent of its Redfish representation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootSourceOverride {
    /// Requested firmware mode; absent when no override has been set.
    pub mode: Option<BootSourceOverrideMode>,
    /// Override lifetime, including Once, Continuous or Disabled.
    pub enabled: Option<BootSourceOverrideEnabled>,
    /// Requested boot target, such as Hdd, Pxe or Cd.
    pub target: Option<BootSource>,
}

impl BootSourceOverride {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let value = [
            (
                "BootSourceOverrideMode",
                self.mode.map(|value| json!(value)),
            ),
            (
                "BootSourceOverrideEnabled",
                self.enabled.map(|value| json!(value)),
            ),
            (
                "BootSourceOverrideTarget",
                self.target.map(|value| json!(value)),
            ),
        ]
        .into_iter()
        .fold(serde_json::Map::new(), |mut object, (key, value)| {
            if let Some(value) = value {
                object.insert(key.to_string(), value);
            }
            object
        });
        serde_json::Value::Object(value)
    }

    fn apply(&mut self, patch: BootSourceOverridePatch) {
        self.mode = patch.mode.unwrap_or(self.mode);
        self.enabled = patch.enabled.unwrap_or(self.enabled);
        self.target = patch.target.unwrap_or(self.target);
    }
}

/// Partial boot-source override: None preserves a field; Some(None) clears it.
#[derive(Clone, Debug, Default)]
pub struct BootSourceOverridePatch {
    /// Change or clear the firmware mode.
    pub mode: Option<Option<BootSourceOverrideMode>>,
    /// Change or clear the override lifetime.
    pub enabled: Option<Option<BootSourceOverrideEnabled>>,
    /// Change or clear the boot target.
    pub target: Option<Option<BootSource>>,
}

impl TryFrom<&serde_json::Value> for BootSourceOverridePatch {
    type Error = String;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        // Preserve the mock's existing PATCH semantics: null and non-string
        // values clear a field, while an omitted field is left unchanged.
        fn field<T: DeserializeOwned + PartialEq>(
            value: &serde_json::Value,
            name: &str,
            unsupported: T,
        ) -> Result<Option<Option<T>>, String> {
            value
                .get(name)
                .map(|value| {
                    if !value.is_string() {
                        return Ok(None);
                    }
                    let parsed = T::deserialize(value)
                        .map_err(|error| format!("invalid {name}: {error}"))?;
                    if parsed == unsupported {
                        return Err(format!("unsupported {name}: {value}"));
                    }
                    Ok(Some(parsed))
                })
                .transpose()
        }

        Ok(Self {
            mode: field(
                value,
                "BootSourceOverrideMode",
                BootSourceOverrideMode::UnsupportedValue,
            )?,
            enabled: field(
                value,
                "BootSourceOverrideEnabled",
                BootSourceOverrideEnabled::UnsupportedValue,
            )?,
            target: field(
                value,
                "BootSourceOverrideTarget",
                BootSource::UnsupportedValue,
            )?,
        })
    }
}

/// Changes requested by one boot operation; omitted fields preserve current values.
#[derive(Clone, Debug, Default)]
pub struct BootConfigPatch {
    /// Partial changes to the boot-source override.
    pub source: BootSourceOverridePatch,
    /// Replace the standard boot order when present.
    pub order: Option<Vec<String>>,
    /// Replace the HPE OEM boot order when present.
    pub hpe_order: Option<Vec<String>>,
}

/// Boot configuration owned by the backend.
#[derive(Clone, Debug, Default)]
pub struct BootState {
    /// Complete boot-source override.
    pub source: BootSourceOverride,
    /// Standard boot order, or None to use the profile default.
    pub order: Option<Vec<String>>,
    /// HPE OEM persistent order, independent of standard BootOption IDs.
    pub hpe_order: Option<Vec<String>>,
    options: Vec<(String, BootOptionKind)>,
}

impl BootState {
    pub(crate) fn new(options: Vec<(String, BootOptionKind)>) -> Self {
        Self {
            options,
            ..Default::default()
        }
    }

    pub(crate) fn apply(&mut self, patch: BootConfigPatch) {
        self.source.apply(patch.source);
        if let Some(order) = patch.order {
            self.order = Some(order);
        }
        if let Some(order) = patch.hpe_order {
            self.hpe_order = Some(order);
        }
    }

    pub(crate) fn hpe_boot_order(&self) -> Vec<String> {
        self.hpe_order.clone().unwrap_or_else(|| {
            self.options
                .iter()
                .map(|(reference, kind)| {
                    let prefix = match kind {
                        BootOptionKind::Disk => "HD",
                        BootOptionKind::Network => "NIC",
                    };
                    format!("{prefix}.BootOption.{reference}")
                })
                .collect()
        })
    }

    pub(crate) fn on_boot_completed(&mut self) {
        if self.source.enabled == Some(BootSourceOverrideEnabled::Once) {
            self.source.enabled = Some(BootSourceOverrideEnabled::Disabled);
        }
    }

    pub(crate) fn persistent_selection(&self) -> Option<BootOptionKind> {
        self.hpe_order
            .as_ref()
            .and_then(|order| {
                order.iter().find_map(|entry| {
                    let (kind, reference) = entry
                        .strip_prefix("HD.BootOption.")
                        .map(|reference| (BootOptionKind::Disk, reference))
                        .or_else(|| {
                            entry
                                .strip_prefix("NIC.BootOption.")
                                .map(|reference| (BootOptionKind::Network, reference))
                        })?;
                    self.options
                        .iter()
                        .find(|(option, option_kind)| *option_kind == kind && option == reference)
                        .map(|(_, kind)| *kind)
                })
            })
            .or_else(|| {
                self.order.as_ref()?.first().and_then(|reference| {
                    self.options
                        .iter()
                        .find(|(option, _)| option == reference)
                        .map(|(_, kind)| *kind)
                })
            })
            .or_else(|| self.options.first().map(|(_, kind)| *kind))
    }

    pub(crate) fn current_selection(&self) -> Option<BootOptionKind> {
        let source = &self.source;
        let selection = if source
            .enabled
            .is_some_and(|value| value != BootSourceOverrideEnabled::Disabled)
            && source.mode == Some(BootSourceOverrideMode::Uefi)
        {
            match source.target {
                Some(BootSource::Hdd) => Some(BootOptionKind::Disk),
                Some(BootSource::UefiHttp | BootSource::Pxe) => Some(BootOptionKind::Network),
                _ => None,
            }
            .filter(|kind| self.options.iter().any(|(_, option)| option == kind))
        } else {
            None
        };
        selection.or_else(|| self.persistent_selection())
    }
}
