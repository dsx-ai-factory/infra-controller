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
        // job id
        pub job_id: uuid::Uuid,
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
        #[error("the object ID must have 3 parts but not as should be {0:?}")]
        WrongFormat(String),
        #[error("the machine ID parsing failed: {0}")]
        MachineIdParsingFailed(#[from] carbide_uuid::machine::MachineIdParseError),
        #[error("the job ID parsing failed: {0}")]
        JobIdParsingFailed(#[from] uuid::Error),
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, FromRow)]
    pub struct SpdmObjectId(pub MachineId, pub String, pub uuid::Uuid);

    impl FromStr for SpdmObjectId {
        type Err = SpdmObjectIdParseError;
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let values = s.split(',').collect_vec();
            if values.len() != 3 {
                return Err(SpdmObjectIdParseError::WrongFormat(s.to_string()));
            }

            Ok(Self(
                values[0].parse().map_err(SpdmObjectIdParseError::from)?,
                values[1].to_string(),
                uuid::Uuid::parse_str(values[2])?,
            ))
        }
    }

    impl Display for SpdmObjectId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{},{},{}", self.0, self.1, self.2)
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
                job_id: row.try_get("job_id")?,
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
    const JOB_ID: uuid::Uuid = uuid::uuid!("d2697720-9e96-4f9a-8183-e7f7feb98327");
    const JOB_ID_STR: &str = "d2697720-9e96-4f9a-8183-e7f7feb98327";

    fn machine_id() -> MachineId {
        VALID_MACHINE_ID.parse().expect("valid machine id")
    }

    #[test]
    fn spdm_object_id_round_trips() {
        let spdm_object_id = SpdmObjectId(machine_id(), "Device-1".to_string(), JOB_ID);

        let expected_str = format!("{VALID_MACHINE_ID},Device-1,{JOB_ID_STR}");
        assert_eq!(expected_str, spdm_object_id.to_string());

        let parsed_object_id: SpdmObjectId = spdm_object_id.to_string().parse().unwrap();
        assert_eq!(parsed_object_id, spdm_object_id);
    }

    #[test]
    fn spdm_object_id_display() {
        value_scenarios!(
            run = |id| id.to_string();
            "simple device id" {
                SpdmObjectId(machine_id(), "Device-1".to_string(), JOB_ID) => format!("{VALID_MACHINE_ID},Device-1,{JOB_ID_STR}"),
            }

            "empty device id" {
                SpdmObjectId(machine_id(), String::new(), JOB_ID) => format!("{VALID_MACHINE_ID},,{JOB_ID_STR}"),
            }

            "device id with internal comma" {
                SpdmObjectId(machine_id(), "a,b".to_string(), JOB_ID) => format!("{VALID_MACHINE_ID},a,b,{JOB_ID_STR}"),
            }

            "device id with spaces" {
                SpdmObjectId(machine_id(), "HGX IRoT GPU 0".to_string(), JOB_ID) => format!("{VALID_MACHINE_ID},HGX IRoT GPU 0,{JOB_ID_STR}"),
            }
        );
    }

    #[test]
    fn spdm_object_id_from_str() {
        // SpdmObjectIdParseError has no PartialEq, so use Fails (+ map_err(drop)).
        check_cases(
            [
                Case {
                    scenario: "valid three parts",
                    input: format!("{VALID_MACHINE_ID},Device-1,{JOB_ID_STR}"),
                    expect: Yields(SpdmObjectId(machine_id(), "Device-1".to_string(), JOB_ID)),
                },
                Case {
                    scenario: "valid with empty device id",
                    input: format!("{VALID_MACHINE_ID},,{JOB_ID_STR}"),
                    expect: Yields(SpdmObjectId(machine_id(), String::new(), JOB_ID)),
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
                    scenario: "two parts is wrong format",
                    input: format!("{VALID_MACHINE_ID},Device-1"),
                    expect: Fails,
                },
                Case {
                    scenario: "four parts is wrong format",
                    input: format!("{VALID_MACHINE_ID},Device-1,{JOB_ID_STR},extra"),
                    expect: Fails,
                },
                Case {
                    scenario: "only commas is wrong format",
                    input: ",,".to_string(),
                    // Three parts, but the first fails to parse as MachineId.
                    expect: Fails,
                },
                Case {
                    scenario: "bad machine id",
                    input: format!("not-a-machine-id,Device-1,{JOB_ID_STR}"),
                    expect: Fails,
                },
                Case {
                    scenario: "bad job id",
                    input: format!("{VALID_MACHINE_ID},Device-1,not-a-job-id"),
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
