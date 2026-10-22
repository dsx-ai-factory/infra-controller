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

use health_report::HealthProbeAlert;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::attestation::profile::{ANY_HARDWARE_CLASS, ComponentIdMatch, UnsatisfiedRequirement};

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

impl AttestationScheduling {
    /// Returns the coverage alert this decision calls for, or `None` when the
    /// outcome reports no gap between the profile and the hardware.
    ///
    /// The four outcomes that raise nothing either had nothing to report or had
    /// no policy to report against.
    pub fn coverage_alert(&self) -> Option<HealthProbeAlert> {
        match self.outcome {
            SchedulingOutcome::NoAttestersFound => {
                Some(HealthProbeAlert::attestation_no_attesters(
                    self.hardware_class.clone(),
                    self.message(
                        "Nothing was attested",
                        "applied, but the BMC reported no eligible attester",
                    ),
                ))
            }
            SchedulingOutcome::PartiallySatisfied => {
                Some(HealthProbeAlert::attestation_policy_unsatisfied(
                    self.hardware_class.clone(),
                    self.message("Attesting less than requested", &self.unsatisfied_detail()),
                ))
            }
            SchedulingOutcome::PolicyMatchedNothing => {
                Some(HealthProbeAlert::attestation_policy_unsatisfied(
                    self.hardware_class.clone(),
                    self.message("Nothing was attested", &self.unsatisfied_detail()),
                ))
            }
            SchedulingOutcome::Scheduled
            | SchedulingOutcome::AttestationDisabled
            | SchedulingOutcome::ClassNotRecorded
            | SchedulingOutcome::NoProfile => None,
        }
    }

    /// Names the profile that decided, so an operator can see which revision to
    /// look at, and follows it with what went wrong.
    fn message(&self, prefix: &str, detail: &str) -> String {
        let name = if self.used_any_fallback {
            ANY_HARDWARE_CLASS
        } else {
            self.hardware_class.as_deref().unwrap_or("unknown")
        };
        match &self.profile_version {
            Some(version) => {
                format!("{prefix}: attestation profile '{name}' version {version} {detail}.")
            }
            None => format!("{prefix}: attestation profile '{name}' {detail}."),
        }
    }

    fn unsatisfied_detail(&self) -> String {
        match &self.unsatisfied {
            Some(UnsatisfiedRequirement::AllowlistPatterns(patterns)) => {
                let patterns: Vec<String> = patterns
                    .iter()
                    .map(|pattern| match pattern {
                        ComponentIdMatch::Exact(id) => format!("'{id}'"),
                        ComponentIdMatch::Prefix(prefix) => format!("'{prefix}*'"),
                    })
                    .collect();
                format!(
                    "requires attesters matching {}, which the BMC did not report",
                    patterns.join(", ")
                )
            }
            Some(UnsatisfiedRequirement::DenylistExcludedEverything) => {
                "excluded every attester the BMC reported".to_string()
            }
            None => "left a requirement unsatisfied".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// A decision whose profile named one attester the BMC did not report.
    fn scheduling(outcome: SchedulingOutcome) -> AttestationScheduling {
        AttestationScheduling {
            outcome,
            hardware_class: Some("nvidia_dgx-h100".to_string()),
            used_any_fallback: false,
            profile_version: Some("V7-T1760631000000000".to_string()),
            unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                ComponentIdMatch::Exact("HGX_ERoT_GPU_7".to_string()),
            ])),
            job_id: None,
        }
    }

    #[test]
    fn each_outcome_raises_the_coverage_alert_it_calls_for() {
        fn alert_id(outcome: SchedulingOutcome) -> Option<String> {
            scheduling(outcome)
                .coverage_alert()
                .map(|alert| alert.id.as_str().to_string())
        }

        value_scenarios!(
            run = alert_id;

            "a gap between the profile and the hardware is reported" {
                SchedulingOutcome::NoAttestersFound => Some("AttestationNoAttesters".to_string()),
                SchedulingOutcome::PartiallySatisfied =>
                    Some("AttestationPolicyUnsatisfied".to_string()),
                SchedulingOutcome::PolicyMatchedNothing =>
                    Some("AttestationPolicyUnsatisfied".to_string()),
            }

            // Two of these got what the policy asked for, and two had no policy
            // to report against.
            "an outcome with no gap raises nothing" {
                SchedulingOutcome::Scheduled => None,
                SchedulingOutcome::AttestationDisabled => None,
                SchedulingOutcome::ClassNotRecorded => None,
                SchedulingOutcome::NoProfile => None,
            }
        );
    }

    #[test]
    fn a_coverage_alert_names_the_profile_version_and_requirement() {
        // An operator reads these to find which revision decided, so they are
        // asserted whole rather than by substring.
        let patterns = scheduling(SchedulingOutcome::PolicyMatchedNothing)
            .coverage_alert()
            .expect("the outcome raises a coverage alert");
        assert_eq!(patterns.target.as_deref(), Some("nvidia_dgx-h100"));
        assert_eq!(
            patterns.message,
            "Nothing was attested: attestation profile 'nvidia_dgx-h100' version \
             V7-T1760631000000000 requires attesters matching 'HGX_ERoT_GPU_7', which the BMC \
             did not report."
        );

        let denylist = AttestationScheduling {
            used_any_fallback: true,
            unsatisfied: Some(UnsatisfiedRequirement::DenylistExcludedEverything),
            ..scheduling(SchedulingOutcome::PolicyMatchedNothing)
        };
        assert_eq!(
            denylist
                .coverage_alert()
                .expect("the outcome raises a coverage alert")
                .message,
            "Nothing was attested: attestation profile 'any' version V7-T1760631000000000 \
             excluded every attester the BMC reported."
        );
    }

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
