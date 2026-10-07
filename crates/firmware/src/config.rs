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

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use model::DpuModel;
use model::firmware::{
    Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry, HostFirmwareConfig,
};
use model::site_explorer::{EndpointExplorationReport, ExploredEndpoint};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct FirmwareConfigSnapshot {
    pub(crate) data: HashMap<String, Firmware>,
}

impl FirmwareConfigSnapshot {
    pub fn values(&self) -> impl Iterator<Item = &Firmware> {
        self.data.values()
    }

    pub fn into_values(self) -> impl Iterator<Item = Firmware> {
        self.data.into_values()
    }

    pub fn find(&self, vendor: bmc_vendor::BMCVendor, model: &str) -> Option<Firmware> {
        let dpu_model = DpuModel::from(model);
        let model = if dpu_model != DpuModel::Unknown {
            dpu_model.to_string()
        } else {
            model.to_string()
        };
        let key = vendor_model_to_key(vendor, &model);
        let ret = self
            .data
            .get(&key)
            .or_else(|| {
                if vendor == bmc_vendor::BMCVendor::LenovoAMI {
                    // LenovoAMI identifies the BMC implementation; firmware bundle
                    // compatibility is still scoped by the Lenovo hardware model.
                    self.data
                        .get(&vendor_model_to_key(bmc_vendor::BMCVendor::Lenovo, &model))
                } else {
                    None
                }
            })
            .cloned();
        tracing::debug!(
            %key,
            firmware = ?ret,
            "Firmware config lookup completed",
        );
        ret
    }

    /// find_fw_info_for_host looks up the firmware config for the given endpoint
    pub fn find_fw_info_for_host(&self, endpoint: &ExploredEndpoint) -> Option<Firmware> {
        self.find_fw_info_for_host_report(&endpoint.report)
    }

    /// find_fw_info_for_host_report looks up the firmware config for the given endpoint report
    pub fn find_fw_info_for_host_report(
        &self,
        report: &EndpointExplorationReport,
    ) -> Option<Firmware> {
        report.vendor.and_then(|vendor| {
            // Use report.model if it is already filled or use model()
            // function to extract model from the report.
            report
                .model
                .as_ref()
                .and_then(|model| self.find(vendor, model))
                .or_else(|| report.model().and_then(|model| self.find(vendor, &model)))
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FirmwareConfig {
    base_map: HashMap<String, Firmware>,
    firmware_directory: PathBuf,
    #[cfg(test)]
    test_overrides: Vec<String>,
}

impl FirmwareConfig {
    pub fn new(
        firmware_dir: PathBuf,
        host_models: &HashMap<String, Firmware>,
        dpu_models: &HashMap<String, Firmware>,
    ) -> Self {
        let mut base_map: HashMap<String, Firmware> = Default::default();
        for host in host_models.values() {
            base_map.insert(vendor_model_to_key(host.vendor, &host.model), host.clone());
        }
        for dpu in dpu_models.values() {
            base_map.insert(
                vendor_model_to_key(
                    dpu.vendor,
                    &DpuModel::from(dpu.model.to_owned()).to_string(),
                ),
                dpu.clone(),
            );
        }
        Self {
            base_map,
            firmware_directory: firmware_dir,
            #[cfg(test)]
            test_overrides: vec![],
        }
    }

    pub fn create_snapshot(&self) -> FirmwareConfigSnapshot {
        let mut data = self.base_map.clone();
        if self.firmware_directory.to_string_lossy() != "" {
            self.merge_firmware_configs(&mut data, &self.firmware_directory);
        }

        #[cfg(test)]
        {
            // Fake configs to merge for unit tests
            for ovrd in &self.test_overrides {
                if let Err(err) = self.merge_from_string(&mut data, ovrd.clone()) {
                    tracing::error!(
                        override_config = %ovrd,
                        error = %err,
                        "Failed to merge test firmware override",
                    );
                }
            }
        }

        FirmwareConfigSnapshot { data }
    }

    /// Builds an effective catalog by applying runtime configs after the static
    /// and metadata.toml catalog has been loaded.
    pub fn create_snapshot_with_overrides(
        &self,
        overrides: impl IntoIterator<Item = HostFirmwareConfig>,
    ) -> FirmwareConfigSnapshot {
        let mut snapshot = self.create_snapshot();
        for firmware in overrides {
            merge_firmware_override(&mut snapshot.data, firmware);
        }
        snapshot
    }

    pub fn config_update_time(&self) -> Option<std::time::SystemTime> {
        if self.firmware_directory.to_string_lossy() == "" {
            return None;
        }

        let metadata = std::fs::metadata(self.firmware_directory.clone()).ok()?;

        metadata.modified().ok()
    }

    fn merge_firmware_configs(
        &self,
        map: &mut HashMap<String, Firmware>,
        firmware_directory: &PathBuf,
    ) {
        if !firmware_directory.is_dir() {
            tracing::error!(?firmware_directory, "Firmware directory does not exist",);
            return;
        }

        for dir in subdirectories_sorted_by_modification_date(firmware_directory) {
            if dir
                .path()
                .file_name()
                .unwrap_or(OsStr::new("."))
                .to_string_lossy()
                .starts_with(".")
            {
                continue;
            }
            let metadata_path = dir.path().join("metadata.toml");
            let metadata = match fs::read_to_string(metadata_path.clone()) {
                Ok(str) => str,
                Err(e) => {
                    tracing::error!(
                        ?metadata_path,
                        error = %e,
                        "Could not read firmware metadata",
                    );
                    continue;
                }
            };
            if let Err(e) = self.merge_from_string(map, metadata) {
                tracing::error!(
                    metadata_directory = ?dir.path(),
                    error = %e,
                    "Failed to merge firmware metadata",
                );
            }
        }
    }

    /// merge_from_string adds the given TOML based config to this Firmware.  Figment based merging won't work for this,
    /// as we want to append new FirmwareEntry instances instead of overwriting.  It is expected that this will be called
    /// on the metadata in order of oldest creation time to newest.
    fn merge_from_string(
        &self,
        map: &mut HashMap<String, Firmware>,
        config_str: String,
    ) -> eyre::Result<()> {
        let cfg: Firmware = toml::from_str(config_str.as_str())?;
        let key = vendor_model_to_key(cfg.vendor, &cfg.model);

        let Some(cur_model) = map.get_mut(&key) else {
            // We haven't seen this model before, so use this as given.
            map.insert(key, cfg);
            return Ok(());
        };

        if !cfg.ordering.is_empty() {
            // Newer ordering definitions take precedence.  For now we don't consider this at a specific version level.
            cur_model.ordering = cfg.ordering
        }

        // if explicit_start_needed is true, it should take precedence. We shouldn't be doing automatic upgrades.
        if cfg.explicit_start_needed {
            cur_model.explicit_start_needed = true;
        }

        for (new_type, new_component) in cfg.components {
            if let Some(cur_component) = cur_model.components.get_mut(&new_type) {
                // The simple fields from the newer version should be used if specified
                if new_component.current_version_reported_as.is_some() {
                    cur_component.current_version_reported_as =
                        new_component.current_version_reported_as;
                }
                if new_component.preingest_upgrade_when_below.is_some() {
                    cur_component.preingest_upgrade_when_below =
                        new_component.preingest_upgrade_when_below;
                }
                if new_component.known_firmware.iter().any(|x| x.default) {
                    // The newer one lists a default, remove default from the old.
                    cur_component.known_firmware = cur_component
                        .known_firmware
                        .iter()
                        .map(|x| {
                            let mut x = x.clone();
                            x.default = false;
                            x
                        })
                        .collect();
                }
                cur_component
                    .known_firmware
                    .extend(new_component.known_firmware.iter().cloned());
            } else {
                // Nothing for this component
                cur_model.components.insert(new_type, new_component);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn add_test_override(&mut self, ovrd: String) {
        self.test_overrides.push(ovrd);
    }
}

// Runtime DB configs overlay the already-built catalog. Metadata entries remain
// unless the runtime config supplies the same component or firmware version.
fn merge_firmware_override(
    map: &mut HashMap<String, Firmware>,
    override_config: HostFirmwareConfig,
) {
    let HostFirmwareConfig {
        vendor,
        model,
        components,
        explicit_start_needed,
        ordering,
    } = override_config;
    let key = vendor_model_to_key(vendor, &model);

    let Some(cur_model) = map.get_mut(&key) else {
        map.insert(
            key,
            Firmware {
                vendor,
                model,
                components,
                explicit_start_needed: explicit_start_needed.unwrap_or(false),
                ordering,
            },
        );
        return;
    };

    cur_model.vendor = vendor;
    cur_model.model = model;
    if let Some(explicit_start_needed) = explicit_start_needed {
        cur_model.explicit_start_needed = explicit_start_needed;
    }
    append_override_ordering(&mut cur_model.ordering, &ordering);

    for (component_type, override_component) in components {
        if let Some(cur_component) = cur_model.components.get_mut(&component_type) {
            merge_component_override(cur_component, override_component);
        } else {
            cur_model
                .components
                .insert(component_type, override_component);
        }
    }
}

// Keep metadata ordering stable and append only ordering entries introduced by
// the runtime config.
fn append_override_ordering(
    current_ordering: &mut Vec<FirmwareComponentType>,
    override_ordering: &[FirmwareComponentType],
) {
    for component_type in override_ordering {
        if !current_ordering.contains(component_type) {
            current_ordering.push(*component_type);
        }
    }
}

// Component-level runtime values win where they are provided; omitted fields
// keep the metadata value.
fn merge_component_override(
    cur_component: &mut FirmwareComponent,
    override_component: FirmwareComponent,
) {
    if override_component.current_version_reported_as.is_some() {
        cur_component.current_version_reported_as = override_component.current_version_reported_as;
    }
    if override_component.preingest_upgrade_when_below.is_some() {
        cur_component.preingest_upgrade_when_below =
            override_component.preingest_upgrade_when_below;
    }
    if override_component
        .known_firmware
        .iter()
        .any(|firmware| firmware.default)
    {
        for firmware in &mut cur_component.known_firmware {
            firmware.default = false;
        }
    }

    for firmware in override_component.known_firmware {
        upsert_firmware_entry(&mut cur_component.known_firmware, firmware);
    }
}

// Runtime versions are upserts keyed by version string, not blind appends.
fn upsert_firmware_entry(entries: &mut Vec<FirmwareEntry>, firmware: FirmwareEntry) {
    if let Some(index) = entries
        .iter()
        .position(|existing| existing.version == firmware.version)
    {
        entries[index] = firmware;
    } else {
        entries.push(firmware);
    }
}

fn vendor_model_to_key(vendor: bmc_vendor::BMCVendor, model: &str) -> String {
    format!("{vendor}:{}", model.to_lowercase())
}

fn subdirectories_sorted_by_modification_date(topdir: &PathBuf) -> Vec<fs::DirEntry> {
    let Ok(dirs) = topdir.read_dir() else {
        tracing::error!(
            firmware_directory = ?topdir,
            "Unreadable firmware directory",
        );
        return vec![];
    };

    // We sort in ascending modification time so that we will use the newest made firmware metadata
    let mut dirs: Vec<fs::DirEntry> = dirs.filter_map(|x| x.ok()).collect();
    dirs.sort_unstable_by(|x, y| {
        let x_time = match x.metadata() {
            Err(_) => SystemTime::now(),
            Ok(x) => match x.modified() {
                Err(_) => SystemTime::now(),
                Ok(x) => x,
            },
        };
        let y_time = match y.metadata() {
            Err(_) => SystemTime::now(),
            Ok(y) => match y.modified() {
                Err(_) => SystemTime::now(),
                Ok(y) => y,
            },
        };
        x_time.partial_cmp(&y_time).unwrap_or(Ordering::Equal)
    });
    dirs
}
