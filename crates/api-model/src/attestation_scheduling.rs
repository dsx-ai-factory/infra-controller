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
//! kept. It is stored on the machine rather than on the job because six of the
//! nine outcomes create no job.

use health_report::HealthProbeAlert;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::attestation::profile::{ANY_HARDWARE_CLASS, ComponentIdMatch, UnsatisfiedRequirement};

/// The attester selection decision, used by scheduling and by read-only
/// assessment.
///
/// A BMC that cannot be reached is not one of these: it stays an error so the
/// controller retries it, where every value here is a settled answer.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, carbide_instrument::LabelValue,
)]
pub enum SchedulingOutcome {
    /// One work row was written per selected attester.
    Scheduled,
    /// Work rows were written, but an allowlist pattern matched no eligible
    /// attester. Attesting is under way for less than the profile asked for.
    PartiallySatisfied,
    /// The profile's mode is `NONE`. The BMC is not contacted.
    AttestationDisabled,
    /// The BMC exposed no eligible attesters for this attempt.
    NoAttestersFound,
    /// An operator-authored requirement went unsatisfied, and nothing was left
    /// to attest.
    PolicyMatchedNothing,
    /// No exploration has recorded a hardware class for this machine's BMC,
    /// and no `any` profile covers it either.
    ClassNotRecorded,
    /// Neither the machine's class nor `any` has a profile.
    NoProfile,
    /// Eligible attesters existed, but none matched the manual attester
    /// override.
    AttesterOverrideMatchedNothing,
    /// Eligible attesters were selected by the manual attester override.
    AttesterOverrideScheduled,
}

/// What attester selection decided and which profile decided it. Carried from
/// selection to the trigger's result and into the stored record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchedulingDecision {
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
}

/// The latest attestation scheduling decision for one machine.
///
/// Every key is always written. A value that does not apply is stored as
/// `null`, so a reader never has to tell a missing key from an absent value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttestationScheduling {
    /// Stored as top-level keys, beside the fields below.
    #[serde(flatten)]
    pub decision: SchedulingDecision,
    /// Which requirement went unsatisfied. `None` for the outcomes that
    /// satisfied one or applied none.
    pub unsatisfied: Option<UnsatisfiedRequirement>,
    /// The attesters an operator named on one trigger, in place of a profile,
    /// in ascending order. Nothing else records them, and the report has no
    /// other way to tell which attesters an override put in scope. `None`
    /// when a profile supplied the selection, which is every lifecycle
    /// attempt. Never empty: an override that selected nothing is recorded
    /// nowhere.
    ///
    /// Distinct from the operator overrides that waive or fail a stored
    /// result: this one decides what gets attested, not what an attestation
    /// concluded.
    pub attester_override: Option<Vec<String>>,
    /// The job this run created. `None` when it created none.
    pub job_id: Option<Uuid>,
}

impl AttestationScheduling {
    /// Returns the coverage alert this decision calls for, or `None` when the
    /// outcome reports no gap between the profile and the hardware.
    ///
    /// The six outcomes that raise nothing either had nothing to report, had
    /// no policy to report against, or came from an attester override.
    pub fn coverage_alert(&self) -> Option<HealthProbeAlert> {
        match self.decision.outcome {
            SchedulingOutcome::NoAttestersFound => {
                Some(HealthProbeAlert::attestation_no_attesters(
                    self.decision.hardware_class.clone(),
                    self.message(
                        "Nothing was attested",
                        "applied, but the BMC reported no eligible attester",
                    ),
                ))
            }
            SchedulingOutcome::PartiallySatisfied => {
                Some(HealthProbeAlert::attestation_policy_unsatisfied(
                    self.decision.hardware_class.clone(),
                    self.message("Attesting less than requested", &self.unsatisfied_detail()),
                ))
            }
            SchedulingOutcome::PolicyMatchedNothing => {
                Some(HealthProbeAlert::attestation_policy_unsatisfied(
                    self.decision.hardware_class.clone(),
                    self.message("Nothing was attested", &self.unsatisfied_detail()),
                ))
            }
            // An override is one operator's request rather than the machine's
            // policy, so one that matched nothing is reported to the caller
            // who asked and never recorded, and a gap it leaves is not the
            // machine's.
            SchedulingOutcome::Scheduled
            | SchedulingOutcome::AttestationDisabled
            | SchedulingOutcome::ClassNotRecorded
            | SchedulingOutcome::NoProfile
            | SchedulingOutcome::AttesterOverrideMatchedNothing
            | SchedulingOutcome::AttesterOverrideScheduled => None,
        }
    }

    /// Names the profile that selected the attesters, so an operator can tell
    /// one revision from another, and follows it with what went wrong and the
    /// job, when the attempt created one.
    ///
    /// Only profile-driven outcomes raise a coverage alert, so no message here
    /// describes an attester override.
    fn message(&self, prefix: &str, detail: &str) -> String {
        let name = if self.decision.used_any_fallback {
            ANY_HARDWARE_CLASS
        } else {
            self.decision.hardware_class.as_deref().unwrap_or("unknown")
        };
        let job = self
            .job_id
            .map(|job_id| format!(" (job {job_id})"))
            .unwrap_or_default();
        match &self.decision.profile_version {
            Some(version) => {
                format!("{prefix}: attestation profile '{name}' version {version} {detail}{job}.")
            }
            None => format!("{prefix}: attestation profile '{name}' {detail}{job}."),
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
            decision: SchedulingDecision {
                outcome,
                hardware_class: Some("nvidia_dgx-h100".to_string()),
                used_any_fallback: false,
                profile_version: Some("V7-T1760631000000000".to_string()),
            },
            unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                ComponentIdMatch::Exact("HGX_ERoT_GPU_7".to_string()),
            ])),
            attester_override: None,
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

            // These got what was asked for, or had nothing to report against.
            // An override that matched nothing is reported to the caller
            // who asked for it and never recorded, so it reaches no report.
            "an outcome with no gap raises nothing" {
                SchedulingOutcome::Scheduled => None,
                SchedulingOutcome::AttestationDisabled => None,
                SchedulingOutcome::ClassNotRecorded => None,
                SchedulingOutcome::NoProfile => None,
                SchedulingOutcome::AttesterOverrideMatchedNothing => None,
                SchedulingOutcome::AttesterOverrideScheduled => None,
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

        let mut denylist = AttestationScheduling {
            unsatisfied: Some(UnsatisfiedRequirement::DenylistExcludedEverything),
            ..scheduling(SchedulingOutcome::PolicyMatchedNothing)
        };
        denylist.decision.used_any_fallback = true;
        assert_eq!(
            denylist
                .coverage_alert()
                .expect("the outcome raises a coverage alert")
                .message,
            "Nothing was attested: attestation profile 'any' version V7-T1760631000000000 \
             excluded every attester the BMC reported."
        );

        let with_job = AttestationScheduling {
            job_id: Some(Uuid::nil()),
            ..scheduling(SchedulingOutcome::PartiallySatisfied)
        };
        assert_eq!(
            with_job
                .coverage_alert()
                .expect("the outcome raises a coverage alert")
                .message,
            "Attesting less than requested: attestation profile 'nvidia_dgx-h100' version \
             V7-T1760631000000000 requires attesters matching 'HGX_ERoT_GPU_7', which the BMC \
             did not report (job 00000000-0000-0000-0000-000000000000)."
        );
    }

    #[test]
    fn scheduling_round_trips_through_its_stored_shape() {
        // Operators query this JSON by key, so the spelling is a contract.
        let record = AttestationScheduling {
            decision: SchedulingDecision {
                outcome: SchedulingOutcome::PartiallySatisfied,
                hardware_class: Some("nvidia_dgx-h100".to_string()),
                used_any_fallback: false,
                profile_version: Some("V7-T1760631000000000".to_string()),
            },
            unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                ComponentIdMatch::Exact("HGX_ERoT_GPU_7".to_string()),
            ])),
            attester_override: None,
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
                "attester_override": null,
                "job_id": "7c3f1f0e-1d2a-4f7b-9a21-0b5e3c8d41a6",
            })
        );
        assert_eq!(
            serde_json::from_value::<AttestationScheduling>(stored).expect("deserializes"),
            record
        );
    }

    #[test]
    fn an_override_round_trips_the_attesters_it_selected() {
        // The report reads these attesters back to decide whose failures it
        // reports, so their key and order are a contract.
        let mut record = AttestationScheduling {
            attester_override: Some(vec![
                "HGX_ERoT_GPU_0".to_string(),
                "HGX_ERoT_GPU_2".to_string(),
            ]),
            unsatisfied: None,
            ..scheduling(SchedulingOutcome::AttesterOverrideScheduled)
        };
        record.decision.profile_version = None;

        let stored = serde_json::to_value(&record).expect("serializes");
        assert_eq!(
            stored["attester_override"],
            serde_json::json!(["HGX_ERoT_GPU_0", "HGX_ERoT_GPU_2"])
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
            decision: SchedulingDecision {
                outcome: SchedulingOutcome::NoProfile,
                hardware_class: Some("nvidia_mgx-power-shelf".to_string()),
                used_any_fallback: false,
                profile_version: None,
            },
            unsatisfied: None,
            attester_override: None,
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
                "attester_override": null,
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
