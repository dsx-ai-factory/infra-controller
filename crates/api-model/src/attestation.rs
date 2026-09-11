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
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use sqlx::FromRow;

#[derive(FromRow, Debug)]
pub struct EkCertVerificationStatus {
    pub ek_sha256: Vec<u8>,
    pub serial_num: String,
    pub signing_ca_found: bool,
    pub issuer: Vec<u8>,
    pub issuer_access_info: Option<String>,
    pub machine_id: MachineId,
    // pub ca_id: Option<i32>, // currently unused
}

#[derive(FromRow, Debug, sqlx::Encode)]
pub struct SecretAkPub {
    pub secret: Vec<u8>,
    pub ak_pub: Vec<u8>,
}

#[derive(FromRow, Debug, sqlx::Encode)]
pub struct TpmCaCert {
    pub id: i32,
    pub not_valid_before: DateTime<Utc>,
    pub not_valid_after: DateTime<Utc>,
    #[sqlx(default)]
    pub ca_cert_der: Vec<u8>,
    pub cert_subject: Vec<u8>,
}

/// Model for SPDM attestation via Redfish
pub mod spdm {
    use std::fmt::Display;
    use std::str::FromStr;

    use config_version::ConfigVersion;
    use itertools::Itertools;
    use nras::{NrasError, NrasVerifierClient, ProcessedAttestationOutcome, RawAttestationOutcome};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use sqlx::Row;
    use sqlx::postgres::PgRow;

    use super::*;
    use crate::bmc_info::BmcInfo;
    use crate::controller_outcome::PersistentStateHandlerOutcome;

    /// Data model to store progress of attestation related to a device/component of a machine BMC (e.g.
    /// GPU, CPU, BMC, CX7)
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct SpdmDeviceAttestation {
        // Host or DPU's machine id
        pub machine_id: MachineId,
        // Component/device of the machine (GPU, CPU, BMC)
        // e.g. HGX_IRoT_GPU_0, HGX_ERoT_CPU_0
        pub device_id: String,
        // BMC info to create a redfish client
        pub bmc_info: BmcInfo,
        // Nonce used in attestation with both NRAS and BMC
        pub nonce: uuid::Uuid,
        // Device State.
        pub state: SpdmAttestationState,
        // State version will increase
        pub state_version: ConfigVersion,
        /// The result of the last attempt to change state
        pub state_outcome: Option<PersistentStateHandlerOutcome>,
        // Fetched latest value during attestation.
        pub metadata: Option<SpdmMachineDeviceMetadata>,
        // CA certificate link to fetch the certificate.
        pub ca_certificate_link: Option<String>,
        // CA certificate fetched from the link.
        pub ca_certificate: Option<CaCertificate>,
        // Evidence target link, used to trigger the measurement collection.
        pub evidence_target: Option<String>,
        // Collected Evidence.
        pub evidence: Option<Evidence>,
        // timestamps
        pub started_at: DateTime<Utc>,
        pub cancelled_at: Option<DateTime<Utc>>,
        pub completed_at: Option<DateTime<Utc>>,
    }

    impl SpdmDeviceAttestation {
        pub fn nonce_hex(&self) -> String {
            hex::encode(Sha256::digest(self.nonce.as_bytes()))
        }
    }

    /// Major state, associated with Machine.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SpdmAttestationState {
        FetchMetadata,
        FetchCertificate,
        TriggerEvidenceCollection { retry_count: i32 },
        PollEvidenceCollection { task_id: String, retry_count: i32 },
        NrasVerification,
        ApplyAppraisalPolicy,
        Passed,
        Failed(String),
        Cancelled,
    }

    impl<'r> sqlx::FromRow<'r, PgRow> for SpdmAttestationState {
        fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
            let controller_state: sqlx::types::Json<SpdmAttestationState> = row.try_get("state")?;
            Ok(controller_state.0)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum SpdmAttestationStatus {
        InProgress,
        Cancelled,
        Passed,
        Failed,
    }

    #[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
    pub enum SpdmHandlerError {
        #[error("unable to complete measurement trigger: {0}")]
        TriggerMeasurementFail(String),
        #[error("nras error: {0}")]
        NrasError(#[from] nras::NrasError),
        #[error("missing values: {field} - {machine_id}/{device_id}")]
        MissingData {
            field: String,
            machine_id: MachineId,
            device_id: String,
        },
        #[error("verifier not implemented at {module} for: {machine_id}/{device_id}")]
        VerifierNotImplemented {
            module: String,
            machine_id: MachineId,
            device_id: String,
        },
        #[error("verification failed: {0}")]
        VerificationFailed(String),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AttestationStatus {
        Success,
        NotSupported,
        Failure { cause: SpdmHandlerError },
    }

    #[derive(Debug)]
    pub enum DeviceType {
        Gpu,
        Cx7,
        Unknown,
    }

    impl FromStr for DeviceType {
        type Err = SpdmHandlerError;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Ok(if s.contains("GPU") {
                DeviceType::Gpu
            } else if s.contains("CX7") {
                DeviceType::Cx7
            } else {
                DeviceType::Unknown
            })
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromRow)]
    pub struct SpdmObjectId_ {
        pub machine_id: MachineId,
        pub device_id: String,
    }

    #[derive(thiserror::Error, Debug, Clone)]
    pub enum SpdmObjectIdParseError {
        #[error("the object ID must have 2 parts but not as should be {0:?}")]
        WrongFormat(String),
        #[error("the machine ID parsing failed: {0}")]
        MachineIdParsingFailed(#[from] carbide_uuid::machine::MachineIdParseError),
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, FromRow)]
    pub struct SpdmObjectId(pub MachineId, pub String);

    impl FromStr for SpdmObjectId {
        type Err = SpdmObjectIdParseError;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let values = s.split(',').collect_vec();
            if values.len() != 2 {
                return Err(SpdmObjectIdParseError::WrongFormat(s.to_string()));
            }

            Ok(Self(
                values[0].parse().map_err(SpdmObjectIdParseError::from)?,
                values[1].to_string(),
            ))
        }
    }

    impl Display for SpdmObjectId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{},{}", self.0, self.1.clone())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SpdmMachineDeviceMetadata {
        pub firmware_version: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize, Clone)]
    #[serde(rename_all = "PascalCase")]
    pub struct CaCertificate {
        pub certificate_string: String,
        pub certificate_type: String,
        pub certificate_usage_types: Vec<String>,
        pub id: String,
        pub name: String,
        #[serde(rename = "SPDM")]
        pub spdm: SlotInfo,
    }

    #[derive(Debug, Serialize, Deserialize, Clone)]
    #[serde(rename_all = "PascalCase")]
    pub struct Evidence {
        pub hashing_algorithm: String,
        pub signed_measurements: String,
        pub signing_algorithm: String,
        pub version: String,
    }

    #[derive(Debug, Serialize, Deserialize, Clone)]
    #[serde(rename_all = "PascalCase")]
    pub struct SlotInfo {
        pub slot_id: u16,
    }

    impl<'r> sqlx::FromRow<'r, PgRow> for SpdmDeviceAttestation {
        fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
            let controller_state: sqlx::types::Json<SpdmAttestationState> = row.try_get("state")?;
            let bmc_info: sqlx::types::Json<BmcInfo> = row.try_get("bmc_info")?;

            let ca_certificate: Option<sqlx::types::Json<CaCertificate>> =
                row.try_get("ca_certificate")?;
            let evidence: Option<sqlx::types::Json<Evidence>> = row.try_get("evidence")?;
            let metadata: Option<sqlx::types::Json<SpdmMachineDeviceMetadata>> =
                row.try_get("metadata")?;
            let controller_state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
                row.try_get("state_outcome")?;

            Ok(SpdmDeviceAttestation {
                machine_id: row.try_get("machine_id")?,
                state: controller_state.0,
                state_version: row.try_get("state_version")?,
                state_outcome: controller_state_outcome.map(|x| x.0),
                device_id: row.try_get("device_id")?,
                nonce: row.try_get("nonce")?,
                bmc_info: bmc_info.0,
                metadata: metadata.map(|x| x.0),
                ca_certificate_link: row.try_get("ca_certificate_link")?,
                evidence_target: row.try_get("evidence_target")?,
                ca_certificate: ca_certificate.map(|x| x.0),
                evidence: evidence.map(|x| x.0),
                started_at: row.try_get("started_at")?,
                cancelled_at: row.try_get("cancelled_at")?,
                completed_at: row.try_get("completed_at")?,
            })
        }
    }

    #[derive(Debug, Clone)]
    pub struct SpdmDeviceAttestationDetails {
        pub machine_id: MachineId,
        pub device_id: String,
        pub state: SpdmAttestationState,
        // timestamps
        pub started_at: DateTime<Utc>,
        pub cancelled_at: Option<DateTime<Utc>>,
        pub completed_at: Option<DateTime<Utc>>,
    }

    impl SpdmDeviceAttestationDetails {
        pub fn get_failure_cause(&self) -> Option<String> {
            if let SpdmAttestationState::Failed(msg) = &self.state {
                Some(format!(
                    "Device: {}, failed reason: {}",
                    self.device_id, msg
                ))
            } else {
                None
            }
        }
    }

    impl<'r> sqlx::FromRow<'r, PgRow> for SpdmDeviceAttestationDetails {
        fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
            let controller_state: sqlx::types::Json<SpdmAttestationState> = row.try_get("state")?;

            Ok(SpdmDeviceAttestationDetails {
                machine_id: row.try_get("machine_id")?,
                state: controller_state.0,
                device_id: row.try_get("device_id")?,
                started_at: row.try_get("started_at")?,
                cancelled_at: row.try_get("cancelled_at")?,
                completed_at: row.try_get("completed_at")?,
            })
        }
    }

    #[async_trait::async_trait]
    pub trait Verifier: std::fmt::Debug + Send + Sync + 'static {
        fn client(&self, nras_config: nras::Config) -> Box<dyn nras::VerifierClient>;
        async fn parse_attestation_outcome(
            &self,
            nras_config: &nras::Config,
            state: &RawAttestationOutcome,
        ) -> Result<ProcessedAttestationOutcome, NrasError>;
    }

    #[derive(Debug, Default)]
    pub struct VerifierImpl {}

    #[async_trait::async_trait]
    impl Verifier for VerifierImpl {
        fn client(&self, nras_config: nras::Config) -> Box<dyn nras::VerifierClient> {
            Box::new(NrasVerifierClient::new_with_config(&nras_config))
        }
        async fn parse_attestation_outcome(
            &self,
            nras_config: &nras::Config,
            state: &RawAttestationOutcome,
        ) -> Result<ProcessedAttestationOutcome, NrasError> {
            // now create a KeyStore to validate those tokens
            let nras_keystore = nras::NrasKeyStore::new_with_config(nras_config).await?;
            let parser = nras::Parser::new_with_config(nras_config);
            parser.parse_attestation_outcome(state, &nras_keystore)
        }
    }
}

/// Operator-authored policy naming which attesters a hardware class requires.
pub mod profile {
    use config_version::ConfigVersion;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::ConfigValidationError;
    use crate::site_explorer::UNRECOGNIZED_HARDWARE_CLASS;

    /// The one reserved class an operator may write. A profile keyed `any`
    /// covers hardware whose own class has no profile of its own.
    pub const ANY_HARDWARE_CLASS: &str = "any";

    /// The only policy document shape this build reads or writes.
    pub const POLICY_SCHEMA_VERSION: u32 = 1;

    /// Ordered from attesting nothing to naming an exact set.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum AttesterSelectionMode {
        /// Attest nothing. Attestation is disabled for this hardware.
        None,
        /// Attest every attester the BMC reports.
        All,
        /// Attest only the attesters matching a pattern.
        Allowlist,
        /// Attest every attester the BMC reports, except those matching a pattern.
        Denylist,
    }

    /// One matcher against a `ComponentIntegrity` `Id`.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ComponentIdMatch {
        /// One ID, matched in full.
        Exact(String),
        /// Every ID starting with the given string.
        Prefix(String),
    }

    impl ComponentIdMatch {
        fn value(&self) -> &str {
            match self {
                Self::Exact(value) | Self::Prefix(value) => value,
            }
        }

        /// Redfish treats `Id` as opaque, so this is case-sensitive.
        fn matches(&self, attester_id: &str) -> bool {
            match self {
                Self::Exact(id) => attester_id == id,
                Self::Prefix(prefix) => attester_id.starts_with(prefix),
            }
        }
    }

    /// What a selection decided for the attesters a BMC reported.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum SelectionOutcome {
        /// Attest these, in the order the BMC reported them.
        Scheduled(Vec<String>),
        /// The mode is `NONE`. A caller resolves this before contacting the
        /// BMC, so reaching it here means it listed anyway.
        AttestationDisabled,
        /// The BMC offered nothing eligible under a policy that asserted
        /// nothing about what must be there. Not a failure, and it points at
        /// the hardware rather than the profile.
        NoAttestersFound,
        /// An operator-authored requirement went unsatisfied.
        PolicyMatchedNothing(UnsatisfiedRequirement),
    }

    /// Which requirement went unsatisfied, so a caller can say which.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum UnsatisfiedRequirement {
        /// These patterns matched no eligible attester. A caller holding the
        /// attesters that failed eligibility can name the ones a pattern
        /// matched but eligibility skipped.
        AllowlistPatterns(Vec<ComponentIdMatch>),
        /// A denylist removed every attester the BMC offered. `NONE` is how an
        /// operator asks for nothing to be attested.
        DenylistExcludedEverything,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct AttesterSelection {
        pub mode: AttesterSelectionMode,
        #[serde(default)]
        pub component_ids: Vec<ComponentIdMatch>,
    }

    impl AttesterSelection {
        pub fn validate(&self) -> Result<(), ConfigValidationError> {
            let patterned = matches!(
                self.mode,
                AttesterSelectionMode::Allowlist | AttesterSelectionMode::Denylist
            );
            // An allowlist of nothing can never be satisfied, and a denylist of
            // nothing means ALL, which has its own spelling.
            if patterned && self.component_ids.is_empty() {
                return Err(ConfigValidationError::invalid_value(format!(
                    "{:?} requires at least one component ID pattern",
                    self.mode
                )));
            }
            if !patterned && !self.component_ids.is_empty() {
                return Err(ConfigValidationError::invalid_value(format!(
                    "{:?} takes no component ID patterns",
                    self.mode
                )));
            }
            // An empty prefix matches every ID, which is ALL by another name.
            if self.component_ids.iter().any(|id| id.value().is_empty()) {
                return Err(ConfigValidationError::invalid_value(
                    "a component ID pattern cannot be empty",
                ));
            }
            Ok(())
        }

        /// Applies the selection to the attesters a BMC reported that already
        /// passed eligibility, in the order it reported them.
        ///
        /// A selection is a requirement rather than a filter, so an allowlist
        /// pattern matching nothing fails the whole selection while a denylist
        /// pattern matching nothing excludes nothing.
        pub fn evaluate(&self, eligible: &[&str]) -> SelectionOutcome {
            match self.mode {
                AttesterSelectionMode::None => SelectionOutcome::AttestationDisabled,

                AttesterSelectionMode::All => {
                    let selected: Vec<String> = eligible.iter().map(|id| id.to_string()).collect();
                    if selected.is_empty() {
                        SelectionOutcome::NoAttestersFound
                    } else {
                        SelectionOutcome::Scheduled(selected)
                    }
                }

                AttesterSelectionMode::Allowlist => {
                    // Every pattern is its own requirement, so one that matches
                    // nothing fails the selection rather than attesting less.
                    let unsatisfied: Vec<_> = self
                        .component_ids
                        .iter()
                        .filter(|pattern| !eligible.iter().any(|id| pattern.matches(id)))
                        .cloned()
                        .collect();
                    let selected: Vec<String> = eligible
                        .iter()
                        .filter(|id| self.matches_any(id))
                        .map(|id| id.to_string())
                        .collect();
                    // A validated allowlist holds at least one pattern, so a
                    // satisfied one always selects something. The second test
                    // covers a selection built without `validate`, which would
                    // otherwise schedule nothing and report success.
                    if !unsatisfied.is_empty() || selected.is_empty() {
                        return SelectionOutcome::PolicyMatchedNothing(
                            UnsatisfiedRequirement::AllowlistPatterns(unsatisfied),
                        );
                    }
                    SelectionOutcome::Scheduled(selected)
                }

                AttesterSelectionMode::Denylist => {
                    // A denylist only subtracts, so an empty BMC is the same
                    // situation ALL reports: nothing was excluded, and nothing
                    // about the policy caused the emptiness.
                    if eligible.is_empty() {
                        return SelectionOutcome::NoAttestersFound;
                    }
                    let selected: Vec<String> = eligible
                        .iter()
                        .filter(|id| !self.matches_any(id))
                        .map(|id| id.to_string())
                        .collect();
                    if selected.is_empty() {
                        SelectionOutcome::PolicyMatchedNothing(
                            UnsatisfiedRequirement::DenylistExcludedEverything,
                        )
                    } else {
                        SelectionOutcome::Scheduled(selected)
                    }
                }
            }
        }

        fn matches_any(&self, attester_id: &str) -> bool {
            self.component_ids
                .iter()
                .any(|pattern| pattern.matches(attester_id))
        }
    }

    /// The profile's policy, stored as one JSON document so a later shape can
    /// be told apart from this one without a migration.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct AttestationPolicyDocument {
        pub schema_version: u32,
        pub selection: AttesterSelection,
    }

    impl AttestationPolicyDocument {
        pub fn new(selection: AttesterSelection) -> Self {
            Self {
                schema_version: POLICY_SCHEMA_VERSION,
                selection,
            }
        }

        pub fn validate(&self) -> Result<(), ConfigValidationError> {
            if self.schema_version != POLICY_SCHEMA_VERSION {
                return Err(ConfigValidationError::invalid_value(format!(
                    "unsupported policy schema version {}, expected {POLICY_SCHEMA_VERSION}",
                    self.schema_version
                )));
            }
            self.selection.validate()
        }
    }

    /// Rejects the class names an operator may not key a profile to. `any` is
    /// writable; `unrecognized` is the explorer's marker for hardware it could
    /// not classify, and such hardware is covered through `any`, so a profile
    /// keyed to it would never be read.
    pub fn validate_hardware_class(hardware_class: &str) -> Result<(), ConfigValidationError> {
        if hardware_class.is_empty() {
            return Err(ConfigValidationError::invalid_value(
                "hardware class cannot be empty",
            ));
        }
        if hardware_class == UNRECOGNIZED_HARDWARE_CLASS {
            return Err(ConfigValidationError::invalid_value(format!(
                "'{UNRECOGNIZED_HARDWARE_CLASS}' is reserved and cannot be used as a profile key"
            )));
        }
        Ok(())
    }

    /// One stored profile, as persisted in `attestation_profiles`.
    #[derive(Clone, Debug, FromRow)]
    pub struct AttestationProfile {
        pub hardware_class: String,
        pub version: ConfigVersion,
        #[sqlx(json)]
        pub policy_document: AttestationPolicyDocument,
        pub updated_at: DateTime<Utc>,
        pub updated_by: String,
    }

    /// Which profile applies to a machine, from the class recorded on its
    /// endpoint. An exact class match always wins over `any`.
    #[derive(Clone, Debug)]
    pub enum ProfileResolution {
        /// This profile applies. `used_any_fallback` distinguishes a policy
        /// written for this hardware from the default written for everything
        /// else, which the class alone cannot report.
        Resolved {
            profile: AttestationProfile,
            used_any_fallback: bool,
        },
        /// No exploration has recorded a class for the endpoint, so there is
        /// nothing to key on. `any` is not consulted: the machine is
        /// unclassified rather than classified as something unprofiled.
        ClassNotRecorded,
        /// The class resolved, but neither it nor `any` has a profile.
        NoProfile,
        /// Classification matched no `HwType`, and no `any` profile is stored.
        ClassUnrecognized,
    }

    /// A validated request to store a profile for a class that has none.
    #[derive(Clone, Debug)]
    pub struct NewAttestationProfile {
        pub hardware_class: String,
        pub policy_document: AttestationPolicyDocument,
    }

    /// A validated request to replace an existing profile's policy.
    #[derive(Clone, Debug)]
    pub struct UpdateAttestationProfile {
        pub hardware_class: String,
        pub policy_document: AttestationPolicyDocument,
        /// When set, the write applies only if the stored version still
        /// matches; when absent, it applies to whatever version is stored.
        pub if_version_match: Option<ConfigVersion>,
    }

    /// A validated request to remove a profile.
    #[derive(Clone, Debug)]
    pub struct DeleteAttestationProfile {
        pub hardware_class: String,
        pub if_version_match: Option<ConfigVersion>,
    }
}

#[cfg(test)]
mod profile_test {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::profile::*;

    fn selection(mode: AttesterSelectionMode, ids: &[ComponentIdMatch]) -> AttesterSelection {
        AttesterSelection {
            mode,
            component_ids: ids.to_vec(),
        }
    }

    fn exact(id: &str) -> ComponentIdMatch {
        ComponentIdMatch::Exact(id.to_string())
    }

    fn prefix(value: &str) -> ComponentIdMatch {
        ComponentIdMatch::Prefix(value.to_string())
    }

    // A GB200 tray, as the `libredfish/test_support.rs` fixture reports it.
    const TRAY: [&str; 4] = [
        "HGX_IRoT_GPU_0",
        "HGX_IRoT_GPU_1",
        "HGX_IRoT_GPU_2",
        "HGX_BMC_0",
    ];

    const GPUS: [&str; 3] = ["HGX_IRoT_GPU_0", "HGX_IRoT_GPU_1", "HGX_IRoT_GPU_2"];

    fn scheduled(attester_ids: &[&str]) -> SelectionOutcome {
        SelectionOutcome::Scheduled(attester_ids.iter().map(|id| id.to_string()).collect())
    }

    fn unsatisfied(patterns: &[ComponentIdMatch]) -> SelectionOutcome {
        SelectionOutcome::PolicyMatchedNothing(UnsatisfiedRequirement::AllowlistPatterns(
            patterns.to_vec(),
        ))
    }

    #[test]
    fn selection_validation() {
        scenarios!(
            run = |selection: AttesterSelection| selection.validate().map_err(drop);

            "an allowlist names at least one pattern" {
                selection(AttesterSelectionMode::Allowlist, &[prefix("HGX_IRoT_GPU_")]) => Yields(()),
            }

            "a denylist names at least one pattern" {
                selection(AttesterSelectionMode::Denylist, &[exact("HGX_BMC_0")]) => Yields(()),
            }

            "exact and prefix patterns may be mixed" {
                selection(
                    AttesterSelectionMode::Allowlist,
                    &[prefix("HGX_IRoT_GPU_"), exact("VERA_CPU_0")],
                ) => Yields(()),
            }

            "ALL takes no patterns" {
                selection(AttesterSelectionMode::All, &[]) => Yields(()),
            }

            "NONE takes no patterns" {
                selection(AttesterSelectionMode::None, &[]) => Yields(()),
            }

            // An allowlist of nothing can never be satisfied.
            "an allowlist without patterns is refused" {
                selection(AttesterSelectionMode::Allowlist, &[]) => Fails,
            }

            // A denylist of nothing means ALL, which has its own spelling.
            "a denylist without patterns is refused" {
                selection(AttesterSelectionMode::Denylist, &[]) => Fails,
            }

            "ALL with patterns is refused" {
                selection(AttesterSelectionMode::All, &[exact("HGX_BMC_0")]) => Fails,
            }

            "NONE with patterns is refused" {
                selection(AttesterSelectionMode::None, &[exact("HGX_BMC_0")]) => Fails,
            }

            // An empty prefix matches every ID, which is ALL by another name.
            "an empty prefix is refused" {
                selection(AttesterSelectionMode::Allowlist, &[prefix("")]) => Fails,
            }

            "an empty exact ID is refused" {
                selection(AttesterSelectionMode::Denylist, &[exact("")]) => Fails,
            }
        );
    }

    #[test]
    fn selection_over_a_reported_tray() {
        value_scenarios!(
            run = |selection: AttesterSelection| selection.evaluate(&TRAY);

            "an allowlist prefix takes the GPUs and not the BMC" {
                selection(AttesterSelectionMode::Allowlist, &[prefix("HGX_IRoT_GPU_")]) => scheduled(&GPUS),
            }

            "a denylist of the BMC leaves the same three" {
                selection(AttesterSelectionMode::Denylist, &[exact("HGX_BMC_0")]) => scheduled(&GPUS),
            }

            "ALL takes everything reported" {
                selection(AttesterSelectionMode::All, &[]) => scheduled(&TRAY),
            }

            "NONE takes nothing" {
                selection(AttesterSelectionMode::None, &[]) => SelectionOutcome::AttestationDisabled,
            }

            "mixed patterns take the union" {
                selection(
                    AttesterSelectionMode::Allowlist,
                    &[prefix("HGX_IRoT_GPU_"), exact("HGX_BMC_0")],
                ) => scheduled(&TRAY),
            }

            "an attester two patterns match is taken once" {
                selection(
                    AttesterSelectionMode::Allowlist,
                    &[prefix("HGX_IRoT_GPU_"), exact("HGX_IRoT_GPU_1")],
                ) => scheduled(&GPUS),
            }

            // Redfish treats `Id` as opaque, so the lowercase prefix matches
            // nothing and the requirement it states goes unsatisfied.
            "matching is case-sensitive" {
                selection(AttesterSelectionMode::Allowlist, &[prefix("hgx_irot_gpu_")])
                    => unsatisfied(&[prefix("hgx_irot_gpu_")]),
            }

            // An unsatisfied allowlist pattern would attest less than intended,
            // so it fails the selection; a dead denylist pattern excludes
            // nothing and attests exactly what was asked for.
            "an allowlist names what must be there" {
                selection(
                    AttesterSelectionMode::Allowlist,
                    &[prefix("HGX_IRoT_GPU_"), exact("VERA_CPU_0")],
                ) => unsatisfied(&[exact("VERA_CPU_0")]),
            }

            "a denylist pattern matching nothing excludes nothing" {
                selection(AttesterSelectionMode::Denylist, &[exact("VERA_CPU_0")]) => scheduled(&TRAY),
            }

            // An operator who wants nothing attested writes NONE.
            "a denylist that excludes everything fails" {
                selection(AttesterSelectionMode::Denylist, &[prefix("HGX_")])
                    => SelectionOutcome::PolicyMatchedNothing(
                        UnsatisfiedRequirement::DenylistExcludedEverything,
                    ),
            }
        );
    }

    #[test]
    fn selecting_nothing_is_not_one_outcome() {
        value_scenarios!(
            run = |(selection, eligible): (AttesterSelection, Vec<&str>)| selection.evaluate(&eligible);

            // The two reasons for selecting nothing have to stay apart: ALL
            // states no requirement, so a BMC with nothing to offer is not a
            // failure, while an allowlist on that same BMC went unsatisfied.
            "ALL over a BMC offering nothing eligible" {
                (selection(AttesterSelectionMode::All, &[]), vec![])
                    => SelectionOutcome::NoAttestersFound,
            }

            "an allowlist over that same BMC" {
                (
                    selection(AttesterSelectionMode::Allowlist, &[prefix("HGX_IRoT_GPU_")]),
                    vec![],
                ) => unsatisfied(&[prefix("HGX_IRoT_GPU_")]),
            }

            // A denylist asserts nothing about what must be there, so it lands
            // with ALL rather than with the allowlist. It excluded nothing.
            "a denylist over that same BMC" {
                (
                    selection(AttesterSelectionMode::Denylist, &[exact("HGX_BMC_0")]),
                    vec![],
                ) => SelectionOutcome::NoAttestersFound,
            }

            // `validate` refuses this, but the fields are public, so evaluate
            // must not call scheduling nothing a success.
            "an allowlist with no patterns selects nothing and fails" {
                (
                    selection(AttesterSelectionMode::Allowlist, &[]),
                    vec!["HGX_BMC_0"],
                ) => unsatisfied(&[]),
            }
        );
    }

    #[test]
    fn hardware_class_validation() {
        scenarios!(
            run = |class: &str| validate_hardware_class(class).map_err(drop);

            "a HwType variant name is a profile key" {
                "Gb200" => Yields(()),
            }

            // `any` is the one reserved class an operator may write.
            "the any fallback is writable" {
                ANY_HARDWARE_CLASS => Yields(()),
            }

            "an empty class is refused" {
                "" => Fails,
            }

            // The explorer's marker for hardware it could not classify. Such
            // hardware is covered through `any`, so a profile keyed to it
            // would never be read.
            "the unrecognized marker is refused" {
                "unrecognized" => Fails,
            }
        );
    }

    #[test]
    fn policy_document_rejects_a_foreign_schema_version() {
        // The server always writes the current version, so a foreign one can
        // only arrive from a build that is not this one.
        let mut document =
            AttestationPolicyDocument::new(selection(AttesterSelectionMode::All, &[]));
        assert!(document.validate().is_ok());

        document.schema_version = POLICY_SCHEMA_VERSION + 1;
        assert!(
            document.validate().is_err(),
            "a document from a later shape must not be read as this one"
        );
    }

    #[test]
    fn policy_document_round_trips_through_its_stored_shape() {
        // Operators read and write this JSON, so the spelling is a contract.
        let document = AttestationPolicyDocument::new(selection(
            AttesterSelectionMode::Allowlist,
            &[prefix("HGX_IRoT_GPU_"), exact("VERA_CPU_0")],
        ));

        let stored = serde_json::to_value(&document).expect("serializes");
        assert_eq!(
            stored,
            serde_json::json!({
                "schema_version": 1,
                "selection": {
                    "mode": "ALLOWLIST",
                    "component_ids": [
                        { "prefix": "HGX_IRoT_GPU_" },
                        { "exact": "VERA_CPU_0" },
                    ],
                },
            })
        );

        let read_back: AttestationPolicyDocument =
            serde_json::from_value(stored).expect("deserializes");
        assert_eq!(read_back, document);
    }

    #[test]
    fn mode_spellings_are_stable() {
        // Stored documents are keyed to these strings.
        value_scenarios!(
            run = |mode| serde_json::to_value(mode).expect("serializes");
            "none" { AttesterSelectionMode::None => serde_json::json!("NONE"), }
            "all" { AttesterSelectionMode::All => serde_json::json!("ALL"), }
            "allowlist" { AttesterSelectionMode::Allowlist => serde_json::json!("ALLOWLIST"), }
            "denylist" { AttesterSelectionMode::Denylist => serde_json::json!("DENYLIST"), }
        );
    }

    #[test]
    fn a_document_this_build_does_not_understand_is_refused() {
        // Silently dropping a key would read a future policy as a weaker one.
        value_scenarios!(
            run = |json| serde_json::from_str::<AttestationPolicyDocument>(json).is_err();

            "an unknown top-level key" {
                r#"{"schema_version":1,"selection":{"mode":"ALL"},"extra":true}"# => true,
            }

            "an unknown selection key" {
                r#"{"schema_version":1,"selection":{"mode":"ALL","extra":true}}"# => true,
            }

            "an unknown pattern kind" {
                r#"{"schema_version":1,"selection":{"mode":"ALLOWLIST","component_ids":[{"glob":"HGX_*"}]}}"# => true,
            }

            "the documented shape still reads" {
                r#"{"schema_version":1,"selection":{"mode":"ALL","component_ids":[]}}"# => false,
            }
        );
    }
}

#[cfg(test)]
mod test {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;
    use crate::attestation::spdm::{
        DeviceType, SpdmAttestationState, SpdmDeviceAttestationDetails, SpdmObjectId,
    };

    // A valid serialized MachineId, reused across rows.
    const VALID_MACHINE_ID: &str = "fm100htv4fu8fpktl0e0qrg4dl58g2bc2g7naq0l6c15ruc22po1i5rfsq0";

    fn machine_id() -> MachineId {
        VALID_MACHINE_ID.parse().expect("valid machine id")
    }

    #[test]
    fn spdm_object_id_round_trips() {
        let spdm_object_id = SpdmObjectId(machine_id(), "Device-1".to_string());

        let expected_str = format!("{VALID_MACHINE_ID},Device-1");
        assert_eq!(expected_str, spdm_object_id.to_string());

        let parsed_object_id: SpdmObjectId = spdm_object_id.to_string().parse().unwrap();
        assert_eq!(parsed_object_id, spdm_object_id);
    }

    #[test]
    fn spdm_object_id_display() {
        value_scenarios!(
            run = |id| id.to_string();
            "simple device id" {
                SpdmObjectId(machine_id(), "Device-1".to_string()) => format!("{VALID_MACHINE_ID},Device-1"),
            }

            "empty device id" {
                SpdmObjectId(machine_id(), String::new()) => format!("{VALID_MACHINE_ID},"),
            }

            "device id with internal comma" {
                SpdmObjectId(machine_id(), "a,b".to_string()) => format!("{VALID_MACHINE_ID},a,b"),
            }

            "device id with spaces" {
                SpdmObjectId(machine_id(), "HGX IRoT GPU 0".to_string()) => format!("{VALID_MACHINE_ID},HGX IRoT GPU 0"),
            }
        );
    }

    #[test]
    fn spdm_object_id_from_str() {
        // SpdmObjectIdParseError has no PartialEq, so use Fails (+ map_err(drop)).
        check_cases(
            [
                Case {
                    scenario: "valid two parts",
                    input: format!("{VALID_MACHINE_ID},Device-1"),
                    expect: Yields(SpdmObjectId(machine_id(), "Device-1".to_string())),
                },
                Case {
                    scenario: "valid with empty device id",
                    input: format!("{VALID_MACHINE_ID},"),
                    expect: Yields(SpdmObjectId(machine_id(), String::new())),
                },
                Case {
                    scenario: "no comma is wrong format",
                    input: VALID_MACHINE_ID.to_string(),
                    expect: Fails,
                },
                Case {
                    scenario: "empty string is wrong format",
                    input: String::new(),
                    expect: Fails,
                },
                Case {
                    scenario: "three parts is wrong format",
                    input: format!("{VALID_MACHINE_ID},Device-1,extra"),
                    expect: Fails,
                },
                Case {
                    scenario: "only a comma is wrong format",
                    input: ",".to_string(),
                    // two parts ("" and ""), but the first fails to parse as MachineId
                    expect: Fails,
                },
                Case {
                    scenario: "bad machine id",
                    input: "not-a-machine-id,Device-1".to_string(),
                    expect: Fails,
                },
            ],
            |s| s.parse::<SpdmObjectId>().map_err(drop),
        );
    }

    #[test]
    fn device_type_from_str() {
        // SpdmHandlerError is PartialEq, but from_str never errors — it always
        // classifies. Use the Display name as the observable, pure result.
        scenarios!(
            run = |s| {
                Ok::<_, ()>(format!(
                    "{:?}",
                    s.parse::<DeviceType>().expect("never errors")
                ))
            };
            "gpu token present" {
                "HGX_IRoT_GPU_0" => Yields("Gpu".to_string()),
            }

            "gpu token bare" {
                "GPU" => Yields("Gpu".to_string()),
            }

            "cx7 token present" {
                "HGX_ERoT_CX7_1" => Yields("Cx7".to_string()),
            }

            "cx7 token bare" {
                "CX7" => Yields("Cx7".to_string()),
            }

            "gpu wins when both present (checked first)" {
                "GPU_CX7" => Yields("Gpu".to_string()),
            }

            "cpu is unknown" {
                "HGX_ERoT_CPU_0" => Yields("Unknown".to_string()),
            }

            "bmc is unknown" {
                "BMC" => Yields("Unknown".to_string()),
            }

            "empty is unknown" {
                "" => Yields("Unknown".to_string()),
            }

            "lowercase gpu does not match (case sensitive)" {
                "gpu" => Yields("Unknown".to_string()),
            }

            "lowercase cx7 does not match (case sensitive)" {
                "cx7" => Yields("Unknown".to_string()),
            }
        );
    }

    fn details_with_state(state: SpdmAttestationState) -> SpdmDeviceAttestationDetails {
        let now = Utc::now();
        SpdmDeviceAttestationDetails {
            machine_id: machine_id(),
            device_id: "GPU_0".to_string(),
            state,
            started_at: now,
            cancelled_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn get_failure_cause() {
        value_scenarios!(
            run = |details| details.get_failure_cause();
            "failed state yields a cause naming device and reason" {
                details_with_state(SpdmAttestationState::Failed(
                    "signature mismatch".to_string(),
                )) => Some("Device: GPU_0, failed reason: signature mismatch".to_string()),
            }

            "failed with empty reason" {
                details_with_state(SpdmAttestationState::Failed(String::new())) => Some("Device: GPU_0, failed reason: ".to_string()),
            }

            "passed state has no cause" {
                details_with_state(SpdmAttestationState::Passed) => None,
            }

            "cancelled state has no cause" {
                details_with_state(SpdmAttestationState::Cancelled) => None,
            }

            "fetch metadata state has no cause" {
                details_with_state(SpdmAttestationState::FetchMetadata) => None,
            }

            "fetch certificate state has no cause" {
                details_with_state(SpdmAttestationState::FetchCertificate) => None,
            }

            "trigger evidence collection state has no cause" {
                details_with_state(SpdmAttestationState::TriggerEvidenceCollection {
                    retry_count: 0,
                }) => None,
            }

            "poll evidence collection state has no cause" {
                details_with_state(SpdmAttestationState::PollEvidenceCollection {
                    task_id: "t1".to_string(),
                    retry_count: 2,
                }) => None,
            }

            "nras verification state has no cause" {
                details_with_state(SpdmAttestationState::NrasVerification) => None,
            }

            "apply appraisal policy state has no cause" {
                details_with_state(SpdmAttestationState::ApplyAppraisalPolicy) => None,
            }
        );
    }
}
