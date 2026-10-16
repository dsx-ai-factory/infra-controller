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

use ::rpc::protos::mlx_device::{LockStatus as LockStatusPb, StatusReport as StatusReportPb};
use chrono;
use serde::{Deserialize, Serialize};

use crate::lockdown::error::{MlxError, MlxResult};
use crate::lockdown::runner::FlintRunner;

// LockStatus represents the current lock status of a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockStatus {
    Locked,
    Unlocked,
    Unknown,
}

impl std::fmt::Display for LockStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockStatus::Locked => write!(f, "locked"),
            LockStatus::Unlocked => write!(f, "unlocked"),
            LockStatus::Unknown => write!(f, "unknown"),
        }
    }
}

// LockdownManager is the main interface for managing device
// lockdown operations.
//
// Just to have this documented somewhere, it's important to call out
// a few notes around the behavior(s) with locking and unlocking cards.
//
// Behind the scenes, there's:
// - hw_access disable <device> <key>, which locks a card with a given key.
// - set_key disable <device> <key>, which does the exact same thing.
// - hw_access enable <device> <key>, which unlocks the card with the key.
//
// Originally I thought you had to set_key, and then you could lock at
// will, and had to unlock with the key. But it turns out that set_key
// and hw_access disable both appear to do the same thing now. If you try
// to call hw_access disable after set_key, it just tells you access
// is already disabled.
//
// I also thought you had to reboot/power cycle the card after changing
// the key, but based on testing, that also seems to not be the case
// either. Once you hw_access enable, it clears the key, and you need
// to either hw_access disable or set_key again (and can do it with a
// different key). This is actually nice, because I don't need to power
// cycle in between tenants (granted we will anyway, but it's one less
// power cycle to worry about).
//
// But fwiw, that behavior may also be card specific. I'm testing on
// a BF3 SuperNIC, and it's letting me do things as I described above.
pub struct LockdownManager {
    // runner is the flint command runner.
    runner: FlintRunner,
}

impl LockdownManager {
    // new creates a new LockdownManager instance.
    pub fn new() -> MlxResult<Self> {
        let runner = FlintRunner::new()?;
        Ok(Self { runner })
    }

    // with_dry_run creates a new LockdownManager with dry-run support.
    pub fn with_dry_run(dry_run: bool) -> MlxResult<Self> {
        let runner = if dry_run {
            // For dry-run, just explicitly set a path to skip the
            // discovery of the flint binary. The problem is on the
            // build machine, it doesn't have flint installed (which
            // is expected), so the CLI parsing tests fail, since it
            // tries to discover the location of the flint binary.
            // And I mean, tbh, it's not really needed anyway, but I
            // like having this subcommand stuff I can import, so I
            // kind of want to keep it maintained and tested.
            FlintRunner::with_path("flint").with_dry_run(true)
        } else {
            FlintRunner::new()?
        };
        Ok(Self { runner })
    }

    // with_runner creates a new LockdownManager with a custom runner.
    pub fn with_runner(runner: FlintRunner) -> Self {
        Self { runner }
    }

    /// lock_device locks hardware access on the specified device with the provided key.
    ///
    /// Key validation, dry-run behavior, and command errors follow
    /// [`FlintRunner::disable_hw_access`]. Returns [`LockStatus::Locked`] on success or
    /// [`MlxError::AlreadyLocked`] when flint reports access already disabled.
    /// Returns [`MlxError::InvalidDeviceId`] if the device identifier is empty or
    /// contains an ASCII space.
    pub fn lock_device(&self, device_id: &str, key: &str) -> MlxResult<LockStatus> {
        FlintRunner::validate_device_id(device_id)?;

        // This will now return an error if already locked instead of silently succeeding
        self.runner.disable_hw_access(device_id, key)?;
        Ok(LockStatus::Locked)
    }

    /// unlock_device unlocks hardware access on the specified device with the provided key.
    ///
    /// Key validation, dry-run behavior, and command errors follow
    /// [`FlintRunner::enable_hw_access`]. Returns [`LockStatus::Unlocked`] on success or
    /// [`MlxError::AlreadyUnlocked`] when flint reports access already enabled.
    /// Returns [`MlxError::InvalidDeviceId`] if the device identifier is empty or
    /// contains an ASCII space.
    pub fn unlock_device(&self, device_id: &str, key: &str) -> MlxResult<LockStatus> {
        FlintRunner::validate_device_id(device_id)?;

        // This will now return an error if already unlocked instead of silently succeeding
        self.runner.enable_hw_access(device_id, key)?;
        Ok(LockStatus::Unlocked)
    }

    // get_status gets the current lock status of the specified device.
    pub fn get_status(&self, device_id: &str) -> MlxResult<LockStatus> {
        FlintRunner::validate_device_id(device_id)?;

        match self.runner.query_device(device_id) {
            Ok(status_str) => match status_str.as_str() {
                "locked" => Ok(LockStatus::Locked),
                "unlocked" => Ok(LockStatus::Unlocked),
                _ => Ok(LockStatus::Unknown),
            },
            Err(e) => {
                // If we can't query, it might be locked
                match e {
                    MlxError::CommandFailed(ref msg) if msg.contains("HW access is disabled") => {
                        Ok(LockStatus::Locked)
                    }
                    _ => Err(e),
                }
            }
        }
    }

    /// set_device_key sets a new hardware access key for the device.
    ///
    /// Key validation, dry-run behavior, and command errors follow [`FlintRunner::set_key`].
    /// Returns [`MlxError::InvalidDeviceId`] if the device identifier is empty or
    /// contains an ASCII space. This method does not return [`MlxError::AlreadyLocked`].
    pub fn set_device_key(&self, device_id: &str, key: &str) -> MlxResult<()> {
        FlintRunner::validate_device_id(device_id)?;
        self.runner.set_key(device_id, key)
    }
}

impl Default for LockdownManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self::with_runner(FlintRunner::default()))
    }
}

// StatusReport is a structured status report for serialization.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusReport {
    // device_id is the device identifier.
    pub device_id: String,
    // status is the current lock status.
    pub status: LockStatus,
    // timestamp is when the status was checked.
    pub timestamp: String,
}

impl StatusReport {
    // new creates a new status report.
    pub fn new(device_id: String, status: LockStatus) -> Self {
        Self {
            device_id,
            status,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    // to_json serializes the status report to JSON.
    pub fn to_json(&self) -> MlxResult<String> {
        serde_json::to_string_pretty(self).map_err(|e| e.into())
    }

    // to_yaml serializes the status report to YAML.
    pub fn to_yaml(&self) -> MlxResult<String> {
        serde_yaml::to_string(self).map_err(|e| MlxError::ParseError(e.to_string()))
    }
}

impl From<LockStatus> for LockStatusPb {
    fn from(status: LockStatus) -> Self {
        match status {
            LockStatus::Locked => LockStatusPb::Locked,
            LockStatus::Unlocked => LockStatusPb::Unlocked,
            LockStatus::Unknown => LockStatusPb::Unknown,
        }
    }
}

impl From<LockStatusPb> for LockStatus {
    fn from(pb: LockStatusPb) -> Self {
        match pb {
            LockStatusPb::Locked => LockStatus::Locked,
            LockStatusPb::Unlocked => LockStatus::Unlocked,
            LockStatusPb::Unknown => LockStatus::Unknown,
        }
    }
}

impl From<StatusReport> for StatusReportPb {
    fn from(report: StatusReport) -> Self {
        StatusReportPb {
            device_id: report.device_id,
            status: LockStatusPb::from(report.status) as i32,
            timestamp: report.timestamp,
        }
    }
}

impl From<StatusReportPb> for StatusReport {
    fn from(pb: StatusReportPb) -> Self {
        let status = LockStatusPb::try_from(pb.status)
            .map(LockStatus::from)
            .unwrap_or(LockStatus::Unknown);

        StatusReport {
            device_id: pb.device_id,
            status,
            timestamp: pb.timestamp,
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    // Every LockStatus serde round-trips through its lowercase rename: serialize to
    // JSON, deserialize back, and you land on the same variant. This pins all three
    // arms of the `rename_all = "lowercase"` encoding via a stable equality.
    #[test]
    fn lock_status_round_trips_through_json() {
        scenarios!(
            run = |status: LockStatus| {
                let json = serde_json::to_string(&status).map_err(drop)?;
                serde_json::from_str(&json).map_err(drop)
            };
            "locked" {
                LockStatus::Locked => Yields(LockStatus::Locked),
            }

            "unlocked" {
                LockStatus::Unlocked => Yields(LockStatus::Unlocked),
            }

            "unknown" {
                LockStatus::Unknown => Yields(LockStatus::Unknown),
            }
        );
    }

    // The exact lowercase JSON token each variant serializes to -- this IS the
    // wire contract (the `rename_all = "lowercase"`), so plain string equality is
    // the right assertion.
    #[test]
    fn lock_status_serializes_to_lowercase_token() {
        scenarios!(
            run = |status: LockStatus| serde_json::to_string(&status).map_err(drop);
            "locked" {
                LockStatus::Locked => Yields("\"locked\"".to_string()),
            }

            "unlocked" {
                LockStatus::Unlocked => Yields("\"unlocked\"".to_string()),
            }

            "unknown" {
                LockStatus::Unknown => Yields("\"unknown\"".to_string()),
            }
        );
    }

    // Deserializing the lowercase tokens lands on the matching variant; an unknown
    // token fails. Covers the deserialize direction for all three arms plus the
    // rejection path.
    #[test]
    fn lock_status_deserializes_each_token() {
        scenarios!(
            run = |raw: &str| serde_json::from_str::<LockStatus>(raw).map_err(drop);
            "locked" {
                "\"locked\"" => Yields(LockStatus::Locked),
            }

            "unlocked" {
                "\"unlocked\"" => Yields(LockStatus::Unlocked),
            }

            "unknown" {
                "\"unknown\"" => Yields(LockStatus::Unknown),
            }

            "an unrecognized token is rejected" {
                "\"bogus\"" => Fails,
            }
        );
    }

    // LockStatus -> LockStatusPb covers every match arm of the protobuf conversion.
    // Comparing the resulting pb's i32 discriminant pins the mapping to the proto
    // numbers (UNKNOWN=0, LOCKED=1, UNLOCKED=2) without relying on Pb being PartialEq.
    #[test]
    fn lock_status_into_pb_discriminant() {
        value_scenarios!(
            run = |status: LockStatus| LockStatusPb::from(status) as i32;
            "locked -> 1" {
                LockStatus::Locked => 1,
            }

            "unlocked -> 2" {
                LockStatus::Unlocked => 2,
            }

            "unknown -> 0" {
                LockStatus::Unknown => 0,
            }
        );
    }

    // LockStatusPb -> LockStatus covers every match arm of the reverse conversion.
    #[test]
    fn lock_status_from_pb() {
        value_scenarios!(
            run = LockStatus::from;
            "Locked" {
                LockStatusPb::Locked => LockStatus::Locked,
            }

            "Unlocked" {
                LockStatusPb::Unlocked => LockStatus::Unlocked,
            }

            "Unknown" {
                LockStatusPb::Unknown => LockStatus::Unknown,
            }
        );
    }

    // LockStatus survives a full pb round-trip (LockStatus -> pb -> LockStatus) for
    // every variant. This exercises both From impls together and pins them as
    // inverses without depending on the pb type being PartialEq.
    #[test]
    fn lock_status_round_trips_through_pb() {
        value_scenarios!(
            run = |status: LockStatus| LockStatus::from(LockStatusPb::from(status));
            "locked" {
                LockStatus::Locked => LockStatus::Locked,
            }

            "unlocked" {
                LockStatus::Unlocked => LockStatus::Unlocked,
            }

            "unknown" {
                LockStatus::Unknown => LockStatus::Unknown,
            }
        );
    }

    // StatusReport -> StatusReportPb copies device_id and timestamp through verbatim
    // and encodes status as its i32 discriminant. We project the pb back to a tuple
    // of the fields we are sure about, keyed by status variant.
    #[test]
    fn status_report_into_pb_preserves_fields() {
        value_scenarios!(
            run = |status: LockStatus| {
                let report = StatusReport {
                    device_id: "dev-a".to_string(),
                    status,
                    timestamp: "2026-01-01T00:00:00+00:00".to_string(),
                };
                let pb = StatusReportPb::from(report);
                (pb.device_id, pb.status, pb.timestamp)
            };
            "locked" {
                LockStatus::Locked => (
                    "dev-a".to_string(),
                    1,
                    "2026-01-01T00:00:00+00:00".to_string(),
                ),
            }

            "unlocked" {
                LockStatus::Unlocked => (
                    "dev-a".to_string(),
                    2,
                    "2026-01-01T00:00:00+00:00".to_string(),
                ),
            }

            "unknown" {
                LockStatus::Unknown => (
                    "dev-a".to_string(),
                    0,
                    "2026-01-01T00:00:00+00:00".to_string(),
                ),
            }
        );
    }

    // StatusReportPb -> StatusReport maps a valid status discriminant back to the
    // matching variant, and -- the key uncovered branch -- coerces any out-of-range
    // discriminant to Unknown via the `unwrap_or(LockStatus::Unknown)`. device_id and
    // timestamp pass through unchanged; we project (device_id, status, timestamp).
    #[test]
    fn status_report_from_pb_maps_status_and_coerces_invalid() {
        value_scenarios!(
            run = |status_i32: i32| {
                let pb = StatusReportPb {
                    device_id: "dev-b".to_string(),
                    status: status_i32,
                    timestamp: "ts".to_string(),
                };
                let report = StatusReport::from(pb);
                (report.device_id, report.status, report.timestamp)
            };
            "0 -> Unknown" {
                0 => ("dev-b".to_string(), LockStatus::Unknown, "ts".to_string()),
            }

            "1 -> Locked" {
                1 => ("dev-b".to_string(), LockStatus::Locked, "ts".to_string()),
            }

            "2 -> Unlocked" {
                2 => ("dev-b".to_string(), LockStatus::Unlocked, "ts".to_string()),
            }

            "out-of-range discriminant coerces to Unknown" {
                99 => ("dev-b".to_string(), LockStatus::Unknown, "ts".to_string()),
            }

            "negative discriminant coerces to Unknown" {
                -1 => ("dev-b".to_string(), LockStatus::Unknown, "ts".to_string()),
            }
        );
    }

    // A StatusReport survives a full pb round-trip for every status variant, with
    // device_id and timestamp intact. Exercises both StatusReport <-> StatusReportPb
    // From impls as inverses.
    #[test]
    fn status_report_round_trips_through_pb() {
        value_scenarios!(
            run = |status: LockStatus| {
                let report = StatusReport {
                    device_id: "d".to_string(),
                    status,
                    timestamp: "t".to_string(),
                };
                let back = StatusReport::from(StatusReportPb::from(report));
                (back.device_id, back.status, back.timestamp)
            };
            "locked" {
                LockStatus::Locked => ("d".to_string(), LockStatus::Locked, "t".to_string()),
            }

            "unlocked" {
                LockStatus::Unlocked => ("d".to_string(), LockStatus::Unlocked, "t".to_string()),
            }

            "unknown" {
                LockStatus::Unknown => ("d".to_string(), LockStatus::Unknown, "t".to_string()),
            }
        );
    }

    // StatusReport::new derives a fresh RFC3339 timestamp and stores the given
    // device_id / status verbatim. We can't pin the exact timestamp, so we assert
    // the parts we control and that the timestamp is non-empty + RFC3339-parseable.
    #[test]
    fn status_report_new_sets_fields_and_timestamp() {
        let report = StatusReport::new("dev-new".to_string(), LockStatus::Locked);
        assert_eq!(report.device_id, "dev-new");
        assert_eq!(report.status, LockStatus::Locked);
        assert!(!report.timestamp.is_empty());
        assert!(chrono::DateTime::parse_from_rfc3339(&report.timestamp).is_ok());
    }

    // to_json round-trips: the pretty JSON it produces deserializes back into an
    // equivalent report. Pins device_id/status (StatusReport is not PartialEq, so we
    // project) and proves the JSON is well-formed for every status variant.
    #[test]
    fn status_report_to_json_round_trips() {
        scenarios!(
            run = |status: LockStatus| {
                let report = StatusReport::new("dev-j".to_string(), status);
                let json = report.to_json().map_err(drop)?;
                let back: StatusReport = serde_json::from_str(&json).map_err(drop)?;
                Ok::<_, ()>((back.device_id, back.status))
            };
            "locked" {
                LockStatus::Locked => Yields(("dev-j".to_string(), LockStatus::Locked)),
            }

            "unlocked" {
                LockStatus::Unlocked => Yields(("dev-j".to_string(), LockStatus::Unlocked)),
            }

            "unknown" {
                LockStatus::Unknown => Yields(("dev-j".to_string(), LockStatus::Unknown)),
            }
        );
    }

    // to_yaml round-trips through serde_yaml for every status variant. Projects
    // device_id/status since StatusReport isn't PartialEq. A YAML parse error would
    // surface as the MlxError::ParseError wrapping path, but the happy round-trip is
    // the assertion here.
    #[test]
    fn status_report_to_yaml_round_trips() {
        check_cases(
            [
                Case {
                    scenario: "locked",
                    input: LockStatus::Locked,
                    expect: Yields(("dev-y".to_string(), LockStatus::Locked)),
                },
                Case {
                    scenario: "unlocked",
                    input: LockStatus::Unlocked,
                    expect: Yields(("dev-y".to_string(), LockStatus::Unlocked)),
                },
                Case {
                    scenario: "unknown",
                    input: LockStatus::Unknown,
                    expect: Yields(("dev-y".to_string(), LockStatus::Unknown)),
                },
            ],
            |status: LockStatus| -> Result<(String, LockStatus), ()> {
                let report = StatusReport::new("dev-y".to_string(), status);
                let yaml = report.to_yaml().map_err(drop)?;
                let back: StatusReport = serde_yaml::from_str(&yaml).map_err(drop)?;
                Ok((back.device_id, back.status))
            },
        );
    }

    // The manager validates the device id before touching the runner: an empty id
    // and an id containing a space are rejected (InvalidDeviceId), while a
    // well-formed id passes validation and only then hits the runner. With a dry-run
    // runner a valid id reaches DryRun; the invalid ids never get that far. We map to
    // a stable token naming the error variant (MlxError isn't PartialEq).
    #[test]
    fn manager_rejects_invalid_device_ids_before_running() {
        fn kind<T>(result: MlxResult<T>) -> &'static str {
            match result {
                Ok(_) => "ok",
                Err(MlxError::InvalidDeviceId(_)) => "InvalidDeviceId",
                Err(MlxError::DryRun(_)) => "DryRun",
                Err(_) => "other",
            }
        }

        let runner = FlintRunner::with_path("flint").with_dry_run(true);
        let manager = LockdownManager::with_runner(runner);

        // lock_device validates first, then would dry-run.
        value_scenarios!(
            run = |device_id: &str| kind(manager.lock_device(device_id, "12345678"));
            "empty id is rejected before the runner" {
                "" => "InvalidDeviceId",
            }

            "id with a space is rejected before the runner" {
                "dev 0" => "InvalidDeviceId",
            }

            "a valid id passes validation and reaches the dry-run" {
                "0000:01:00.0" => "DryRun",
            }
        );
    }

    // get_status applies the same up-front device-id validation as the other manager
    // methods: empty and space-bearing ids are rejected as InvalidDeviceId before the
    // runner is queried, while a valid id reaches the dry-run runner (DryRun).
    #[test]
    fn get_status_validates_device_id() {
        fn kind<T>(result: MlxResult<T>) -> &'static str {
            match result {
                Ok(_) => "ok",
                Err(MlxError::InvalidDeviceId(_)) => "InvalidDeviceId",
                Err(MlxError::DryRun(_)) => "DryRun",
                Err(_) => "other",
            }
        }

        let runner = FlintRunner::with_path("flint").with_dry_run(true);
        let manager = LockdownManager::with_runner(runner);

        value_scenarios!(
            run = |device_id: &str| kind(manager.get_status(device_id));
            "empty id" {
                "" => "InvalidDeviceId",
            }

            "id with a space" {
                "bad id" => "InvalidDeviceId",
            }

            "valid id reaches the dry-run query" {
                "mlx5_0" => "DryRun",
            }
        );
    }
}
