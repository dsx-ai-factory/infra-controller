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
use std::collections::HashMap;
use std::fmt::{Display, Write};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use super::hardware_info::CpuInfo;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Sku {
    pub schema_version: u32,
    pub id: String,
    pub description: String,
    pub created: DateTime<Utc>,
    pub components: SkuComponents,
    pub device_type: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for Sku {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let schema_version: u32 = row.try_get::<i32, &str>("schema_version")? as u32;
        let id: String = row.try_get("id")?;
        let description: String = row.try_get("description")?;
        let created: DateTime<Utc> = row.try_get("created")?;
        let components = row
            .try_get::<sqlx::types::Json<SkuComponents>, _>("components")?
            .0;
        let device_type = row.try_get("device_type")?;
        Ok(Sku {
            schema_version,
            id,
            description,
            created,
            components,
            device_type,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SkuComponents {
    pub chassis: SkuComponentChassis,
    pub cpus: Vec<SkuComponentCpu>,
    pub gpus: Vec<SkuComponentGpu>,
    pub memory: Vec<SkuComponentMemory>,
    pub infiniband_devices: Vec<SkuComponentInfinibandDevices>,
    #[serde(default)]
    pub storage: Vec<SkuComponentStorage>,
    #[serde(default)]
    pub tpm: Option<SkuComponentTpm>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct SkuComponentChassis {
    pub vendor: String,
    pub model: String,
    pub architecture: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct SkuComponentCpu {
    pub vendor: String,
    pub model: String,
    pub thread_count: u32,
    pub count: u32,
}

impl From<&CpuInfo> for SkuComponentCpu {
    fn from(value: &CpuInfo) -> Self {
        SkuComponentCpu {
            vendor: value.vendor.clone(),
            model: value.model.clone(),
            count: value.sockets,
            thread_count: value.threads,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct SkuComponentGpu {
    pub vendor: String,
    pub model: String,
    pub total_memory: String,
    pub count: u32,
}

impl Display for SkuComponentGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}/{}", self.count, self.vendor, self.model)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct SkuComponentMemory {
    pub memory_type: String,
    pub capacity_mb: u32,
    pub count: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Ord, PartialOrd)]
pub struct SkuComponentInfinibandDevices {
    /// The Vendor of the InfiniBand device. E.g. `Mellanox`
    pub vendor: String,
    /// The Device Name of the InfiniBand device. E.g. `MT2910 Family [ConnectX-7]`
    pub model: String,
    /// The total amount of InfiniBand devices of the given
    /// vendor and model combination
    pub count: u32,
    /// The indexes of InfiniBand Devices which are not active and thereby can
    /// not be utilized by Instances.
    /// Inactive devices are devices where for example there is no connection
    /// between the port and the InfiniBand switch.
    /// Example: A `{count: 4, inactive_devices: [1,3]}` means that the devices
    /// with index `0` and `2` of the Host can be utilized, and devices with index
    /// `1` and `3` can not be used.
    pub inactive_devices: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct SkuComponentStorage {
    pub model: String,
    pub count: u32,
    /// Inclusive lower bound (MB) each drive in this group must meet. `None`
    /// means no lower bound. Only consulted for schema version >= 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size_mb: Option<u32>,
    /// Inclusive upper bound (MB) each drive in this group must meet. `None`
    /// means no upper bound. Only consulted for schema version >= 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size_mb: Option<u32>,
    /// Regex patterns matched against a drive's sysfs/PCI path to validate its
    /// physical location. Empty means the drive location is not checked. On a
    /// discovered (actual) SKU this holds the concrete path of each drive.
    /// Only consulted for schema version >= 5.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pci_patterns: Vec<String>,
}

impl Display for SkuComponentStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "model: {} count {}", self.model, self.count)?;
        match (self.min_size_mb, self.max_size_mb) {
            (None, None) => {}
            (min, max) => write!(
                f,
                " size {}-{} MB",
                min.map(|v| v.to_string())
                    .unwrap_or_else(|| "*".to_string()),
                max.map(|v| v.to_string())
                    .unwrap_or_else(|| "*".to_string()),
            )?,
        }
        if !self.pci_patterns.is_empty() {
            write!(f, " pci {:?}", self.pci_patterns)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub struct SkuComponentTpm {
    pub vendor: String,
    pub version: String,
}

impl Display for SkuComponentTpm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vendor: {} version: {}", self.vendor, self.version)
    }
}

// Store information for communication between the state
// machine and other components.  This is kept as a json
// field in the machines table
#[derive(Clone, Debug, Default, Deserialize, FromRow, Serialize)]
pub struct SkuStatus {
    // The time of the last SKU validation request or None.
    // used by the state machine to determing if a machine needs
    // to be validated against its assigned SKU
    pub verify_request_time: Option<DateTime<Utc>>,
    // Periodically the state machine will attempt to find a match
    // for this machine.  This is the last time an attempt was made.
    // None means no attempt has been made.  This value is not valid
    // if the machine has a SKU assigned.
    pub last_match_attempt: Option<DateTime<Utc>>,
    // If the a SKU is assinged in expected machines but is missing,
    // the state machine will attempt to create it from generated
    // machine data.  This marks the last time an attempt was made.
    // None means no attempt has been made.  This value is not valid
    // if the assigned SKU exists or the assigned SKU is not from the
    // expected machine.
    pub last_generate_attempt: Option<DateTime<Utc>>,
}

/// First SKU schema version that validates storage by drive size range and
/// PCI location instead of by model. Versions below this keep the legacy
/// model + count storage comparison.
pub const SKU_VERSION_WITH_DRIVE_LOCATION: u32 = 5;

/// diff an actual sku against an expected sku and return the differences.
///
/// Note that the version check is done on the expected_sku so order of arguements is important.
/// SKUs with different versions may match one way, but not the other.
pub fn diff_skus(actual_sku: &Sku, expected_sku: &Sku) -> Vec<String> {
    let mut diffs = Vec::default();

    if actual_sku.components.chassis.model != expected_sku.components.chassis.model {
        diffs.push(format!(
            r#"Actual chassis model "{}" does not match expected "{}""#,
            actual_sku.components.chassis.model, expected_sku.components.chassis.model
        ));
    }
    if actual_sku.components.chassis.architecture != expected_sku.components.chassis.architecture {
        diffs.push(format!(
            r#"Actual chassis architecture "{}" does not match expected "{}""#,
            actual_sku.components.chassis.architecture,
            expected_sku.components.chassis.architecture
        ));
    }

    let expected_cpu_count = expected_sku
        .components
        .cpus
        .iter()
        .map(|c| c.count)
        .sum::<u32>();
    let actual_cpu_count = actual_sku
        .components
        .cpus
        .iter()
        .map(|c| c.count)
        .sum::<u32>();

    if expected_cpu_count != actual_cpu_count {
        diffs.push(format!(
            "Number of CPUs ({actual_cpu_count}) does not match expected ({expected_cpu_count})"
        ));
    }

    let expected_thread_count = expected_sku
        .components
        .cpus
        .iter()
        .map(|c| c.thread_count)
        .sum::<u32>();
    let actual_thread_count = actual_sku
        .components
        .cpus
        .iter()
        .map(|c| c.thread_count)
        .sum::<u32>();

    if expected_thread_count != actual_thread_count {
        diffs.push(format!(
            "Number of CPU threads ({actual_thread_count}) does not match expected ({expected_thread_count})"
        ));
    }

    // FORGE-6856: Disable checking of VRAM because the value can change if ECC mode is enabled on the GPU.
    let mut expected_gpus: HashMap<&str, &SkuComponentGpu> = expected_sku
        .components
        .gpus
        .iter()
        .map(|gpu| (gpu.model.as_str(), gpu))
        .collect();

    for actual_gpu in actual_sku.components.gpus.iter() {
        match expected_gpus.remove(&actual_gpu.model.as_str()) {
            None => diffs.push(format!("Unexpected GPU config ({actual_gpu}) found")),
            Some(expected_gpu) => {
                if actual_gpu.count != expected_gpu.count {
                    diffs.push(format!(
                        "Expected gpu count ({}) does not match actual ({}) for gpu model ({})",
                        expected_gpu.count, actual_gpu.count, expected_gpu.model
                    ));
                }
            }
        }
    }

    for missing_gpu in expected_gpus.values() {
        diffs.push(format!("Missing GPU config: {missing_gpu}"));
    }

    let mut expected_ib_device_by_name: HashMap<
        (&String, &String),
        &SkuComponentInfinibandDevices,
    > = HashMap::new();
    for ib_devices in expected_sku.components.infiniband_devices.iter() {
        expected_ib_device_by_name.insert((&ib_devices.vendor, &ib_devices.model), ib_devices);
    }

    for actual_ib_device_definition in actual_sku.components.infiniband_devices.iter() {
        match expected_ib_device_by_name.remove(&(
            &actual_ib_device_definition.vendor,
            &actual_ib_device_definition.model,
        )) {
            Some(expected) => {
                if expected != actual_ib_device_definition {
                    let mut msg = format!(
                        "Configuration mismatch for InfiniBand devices of Vendor: \"{}\" and Model: \"{}\". ",
                        expected.vendor, expected.model
                    );
                    write!(
                        &mut msg,
                        "Expected \"count: {}, inactive_devices: {:?}\". ",
                        expected.count, expected.inactive_devices
                    )
                    .unwrap();
                    write!(
                        &mut msg,
                        "Actual \"count: {}, inactive_devices: {:?}\". ",
                        actual_ib_device_definition.count,
                        actual_ib_device_definition.inactive_devices
                    )
                    .unwrap();
                    diffs.push(msg);
                }
            }
            None => {
                diffs.push(format!(
                    "Unexpected {} InfiniBand devices of Vendor: \"{}\" and Model: \"{}\"",
                    actual_ib_device_definition.count,
                    actual_ib_device_definition.vendor,
                    actual_ib_device_definition.model
                ));
            }
        }
    }
    for missing_ib_devices in expected_ib_device_by_name.values() {
        diffs.push(format!(
            "Missing {} InfiniBand devices of Vendor: \"{}\" and Model: \"{}\"",
            missing_ib_devices.count, missing_ib_devices.vendor, missing_ib_devices.model
        ));
    }

    let actual_total_memory = actual_sku
        .components
        .memory
        .iter()
        .fold(0, |a, m| a + (m.capacity_mb * m.count));
    let expected_total_memory = expected_sku
        .components
        .memory
        .iter()
        .fold(0, |a, m| a + (m.capacity_mb * m.count));

    if expected_total_memory != actual_total_memory {
        diffs.push(format!(
            "Actual memory ({expected_total_memory}) differs from expected ({actual_total_memory})"
        ));
    }

    if expected_sku.schema_version >= SKU_VERSION_WITH_DRIVE_LOCATION {
        diff_storage_by_location(actual_sku, expected_sku, &mut diffs);
    } else {
        diff_storage_by_model(actual_sku, expected_sku, &mut diffs);
    }

    // Vendor and Model fields do not contain useful information.  They seem limited and encoded somehow.
    // We really only care about the spec version supported and that a TPM exists.
    match (&actual_sku.components.tpm, &expected_sku.components.tpm) {
        (None, None) => {}
        (None, Some(expected_tpm)) => diffs.push(format!(
            "Missing a TPM module: version: {}",
            expected_tpm.version
        )),
        (Some(actual_tpm), None) => diffs.push(format!(
            "Found unexpected TPM config: version: {}",
            actual_tpm.version
        )),
        (Some(actual_tpm), Some(expected_tpm)) => {
            if actual_tpm.version != expected_tpm.version {
                diffs.push(format!(
                    "Expected TPM version ({}) does not match actual ({})",
                    expected_tpm.version, actual_tpm.version
                ));
            }
        }
    }
    diffs
}

/// Legacy (schema version < 5) storage comparison: match discovered storage to
/// expected storage by model and compare counts.
fn diff_storage_by_model(actual_sku: &Sku, expected_sku: &Sku, diffs: &mut Vec<String>) {
    // v5 actual SKUs have one entry per drive; aggregate by model so the count
    // comparison against a v4 expected SKU (grouped by model) is correct.
    let mut actual_storage: HashMap<String, SkuComponentStorage> = HashMap::new();
    for s in &actual_sku.components.storage {
        actual_storage
            .entry(s.model.clone())
            .and_modify(|e| e.count += s.count)
            .or_insert_with(|| s.clone());
    }

    for es in &expected_sku.components.storage {
        if let Some(actual_storage) = actual_storage.remove(&es.model) {
            if actual_storage.count != es.count {
                diffs.push(format!(
                    "Expected device count ({}) does not match actual ({}) for storage model ({})",
                    es.count, actual_storage.count, actual_storage.model,
                ));
            }
        } else {
            diffs.push(format!("Missing storage config: {es}"));
        };
    }
    for s in actual_storage.values() {
        diffs.push(format!("Found unexpected storage config: {s}"));
    }
}

/// A single discovered drive, projected out of an actual (generated) SKU where
/// each storage entry represents one drive: `pci_patterns[0]` is its concrete
/// path and `min_size_mb == max_size_mb` is its capacity.
struct DiscoveredDrive<'a> {
    path: Option<&'a str>,
    size_mb: Option<u32>,
}

impl DiscoveredDrive<'_> {
    fn describe(&self) -> String {
        format!(
            "path {}, size {}",
            self.path.unwrap_or("<unknown>"),
            self.size_mb
                .map(|v| format!("{v} MB"))
                .unwrap_or_else(|| "<unknown>".to_string()),
        )
    }

    fn size_in_range(&self, min: Option<u32>, max: Option<u32>) -> bool {
        // Fail safe: an unknown size can only satisfy a group that configures no
        // size bounds. Against an explicit min/max it is treated as a mismatch.
        let Some(size) = self.size_mb else {
            return min.is_none() && max.is_none();
        };
        min.is_none_or(|m| size >= m) && max.is_none_or(|m| size <= m)
    }
}

/// Attempt to place `drive` into an eligible, unfilled slot using Kuhn's
/// augmenting-path algorithm, displacing already-placed drives when doing so
/// frees a slot for them elsewhere. `slot_group[s]` is the expected-group index
/// that owns slot `s`; `eligibility[g]` lists the drive indices group `g` can
/// legally claim. Returns whether `drive` was placed.
fn augment_drive(
    drive: usize,
    slot_group: &[usize],
    eligibility: &[Vec<usize>],
    slot_to_drive: &mut [Option<usize>],
    visited: &mut [bool],
) -> bool {
    for s in 0..slot_group.len() {
        if visited[s] || !eligibility[slot_group[s]].contains(&drive) {
            continue;
        }
        visited[s] = true;
        let freed = match slot_to_drive[s] {
            None => true,
            Some(occupant) => {
                augment_drive(occupant, slot_group, eligibility, slot_to_drive, visited)
            }
        };
        if freed {
            slot_to_drive[s] = Some(drive);
            return true;
        }
    }
    false
}

/// Storage comparison for schema version >= 5. Model is intentionally ignored;
/// each expected storage group is validated by drive size range, count, and PCI
/// location patterns. Fails safely (emits a diff) when an expected drive is
/// missing, when a location is ambiguous, when a drive is the wrong size, or
/// when a discovered drive is not expected anywhere.
///
/// Drives are assigned to expected groups with a global maximum matching rather
/// than greedily, so a broad group (e.g. `/nvme/`) never claims a drive that
/// only a more specific group (e.g. `0000:04:00\.0`) can satisfy when a valid
/// overall assignment exists.
fn diff_storage_by_location(actual_sku: &Sku, expected_sku: &Sku, diffs: &mut Vec<String>) {
    let drives: Vec<DiscoveredDrive> = actual_sku
        .components
        .storage
        .iter()
        .map(|s| DiscoveredDrive {
            path: s.pci_patterns.first().map(|p| p.as_str()),
            size_mb: s.min_size_mb,
        })
        .collect();

    // Order location-constrained groups before generic (size-only) ones. The
    // assignment below is global, so this ordering does not affect whether a
    // valid assignment is found; it only biases which of several equivalent
    // assignments is chosen so a drive prefers the group naming its PCI
    // location, keeping diagnostics deterministic.
    let (constrained, generic): (Vec<_>, Vec<_>) = expected_sku
        .components
        .storage
        .iter()
        .partition(|es| !es.pci_patterns.is_empty());
    let groups: Vec<&SkuComponentStorage> = constrained.into_iter().chain(generic).collect();

    // Compute, per expected group, the drives it may legally claim. A generic
    // group (no patterns) accepts any drive within its size range. A
    // location-constrained group accepts any drive matching at least one of its
    // patterns; size is validated after assignment (as a diagnostic) so a drive
    // at the right location but wrong size is reported as out-of-range rather
    // than silently left unclaimed. Invalid patterns are reported and match
    // nothing, and a pattern that matches no drive at all is reported here since
    // it is unsatisfiable regardless of the final assignment.
    let mut eligibility: Vec<Vec<usize>> = Vec::with_capacity(groups.len());
    for es in &groups {
        let mut eligible = Vec::new();
        if es.pci_patterns.is_empty() {
            for (i, drive) in drives.iter().enumerate() {
                if drive.size_in_range(es.min_size_mb, es.max_size_mb) {
                    eligible.push(i);
                }
            }
        } else {
            let compiled: Vec<(&String, Option<Regex>)> = es
                .pci_patterns
                .iter()
                .map(|pattern| match Regex::new(pattern) {
                    Ok(re) => (pattern, Some(re)),
                    Err(err) => {
                        diffs.push(format!("Invalid storage PCI pattern \"{pattern}\": {err}"));
                        (pattern, None)
                    }
                })
                .collect();

            for (i, drive) in drives.iter().enumerate() {
                let matches = drive.path.is_some_and(|p| {
                    compiled
                        .iter()
                        .any(|(_, re)| re.as_ref().is_some_and(|re| re.is_match(p)))
                });
                if matches {
                    eligible.push(i);
                }
            }

            for (pattern, re) in &compiled {
                if let Some(re) = re {
                    let found = drives
                        .iter()
                        .any(|d| d.path.is_some_and(|p| re.is_match(p)));
                    if !found {
                        diffs.push(format!(
                            "Expected storage drive at PCI location /{pattern}/ not found"
                        ));
                    }
                }
            }
        }
        eligibility.push(eligible);
    }

    // Expand each group into `count` interchangeable slots, then find a global
    // maximum matching of drives to slots. Maximising the number of matched
    // drives yields a saturating assignment whenever one exists.
    let mut slot_group: Vec<usize> = Vec::new();
    for (g, es) in groups.iter().enumerate() {
        for _ in 0..es.count {
            slot_group.push(g);
        }
    }
    let mut slot_to_drive: Vec<Option<usize>> = vec![None; slot_group.len()];
    for drive in 0..drives.len() {
        let mut visited = vec![false; slot_group.len()];
        augment_drive(
            drive,
            &slot_group,
            &eligibility,
            &mut slot_to_drive,
            &mut visited,
        );
    }

    let mut drive_to_group: Vec<Option<usize>> = vec![None; drives.len()];
    let mut assigned_per_group = vec![0u32; groups.len()];
    for (s, occupant) in slot_to_drive.iter().enumerate() {
        if let Some(drive) = occupant {
            drive_to_group[*drive] = Some(slot_group[s]);
            assigned_per_group[slot_group[s]] += 1;
        }
    }

    // Report groups that could not be fully satisfied.
    for (g, es) in groups.iter().enumerate() {
        if assigned_per_group[g] == es.count {
            continue;
        }
        if es.pci_patterns.is_empty() {
            diffs.push(format!(
                "Expected {} storage drive(s) ({es}) but found {} matching the size range",
                es.count, assigned_per_group[g],
            ));
        } else if !eligibility[g].is_empty() {
            // Only report the count shortfall when at least one drive matched a
            // pattern. When none did, every pattern already emitted its own
            // "Expected storage drive at PCI location /.../ not found" diff
            // above, so a group-level "matched 0" diff would double-report the
            // same root cause.
            diffs.push(format!(
                "Expected {} storage drive(s) ({es}) but matched {} at those PCI locations",
                es.count, assigned_per_group[g],
            ));
        }
    }

    // Report size mismatches for drives claimed by a location-constrained group.
    for (i, drive) in drives.iter().enumerate() {
        let Some(g) = drive_to_group[i] else { continue };
        let es = groups[g];
        if !es.pci_patterns.is_empty() && !drive.size_in_range(es.min_size_mb, es.max_size_mb) {
            diffs.push(format!(
                "Storage drive ({}) is outside expected size range {}-{} MB",
                drive.describe(),
                es.min_size_mb
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "*".to_string()),
                es.max_size_mb
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "*".to_string()),
            ));
        }
    }

    // Any drive not claimed by a group is unexpected.
    for (i, drive) in drives.iter().enumerate() {
        if drive_to_group[i].is_none() {
            diffs.push(format!(
                "Found unexpected storage drive ({})",
                drive.describe()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sku(schema_version: u32, storage: Vec<SkuComponentStorage>) -> Sku {
        Sku {
            schema_version,
            id: "test".to_string(),
            description: String::new(),
            created: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            components: SkuComponents {
                chassis: SkuComponentChassis::default(),
                cpus: Vec::new(),
                gpus: Vec::new(),
                memory: Vec::new(),
                infiniband_devices: Vec::new(),
                storage,
                tpm: None,
            },
            device_type: None,
        }
    }

    /// A discovered drive as an actual (generated) v5 storage entry.
    fn drive(path: &str, size_mb: u32) -> SkuComponentStorage {
        SkuComponentStorage {
            model: "nvme".to_string(),
            count: 1,
            min_size_mb: Some(size_mb),
            max_size_mb: Some(size_mb),
            pci_patterns: vec![path.to_string()],
        }
    }

    /// An expected v5 storage group.
    fn expected(
        count: u32,
        min: Option<u32>,
        max: Option<u32>,
        patterns: &[&str],
    ) -> SkuComponentStorage {
        SkuComponentStorage {
            model: String::new(),
            count,
            min_size_mb: min,
            max_size_mb: max,
            pci_patterns: patterns.iter().map(|s| s.to_string()).collect(),
        }
    }

    const PATH_A: &str = "/devices/pci0000:00/0000:04:00.0/nvme/nvme0/nvme0n1";
    const PATH_B: &str = "/devices/pci0000:00/0000:05:00.0/nvme/nvme1/nvme1n1";

    fn v5_storage_diffs(
        actual: Vec<SkuComponentStorage>,
        expected: Vec<SkuComponentStorage>,
    ) -> Vec<String> {
        diff_skus(&sku(5, actual), &sku(5, expected))
    }

    #[test]
    fn v5_exact_location_and_size_match() {
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![
                expected(1, Some(3_800_000), Some(4_000_000), &[r"0000:04:00\.0"]),
                expected(1, Some(3_800_000), Some(4_000_000), &[r"0000:05:00\.0"]),
            ],
        );
        assert!(diffs.is_empty(), "expected no diffs, got {diffs:?}");
    }

    #[test]
    fn v5_missing_expected_location() {
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000)],
            vec![expected(1, None, None, &[r"0000:09:00\.0"])],
        );
        assert!(
            diffs.iter().any(|d| d.contains("not found")),
            "got {diffs:?}"
        );
        // The unmatched discovered drive is reported as unexpected.
        assert!(
            diffs.iter().any(|d| d.contains("unexpected storage drive")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_missing_location_reported_once() {
        // A constrained group whose only pattern matches no drive reports the
        // missing location once (the per-pattern "not found"), not also a
        // redundant group-level "at those PCI locations" count diff.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000)],
            vec![expected(1, None, None, &[r"0000:09:00\.0"])],
        );
        assert!(
            diffs.iter().any(|d| d.contains("not found")),
            "got {diffs:?}"
        );
        assert!(
            !diffs.iter().any(|d| d.contains("at those PCI locations")),
            "missing location should not be double-reported, got {diffs:?}"
        );
    }

    #[test]
    fn v5_partial_location_match_reports_count() {
        // When at least one pattern matches a drive but the group is still
        // short, the group-level count diff is not redundant with a per-pattern
        // "not found", so both are reported.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000)],
            vec![expected(
                2,
                None,
                None,
                &[r"0000:04:00\.0", r"0000:09:00\.0"],
            )],
        );
        assert!(
            diffs.iter().any(|d| d.contains("at those PCI locations")),
            "expected count-shortfall diff, got {diffs:?}"
        );
        assert!(
            diffs.iter().any(|d| d.contains("not found")),
            "expected missing-location diff, got {diffs:?}"
        );
    }

    #[test]
    fn v5_unexpected_extra_drive() {
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![expected(1, None, None, &[r"0000:04:00\.0"])],
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.contains("unexpected storage drive") && d.contains("0000:05:00.0")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_drive_size_out_of_range() {
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 1_920_000)],
            vec![expected(
                1,
                Some(3_800_000),
                Some(4_000_000),
                &[r"0000:04:00\.0"],
            )],
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.contains("outside expected size range")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_broad_pattern_over_matches_reported_as_unexpected() {
        // One pattern matches both drives but only one is expected. Global
        // assignment claims a single drive for the group and reports the
        // leftover as unexpected rather than inflating the group's match count.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![expected(1, None, None, &[r"nvme"])],
        );
        assert!(
            diffs.iter().any(|d| d.contains("unexpected storage drive")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_broad_constrained_group_does_not_starve_specific_group() {
        // A broad constrained group (/nvme/) and a specific one (0000:04:00.0)
        // each expect one of two drives. A valid assignment exists
        // (/nvme/ -> PATH_B, 0000:04:00.0 -> PATH_A), so global matching must
        // find it instead of letting the broad group claim both drives.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![
                expected(1, None, None, &[r"/nvme/"]),
                expected(1, None, None, &[r"0000:04:00\.0"]),
            ],
        );
        assert!(diffs.is_empty(), "expected no diffs, got {diffs:?}");
    }

    #[test]
    fn v5_broad_pattern_count_match() {
        // One pattern matching all drives, count set to match, is accepted.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![expected(2, None, None, &[r"/nvme/"])],
        );
        assert!(diffs.is_empty(), "expected no diffs, got {diffs:?}");
    }

    #[test]
    fn v5_invalid_pattern_fails_soft() {
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000)],
            vec![expected(1, None, None, &["0000:04:00(.0"])],
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.contains("Invalid storage PCI pattern")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_size_only_no_location() {
        // Empty patterns: validate by size range and count only.
        let ok = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![expected(2, Some(3_800_000), Some(4_000_000), &[])],
        );
        assert!(ok.is_empty(), "expected no diffs, got {ok:?}");

        let bad = v5_storage_diffs(
            vec![drive(PATH_A, 1_920_000)],
            vec![expected(1, Some(3_800_000), Some(4_000_000), &[])],
        );
        assert!(
            bad.iter().any(|d| d.contains("matching the size range")),
            "got {bad:?}"
        );
    }

    #[test]
    fn v5_generic_group_does_not_starve_constrained_group() {
        // A generic (size-only) group is listed before a location-constrained
        // group and both match PATH_A by size. If groups were processed in list
        // order the generic group would greedily claim PATH_A, leaving the
        // constrained group unable to match its required location. Constrained
        // groups must be resolved first so a valid overall assignment is found.
        let diffs = v5_storage_diffs(
            vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)],
            vec![
                expected(1, Some(3_800_000), Some(4_000_000), &[]),
                expected(1, Some(3_800_000), Some(4_000_000), &[r"0000:04:00\.0"]),
            ],
        );
        assert!(diffs.is_empty(), "expected no diffs, got {diffs:?}");
    }

    #[test]
    fn v5_unknown_size_fails_explicit_bounds() {
        // A discovered drive whose capacity could not be read (min_size_mb None)
        // must not silently satisfy a group that configures a size range.
        let mut unknown = drive(PATH_A, 0);
        unknown.min_size_mb = None;
        unknown.max_size_mb = None;

        let diffs = v5_storage_diffs(
            vec![unknown.clone()],
            vec![expected(
                1,
                Some(3_800_000),
                Some(4_000_000),
                &[r"0000:04:00\.0"],
            )],
        );
        assert!(
            diffs
                .iter()
                .any(|d| d.contains("outside expected size range")),
            "got {diffs:?}"
        );

        // With no bounds configured, unknown size is accepted.
        let ok = v5_storage_diffs(
            vec![unknown],
            vec![expected(1, None, None, &[r"0000:04:00\.0"])],
        );
        assert!(ok.is_empty(), "expected no diffs, got {ok:?}");
    }

    #[test]
    fn legacy_v4_still_matches_by_model() {
        // Under v4 the model is compared and size/pci fields are ignored.
        let actual = vec![SkuComponentStorage {
            model: "MODEL_X".to_string(),
            count: 4,
            min_size_mb: None,
            max_size_mb: None,
            pci_patterns: Vec::new(),
        }];
        let expected_match = actual.clone();
        assert!(diff_skus(&sku(4, actual.clone()), &sku(4, expected_match)).is_empty());

        let expected_wrong = vec![SkuComponentStorage {
            model: "MODEL_Y".to_string(),
            count: 4,
            min_size_mb: None,
            max_size_mb: None,
            pci_patterns: Vec::new(),
        }];
        let diffs = diff_skus(&sku(4, actual), &sku(4, expected_wrong));
        assert!(
            diffs.iter().any(|d| d.contains("Missing storage config")),
            "got {diffs:?}"
        );
    }

    #[test]
    fn v5_actual_matches_v4_expected_by_model() {
        // A v5 actual SKU (one entry per drive, count=1 each) must match a v4
        // expected SKU (grouped by model) correctly. The regression: HashMap
        // collection clobbered duplicate models, leaving count=1 instead of 2.
        let actual_v5 = vec![drive(PATH_A, 3_840_000), drive(PATH_B, 3_840_000)];
        let expected_v4 = vec![SkuComponentStorage {
            model: "nvme".to_string(),
            count: 2,
            min_size_mb: None,
            max_size_mb: None,
            pci_patterns: Vec::new(),
        }];
        let diffs = diff_skus(&sku(5, actual_v5), &sku(4, expected_v4));
        assert!(diffs.is_empty(), "expected no diffs, got {diffs:?}");
    }
}
