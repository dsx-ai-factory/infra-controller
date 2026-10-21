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

//! What attestation scheduling decided for one machine.
//!
//! Stored as one JSON object in `machines.attestation_scheduling` and rewritten
//! in full on every run, so it holds the latest decision and no history. A
//! machine that has never been scheduled has none.
//!
//! It is stored rather than worked out later because the outcome depends on
//! which attesters the BMC reported at that moment, and that list is never
//! kept. It is stored on the machine rather than on the job because five of the
//! seven outcomes create no job.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::attestation::profile::UnsatisfiedRequirement;

/// What scheduling decided for one machine.
///
/// A BMC that cannot be reached is not one of these: it stays an error so the
/// controller retries it, where every value here is a settled answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SchedulingOutcome {
    /// One work row was written per selected attester.
    Scheduled,
    /// Work rows were written, but an allowlist pattern matched no eligible
    /// attester. Attesting is under way for less than the profile asked for.
    PartiallySatisfied,
    /// The profile's mode is `NONE`. The BMC is not contacted.
    AttestationDisabled,
    /// The BMC offered nothing eligible, under a policy that asserted nothing
    /// about what must be there. Points at the hardware, not the profile.
    NoAttestersFound,
    /// An operator-authored requirement went unsatisfied, and nothing was left
    /// to attest.
    PolicyMatchedNothing,
    /// No exploration has recorded a hardware class for this machine's BMC,
    /// and no `any` profile covers it either.
    ClassNotRecorded,
    /// Neither the machine's class nor `any` has a profile.
    NoProfile,
}

/// The latest attestation scheduling decision for one machine.
///
/// Every key is always written. A value that does not apply is stored as
/// `null`, so a reader never has to tell a missing key from an absent value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttestationScheduling {
    /// How the run ended.
    pub outcome: SchedulingOutcome,
    /// The class recorded for the machine's BMC, in `<manufacturer>_<model>`
    /// form. `None` when exploration has recorded none. The proto response
    /// reports that same case as an empty string, and this record does not
    /// copy that.
    pub hardware_class: Option<String>,
    /// Whether the reserved `any` profile supplied the policy.
    pub used_any_fallback: bool,
    /// The version of the profile that decided, as
    /// [`config_version::ConfigVersion::version_string`] renders it. `None`
    /// when no profile applied. Text rather than a version, because it is only
    /// read back to name the revision that decided.
    pub profile_version: Option<String>,
    /// Which requirement went unsatisfied. `None` for the five outcomes that
    /// satisfied one or applied none.
    pub unsatisfied: Option<UnsatisfiedRequirement>,
    /// The job this run created. `None` when it created none.
    pub job_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::profile::ComponentIdMatch;

    #[test]
    fn scheduling_round_trips_through_its_stored_shape() {
        // Operators query this JSON by key, so the spelling is a contract.
        let record = AttestationScheduling {
            outcome: SchedulingOutcome::PartiallySatisfied,
            hardware_class: Some("nvidia_dgx-h100".to_string()),
            used_any_fallback: false,
            profile_version: Some("V7-T1760631000000000".to_string()),
            unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                ComponentIdMatch::Exact("HGX_ERoT_GPU_7".to_string()),
            ])),
            job_id: Some(
                Uuid::parse_str("7c3f1f0e-1d2a-4f7b-9a21-0b5e3c8d41a6").expect("valid uuid"),
            ),
        };

        let stored = serde_json::to_value(&record).expect("serializes");
        assert_eq!(
            stored,
            serde_json::json!({
                "outcome": "PartiallySatisfied",
                "hardware_class": "nvidia_dgx-h100",
                "used_any_fallback": false,
                "profile_version": "V7-T1760631000000000",
                "unsatisfied": { "allowlist_patterns": [{ "exact": "HGX_ERoT_GPU_7" }] },
                "job_id": "7c3f1f0e-1d2a-4f7b-9a21-0b5e3c8d41a6",
            })
        );
        assert_eq!(
            serde_json::from_value::<AttestationScheduling>(stored).expect("deserializes"),
            record
        );
    }

    #[test]
    fn a_decision_that_created_no_job_stores_every_key_as_null() {
        // The uncovered-machines query reads these keys on machines that never
        // reached a profile, so they have to be present rather than omitted.
        let record = AttestationScheduling {
            outcome: SchedulingOutcome::NoProfile,
            hardware_class: Some("nvidia_mgx-power-shelf".to_string()),
            used_any_fallback: false,
            profile_version: None,
            unsatisfied: None,
            job_id: None,
        };

        assert_eq!(
            serde_json::to_value(&record).expect("serializes"),
            serde_json::json!({
                "outcome": "NoProfile",
                "hardware_class": "nvidia_mgx-power-shelf",
                "used_any_fallback": false,
                "profile_version": null,
                "unsatisfied": null,
                "job_id": null,
            })
        );
    }

    #[test]
    fn a_denylist_that_excluded_everything_stores_as_a_bare_string() {
        let stored = serde_json::to_value(UnsatisfiedRequirement::DenylistExcludedEverything)
            .expect("serializes");

        assert_eq!(stored, serde_json::json!("denylist_excluded_everything"));
    }
}
