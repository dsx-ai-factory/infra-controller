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
//! Durable, opt-in virtual-media boot requests owned by Core.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The lifecycle operation for which a host is being booted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBootPurpose {
    /// Enter Scout for initial host discovery.
    Discovery,
    /// Enter the tenant's selected operating-system installer.
    Installation,
    /// Enter Scout to clean a released host.
    Cleanup,
    /// Remove Core-owned media and hand booting to the local disk.
    LocalDisk,
}

/// Persisted boundary of a virtual-media boot attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBootPhase {
    /// No external write has been issued for this attempt.
    Pending,
    /// Inspect capabilities and preserve the original boot state.
    Inspecting,
    /// Media insertion may have been dispatched; read back before retrying.
    Mounting,
    /// The requested media has been observed in the owned slot.
    Mounted,
    /// Boot-override write may have been dispatched; read back before retrying.
    SettingBoot,
    /// Media and one-time boot override have both been read back.
    Armed,
    /// Reset may have been dispatched and must not be blindly repeated.
    Resetting,
    /// Wait for evidence carrying this attempt's token.
    AwaitingAcknowledgement,
    /// Remove only owned media and restore the preserved boot settings.
    Detaching,
    /// This attempt and its media cleanup have completed.
    Completed,
    /// Cancellation and owned-media cleanup have completed.
    Cancelled,
    /// A non-retryable conflict or uncertain external write needs attention.
    Failed,
}

impl HostBootPhase {
    /// Whether a new generation may replace this fully cleaned request.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

/// A frozen host identity and artifact together with its recoverable boot progress.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HostBootIntent {
    /// Expected-machine identity; registration remains the source of MAC/IP truth.
    pub expected_machine_id: Uuid,
    /// Frozen BMC MAC used by lifecycle hooks before a machine ID exists.
    pub bmc_mac_address: MacAddress,
    /// Registered Host interface used to fetch PXE instructions.
    pub boot_mac_address: MacAddress,
    /// Expected external DHCP fixed lease, copied from the registered interface.
    pub fixed_ip_address: IpAddr,
    /// Caller-generated idempotency key, immutable for this task.
    pub generation: Uuid,
    /// Frozen purpose supplied by the caller for idempotent request retries.
    pub requested_purpose: HostBootPurpose,
    /// Explicit reason for the current lifecycle boot operation.
    pub purpose: HostBootPurpose,
    /// Immutable, unauthenticated HTTP(S) URL the BMC can fetch.
    pub artifact_url: String,
    /// Lowercase hexadecimal SHA-256 of the published bootstrap ISO.
    pub artifact_sha256: String,
    /// Durable external-operation boundary.
    pub phase: HostBootPhase,
    /// Redfish slot Core selected and owns for this artifact.
    pub media_uri: Option<String>,
    /// Original Redfish Boot object, retained until owned-media cleanup succeeds.
    pub previous_boot: Option<serde_json::Value>,
    /// Nonce rotated before every boot attempt, including remounts in one generation.
    pub acknowledgement_token: Uuid,
    /// Current-token Scout or installer evidence has been accepted.
    pub acknowledged: bool,
    /// Cancellation stops future boot writes and requests owned-media cleanup.
    pub cancelled: bool,
    /// Time the current asynchronous external operation was durably dispatched.
    pub phase_started_at: Option<DateTime<Utc>>,
    /// Persisted before dispatching reset; absence is distinct from an unknown result.
    pub reset_attempted_at: Option<DateTime<Utc>>,
    /// Last diagnostic, without BMC credentials or private keys.
    pub last_error: Option<String>,
    /// External-operation boundary retained for explicit cleanup recovery.
    #[serde(default)]
    pub failed_from_phase: Option<HostBootPhase>,
    /// Controller state/version key that deduplicates a lifecycle-triggered boot.
    pub lifecycle_key: Option<String>,
    /// Compare-and-swap revision, incremented by every persisted update.
    pub revision: i64,
}

impl HostBootIntent {
    /// Whether this policy has proven that no owned media or boot snapshot remains.
    pub fn cleanup_complete(&self) -> bool {
        self.phase.is_terminal() && self.media_uri.is_none() && self.previous_boot.is_none()
    }

    /// Accept current dispatched-attempt evidence, including proof needed for safe cancellation cleanup.
    pub fn accepts_acknowledgement(&self, generation: Uuid, token: Uuid) -> bool {
        (!self.cancelled
            || matches!(
                self.phase,
                HostBootPhase::Resetting | HostBootPhase::AwaitingAcknowledgement
            ))
            && self.generation == generation
            && self.acknowledgement_token == token
            && matches!(
                self.phase,
                HostBootPhase::Resetting
                    | HostBootPhase::AwaitingAcknowledgement
                    | HostBootPhase::Detaching
                    | HostBootPhase::Completed
            )
    }

    /// Start one lifecycle-keyed attempt after the caller proves prior media cleanup.
    ///
    /// The caller persists this transition with compare-and-swap before any
    /// external write. Artifact ownership and the original boot snapshot are
    /// deliberately retained until cleanup is proven.
    pub fn begin_lifecycle_attempt(&mut self, key: String, purpose: HostBootPurpose) {
        self.phase = HostBootPhase::Pending;
        self.lifecycle_key = Some(key);
        self.purpose = purpose;
        self.acknowledgement_token = Uuid::new_v4();
        self.acknowledged = false;
        self.reset_attempted_at = None;
        self.phase_started_at = None;
        self.last_error = None;
        self.failed_from_phase = None;
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    fn intent() -> HostBootIntent {
        HostBootIntent {
            expected_machine_id: Uuid::new_v4(),
            bmc_mac_address: "02:00:00:00:00:01".parse().unwrap(),
            boot_mac_address: "02:00:00:00:00:02".parse().unwrap(),
            fixed_ip_address: "192.0.2.10".parse().unwrap(),
            generation: Uuid::new_v4(),
            requested_purpose: HostBootPurpose::Discovery,
            purpose: HostBootPurpose::Discovery,
            artifact_url: "http://example.test/boot.iso".into(),
            artifact_sha256: "a".repeat(64),
            phase: HostBootPhase::AwaitingAcknowledgement,
            media_uri: None,
            previous_boot: None,
            acknowledgement_token: Uuid::new_v4(),
            acknowledged: false,
            cancelled: false,
            phase_started_at: None,
            reset_attempted_at: None,
            last_error: None,
            failed_from_phase: None,
            lifecycle_key: Some("discovery:1".into()),
            revision: 0,
        }
    }

    #[test]
    fn current_attempt_evidence_is_required() {
        use HostBootPhase::{Armed, AwaitingAcknowledgement, Completed};
        let cases = [
            Check {
                scenario: "current dispatched Scout",
                input: (AwaitingAcknowledgement, false, true, true),
                expect: true,
            },
            Check {
                scenario: "stale task",
                input: (AwaitingAcknowledgement, false, false, true),
                expect: false,
            },
            Check {
                scenario: "old boot within same task",
                input: (AwaitingAcknowledgement, false, true, false),
                expect: false,
            },
            Check {
                scenario: "not dispatched",
                input: (Armed, false, true, true),
                expect: false,
            },
            Check {
                scenario: "cancelled current boot proves safe cleanup",
                input: (AwaitingAcknowledgement, true, true, true),
                expect: true,
            },
            Check {
                scenario: "idempotent accepted evidence",
                input: (Completed, false, true, true),
                expect: true,
            },
        ];
        check_values(
            cases,
            |(phase, cancelled, generation_matches, token_matches)| {
                let mut row = intent();
                row.phase = phase;
                row.cancelled = cancelled;
                let generation = if generation_matches {
                    row.generation
                } else {
                    Uuid::new_v4()
                };
                let token = if token_matches {
                    row.acknowledgement_token
                } else {
                    Uuid::new_v4()
                };
                row.accepts_acknowledgement(generation, token)
            },
        );
    }

    #[test]
    fn remount_rotates_nonce_and_retains_artifact_ownership() {
        let mut row = intent();
        let previous_nonce = row.acknowledgement_token;
        let original_generation = row.generation;
        row.media_uri = Some("/redfish/v1/Managers/1/VirtualMedia/1".into());
        row.previous_boot = Some(serde_json::json!({"BootSourceOverrideEnabled":"Disabled"}));
        row.acknowledged = true;
        row.begin_lifecycle_attempt("release:2".into(), HostBootPurpose::Cleanup);
        assert_eq!(row.generation, original_generation);
        assert_ne!(row.acknowledgement_token, previous_nonce);
        assert!(!row.acknowledged);
        assert!(row.media_uri.is_some());
        assert!(row.previous_boot.is_some());
        assert!(!row.accepts_acknowledgement(original_generation, previous_nonce));
        row.phase = HostBootPhase::Completed;
        assert!(!row.cleanup_complete());
        row.media_uri = None;
        assert!(!row.cleanup_complete());
        row.previous_boot = None;
        assert!(row.cleanup_complete());
    }
}
