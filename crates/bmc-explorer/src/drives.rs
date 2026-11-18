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

//! Drive firmware versions reported through `Systems/{id}/Storage/{id}/Drives`.
//!
//! Most BMCs leave drives out of the `UpdateService` firmware inventory, so
//! drive firmware is read from the Storage subsystem and reported as extra
//! firmware inventory entries with the ID `Drive_<Model>_<DriveId>`. Each
//! entry carries the drive's model as data, which is what host firmware
//! configuration selects a drive model by; the ID is only for people.

use std::collections::BTreeMap;
use std::sync::Arc;

use model::site_explorer::{DriveIdentity, Inventory as ModelInventory};
use nv_redfish::Bmc;
use nv_redfish::computer_system::{ComputerSystem, Drive};
use nv_redfish::schema::drive::{Drive as DriveSchema, MediaType};
use nv_redfish::schema::resource::State;
use nv_redfish::schema::storage::StorageController as StorageControllerSchema;

/// Longest reported value accepted as a drive firmware version. Drive firmware
/// revisions are at most 8 characters, the width of the ATA and NVMe firmware
/// revision fields (SCSI uses 4), so a longer value is something else, such as
/// the "NVMe Spec v1.4" some AMI BMCs report in `Revision`.
const MAX_VERSION_LEN: usize = 8;

pub(crate) struct ExploredDrives<B: Bmc> {
    /// Each drive, once, with the controllers embedded in every Storage that
    /// links it.
    drives: Vec<(Drive<B>, Vec<Arc<StorageControllerSchema>>)>,
}

impl<B: Bmc> ExploredDrives<B> {
    /// Collects the drives of every Storage subsystem of `system`.
    ///
    /// Drive inventory is best effort: a Storage subsystem whose collection or
    /// drives cannot be fetched is logged and left out, and never fails the
    /// exploration. A drive linked from two Storage subsystems is kept once,
    /// with the controllers of both, so the result does not depend on the
    /// order the BMC lists them in.
    pub(crate) async fn explore(system: &ComputerSystem<B>) -> Self {
        let storages = match system.storage_controllers().await {
            Ok(Some(storages)) => storages,
            Ok(None) => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, system_id = %system.raw().id, "Failed to fetch storage subsystems");
                Vec::new()
            }
        };
        let mut drives = BTreeMap::new();
        for storage in storages {
            let storage_drives = match storage.drives().await {
                Ok(storage_drives) => storage_drives.unwrap_or_default(),
                Err(error) => {
                    tracing::warn!(%error, storage_id = %storage.raw().id, "Failed to fetch storage drives");
                    continue;
                }
            };
            // Embedded controllers come with the Storage resource, so reading
            // them costs no request. They only supply a fallback version.
            let controllers = match storage.storage_controllers().await {
                Ok(controllers) => controllers.unwrap_or_default(),
                Err(error) => {
                    tracing::debug!(%error, storage_id = %storage.raw().id, "Storage controllers not usable");
                    Vec::new()
                }
            };
            for drive in storage_drives {
                drives
                    .entry(drive.raw().odata_id.clone())
                    .or_insert_with(|| (drive, Vec::new()))
                    .1
                    .extend(controllers.iter().cloned());
            }
        }
        Self {
            drives: drives.into_values().collect(),
        }
    }

    /// Converts collected drives to firmware inventory entries without BMC requests.
    pub(crate) fn to_model(&self) -> Vec<ModelInventory> {
        self.drives
            .iter()
            .filter_map(|(drive, controllers)| drive.raw().to_firmware_inventory(controllers))
            .collect()
    }
}

/// Converts a collected drive schema to a firmware inventory entry.
trait DriveExt {
    /// Returns `None` for a drive that must not be compared or upgraded: one
    /// that is not enabled or has no model or usable firmware version.
    /// `FailurePredicted` is not a filter: it is a prediction, not a failure,
    /// so a flagged drive is still reported.
    ///
    /// `controllers` are the controllers embedded in the drive's Storage. They
    /// supply the version only when the drive itself reports none.
    fn to_firmware_inventory(
        &self,
        controllers: &[Arc<StorageControllerSchema>],
    ) -> Option<ModelInventory>;
}

impl DriveExt for DriveSchema {
    fn to_firmware_inventory(
        &self,
        controllers: &[Arc<StorageControllerSchema>],
    ) -> Option<ModelInventory> {
        let state = self
            .status
            .as_ref()
            .and_then(|status| status.state.flatten());
        if state != Some(State::Enabled) {
            return None;
        }
        let model = trimmed(&self.model)?;
        // FirmwareVersion is the dedicated property. Revision is the older,
        // looser one ("firmware or hardware version") and the only one Dell
        // iDRAC fills in.
        let firmware_version = trimmed(&self.firmware_version);
        let revision = trimmed(&self.revision);
        let Some(version) = [firmware_version, revision]
            .into_iter()
            .flatten()
            .find(|version| is_drive_version(version))
            .or_else(|| controller_version(controllers, model))
        else {
            // Recurs on every exploration; hosts whose configured drive
            // component matches no entry are surfaced separately.
            tracing::debug!(
                drive_id = %self.id,
                model,
                firmware_version,
                revision,
                "Drive reports no usable firmware version"
            );
            return None;
        };
        let media_type = self.media_type.flatten().and_then(|media| match media {
            MediaType::Ssd => Some("SSD"),
            MediaType::Hdd => Some("HDD"),
            MediaType::Smr => Some("SMR"),
            MediaType::UnsupportedValue => None,
        });
        Some(ModelInventory {
            id: format!("Drive_{model}_{}", self.id),
            description: Some(self.odata_id.to_string()),
            version: Some(version.to_string()),
            release_date: None,
            drive: Some(DriveIdentity {
                model: model.to_string(),
                media_type: media_type.map(str::to_string),
            }),
        })
    }
}

/// Whether `version` can be a drive firmware revision.
fn is_drive_version(version: &str) -> bool {
    version.len() <= MAX_VERSION_LEN && !version.contains(char::is_whitespace)
}

/// The version of the Storage controllers whose model equals the drive's.
///
/// Some AMI BMCs, such as the Lenovo HS350X V3's, report an NVMe drive's
/// firmware only on its controller in the embedded `StorageControllers` array.
/// Two drives of one model in a Storage are indistinguishable there, so
/// matching controllers that disagree give no version.
fn controller_version<'a>(
    controllers: &'a [Arc<StorageControllerSchema>],
    model: &str,
) -> Option<&'a str> {
    let mut versions = controllers
        .iter()
        .filter(|controller| trimmed(&controller.model) == Some(model))
        .map(|controller| trimmed(&controller.firmware_version));
    let version = versions.next()??;
    (is_drive_version(version) && versions.all(|other| other == Some(version))).then_some(version)
}

/// The trimmed value of an optional string property, or `None` when it is
/// absent, null or blank.
fn trimmed(value: &Option<Option<String>>) -> Option<&str> {
    value
        .as_ref()
        .and_then(Option::as_deref)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_test_support::value_scenarios;
    use serde_json::{Value, json};

    use super::{DriveExt, DriveIdentity, DriveSchema, ModelInventory, StorageControllerSchema};

    /// A Dell iDRAC SATA HDD as captured from an R760xd2, reduced to the
    /// properties the conversion reads.
    fn dell_hdd() -> Value {
        json!({
            "@odata.id": "/redfish/v1/Systems/System.Embedded.1/Storage/NonRAID.Slot.5-1/Drives/Disk.Bay.0:Enclosure.Internal.0-3:NonRAID.Slot.5-1",
            "Id": "Disk.Bay.0:Enclosure.Internal.0-3:NonRAID.Slot.5-1",
            "Name": "Physical Disk 0:3:0",
            "MediaType": "HDD",
            "Model": "TOSHIBA MG11ACA2",
            "Revision": "GR02",
            "FailurePredicted": false,
            "Status": { "Health": "OK", "State": "Enabled" }
        })
    }

    const DELL_HDD_PATH: &str = "/redfish/v1/Systems/System.Embedded.1/Storage/NonRAID.Slot.5-1/Drives/Disk.Bay.0:Enclosure.Internal.0-3:NonRAID.Slot.5-1";

    /// Applies `changes` to the Dell HDD and converts it.
    fn convert(changes: Value) -> Option<ModelInventory> {
        let mut drive = dell_hdd();
        for (key, value) in changes.as_object().unwrap() {
            drive[key] = value.clone();
        }
        let drive: DriveSchema = serde_json::from_value(drive).unwrap();
        drive.to_firmware_inventory(&[])
    }

    /// The entry expected for the Dell HDD with `version` and `media_type`.
    fn dell_hdd_entry(version: &str, media_type: Option<&str>) -> Option<ModelInventory> {
        Some(ModelInventory {
            id: "Drive_TOSHIBA MG11ACA2_Disk.Bay.0:Enclosure.Internal.0-3:NonRAID.Slot.5-1"
                .to_string(),
            description: Some(DELL_HDD_PATH.to_string()),
            version: Some(version.to_string()),
            release_date: None,
            drive: Some(DriveIdentity {
                model: "TOSHIBA MG11ACA2".to_string(),
                media_type: media_type.map(str::to_string),
            }),
        })
    }

    #[test]
    fn drive_firmware_inventory_entry() {
        let hdd = |version| dell_hdd_entry(version, Some("HDD"));
        value_scenarios!(run = convert;
            "version source" {
                json!({}) => hdd("GR02"),
                json!({ "Revision": null, "FirmwareVersion": "1XET7106" }) => hdd("1XET7106"),
                json!({ "FirmwareVersion": "LDDD2U2Q" }) => hdd("LDDD2U2Q"),
                json!({ "FirmwareVersion": "  ", "Revision": "GR02" }) => hdd("GR02"),
            }

            "version validity" {
                json!({ "Revision": " EIED51.C " }) => hdd("EIED51.C"),
                json!({ "Revision": "EIED51.CX" }) => None,
                json!({ "Revision": "NVMe Spec v1.4" }) => None,
                json!({ "Revision": null }) => None,
                json!({ "FirmwareVersion": "NVMe Spec v1.4" }) => hdd("GR02"),
            }

            "media type is reported, never filtered" {
                json!({ "MediaType": "SSD" }) => dell_hdd_entry("GR02", Some("SSD")),
                json!({ "MediaType": "SMR" }) => dell_hdd_entry("GR02", Some("SMR")),
                json!({ "MediaType": null }) => dell_hdd_entry("GR02", None),
            }

            "model" {
                json!({ "Model": "TOSHIBA MG11ACA2 " }) => hdd("GR02"),
                json!({ "Model": "" }) => None,
            }

            "status and health" {
                json!({ "Status": { "State": "Absent" } }) => None,
                json!({ "Status": null }) => None,
                json!({ "FailurePredicted": true }) => hdd("GR02"),
            }
        );
    }

    /// An NVMe drive as a Lenovo HS350X V3 reports it: `Revision` holds the
    /// NVMe specification version, and the firmware version is only on the
    /// matching controller of its Storage.
    fn ami_nvme_drive() -> Value {
        json!({
            "@odata.id": "/redfish/v1/Systems/Self/Storage/1/Drives/0",
            "Id": "0",
            "Name": "Drive 0",
            "MediaType": "SSD",
            "Model": "Micron_7450_MTFDKBG960TFR",
            "Revision": "NVMe Spec v1.4",
            "Status": { "State": "Enabled" }
        })
    }

    /// Converts the AMI NVMe drive with `changes` applied, against embedded
    /// controllers given as `(model, firmware version)`.
    fn convert_with_controllers(
        (changes, controllers): (Value, &[(&str, &str)]),
    ) -> Option<String> {
        let mut drive = ami_nvme_drive();
        for (key, value) in changes.as_object().unwrap() {
            drive[key] = value.clone();
        }
        let drive: DriveSchema = serde_json::from_value(drive).unwrap();
        let controllers = controllers
            .iter()
            .enumerate()
            .map(|(index, (model, version))| {
                let controller: StorageControllerSchema = serde_json::from_value(json!({
                    "@odata.id": format!("/redfish/v1/Systems/Self/Storage/1#/StorageControllers/{index}"),
                    "MemberId": format!("StorageUnit_{index}"),
                    "Model": model,
                    "FirmwareVersion": version,
                }))
                .unwrap();
                Arc::new(controller)
            })
            .collect::<Vec<_>>();
        drive
            .to_firmware_inventory(&controllers)
            .and_then(|entry| entry.version)
    }

    #[test]
    fn drive_version_from_storage_controller() {
        const MICRON: &str = "Micron_7450_MTFDKBG960TFR";
        const INTEL: &str = "INTEL SSDPF2KE016T1O";
        let version = |version: &str| Some(version.to_string());
        value_scenarios!(run = convert_with_controllers;
            "matching controller" {
                (json!({}), &[(INTEL, "9CV10420"), (MICRON, "E2MU116")][..]) => version("E2MU116"),
                (json!({}), &[(" Micron_7450_MTFDKBG960TFR ", " E2MU116 ")][..]) => version("E2MU116"),
                (json!({}), &[(MICRON, "E2MU116"), (MICRON, "E2MU116")][..]) => version("E2MU116"),
            }

            "no usable controller version" {
                (json!({}), &[][..]) => None,
                (json!({}), &[(INTEL, "9CV10420")][..]) => None,
                (json!({}), &[(MICRON, "NVMe Spec v1.4")][..]) => None,
                (json!({}), &[(MICRON, "E2MU116"), (MICRON, "E2MU300")][..]) => None,
            }

            "drive's own version wins" {
                (json!({ "FirmwareVersion": "E2MU300" }), &[(MICRON, "E2MU116")][..]) => version("E2MU300"),
            }
        );
    }
}
