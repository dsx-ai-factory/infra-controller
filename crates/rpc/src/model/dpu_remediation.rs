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

use model::dpu_remediation::{
    AppliedRemediation, ApproveRemediation, DisableRemediation, EnableRemediation, NewRemediation,
    Remediation, RemediationApplicationStatus, RevokeRemediation,
};
use model::metadata::Metadata;

use crate as rpc;
use crate::errors::RpcDataConversionError;
use crate::forge::{
    ApproveRemediationRequest, CreateRemediationRequest, DisableRemediationRequest,
    EnableRemediationRequest, RevokeRemediationRequest,
};
use crate::model::RpcTryFrom;

impl TryFrom<rpc::forge::RemediationApplicationStatus> for RemediationApplicationStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: rpc::forge::RemediationApplicationStatus) -> Result<Self, Self::Error> {
        let metadata = status.metadata.map(Metadata::try_from).transpose()?;
        Ok(RemediationApplicationStatus {
            succeeded: status.succeeded,
            metadata,
        })
    }
}

// about 16KB file size, long enough for any reasonable script but small enough to make it
// almost impossible to stuff a binary in the DB, which is the point of the limit.
const MAXIMUM_SCRIPT_LENGTH: usize = 2 << 13;

impl RpcTryFrom<(CreateRemediationRequest, String)> for NewRemediation {
    type Error = RpcDataConversionError;

    fn rpc_try_from(value: (CreateRemediationRequest, String)) -> Result<Self, Self::Error> {
        let rpc_request = value.0;
        let author = value.1.into();

        let metadata = if let Some(metadata) = rpc_request.metadata {
            Some(Metadata::try_from(metadata)?)
        } else {
            None
        };
        let retries = if rpc_request.retries < 0 {
            return Err(RpcDataConversionError::InvalidArgument(String::from(
                "retries must be a positive integer or 0",
            )));
        } else {
            rpc_request.retries
        };

        let script = rpc_request.script.to_string();
        if script.len() > MAXIMUM_SCRIPT_LENGTH {
            return Err(RpcDataConversionError::InvalidArgument(format!(
                "script must not exceed length: {MAXIMUM_SCRIPT_LENGTH}"
            )));
        } else if script.is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "script cannot be empty".to_string(),
            ));
        }

        Ok(Self {
            script,
            metadata,
            retries,
            author,
        })
    }
}

impl From<Remediation> for rpc::forge::Remediation {
    fn from(value: Remediation) -> Self {
        Self {
            id: value.id.into(),
            metadata: value.metadata.map(|m| m.into()),
            creation_time: Some(value.creation_time.into()),
            script_author: value.author.to_string(),
            script_reviewed_by: value.reviewer.map(|r| r.to_string()),
            script: value.script,
            enabled: value.enabled,
            retries: value.retries,
        }
    }
}

impl From<Remediation> for rpc::forge::CreateRemediationResponse {
    fn from(value: Remediation) -> Self {
        rpc::forge::CreateRemediationResponse {
            remediation_id: value.id.into(),
        }
    }
}

impl From<AppliedRemediation> for rpc::forge::AppliedRemediation {
    fn from(value: AppliedRemediation) -> Self {
        let metadata = Metadata {
            labels: value.status,
            description: String::new(),
            name: String::new(),
        };
        Self {
            dpu_machine_id: Some(value.dpu_machine_id),
            remediation_id: Some(value.id),
            attempt: value.attempt,
            metadata: Some(metadata.into()),
            succeeded: value.succeeded,
            applied_time: Some(value.applied_time.into()),
        }
    }
}

impl RpcTryFrom<(ApproveRemediationRequest, String)> for ApproveRemediation {
    type Error = RpcDataConversionError;

    fn rpc_try_from(value: (ApproveRemediationRequest, String)) -> Result<Self, Self::Error> {
        let id = value
            .0
            .remediation_id
            .ok_or(RpcDataConversionError::MissingArgument(
                "Request must contain a remediation id.",
            ))?;
        let reviewer = value.1.into();

        Ok(Self { id, reviewer })
    }
}

impl RpcTryFrom<(RevokeRemediationRequest, String)> for RevokeRemediation {
    type Error = RpcDataConversionError;

    fn rpc_try_from(value: (RevokeRemediationRequest, String)) -> Result<Self, Self::Error> {
        let id = value
            .0
            .remediation_id
            .ok_or(RpcDataConversionError::MissingArgument(
                "Request must contain a remediation id.",
            ))?;
        let revoked_by = value.1;
        tracing::info!(
            remediation_id = %id,
            revoked_by = %revoked_by,
            "Remediation revoked",
        );

        Ok(Self { id })
    }
}

impl RpcTryFrom<(EnableRemediationRequest, String)> for EnableRemediation {
    type Error = RpcDataConversionError;

    fn rpc_try_from(value: (EnableRemediationRequest, String)) -> Result<Self, Self::Error> {
        let id = value
            .0
            .remediation_id
            .ok_or(RpcDataConversionError::MissingArgument(
                "Request must contain a remediation id.",
            ))?;
        let enabled_by = value.1;
        tracing::info!(
            remediation_id = %id,
            enabled_by = %enabled_by,
            "Remediation enabled",
        );

        Ok(Self { id })
    }
}

impl RpcTryFrom<(DisableRemediationRequest, String)> for DisableRemediation {
    type Error = RpcDataConversionError;

    fn rpc_try_from(value: (DisableRemediationRequest, String)) -> Result<Self, Self::Error> {
        let id = value
            .0
            .remediation_id
            .ok_or(RpcDataConversionError::MissingArgument(
                "Request must contain a remediation id.",
            ))?;
        let disabled_by = value.1;
        tracing::info!(
            remediation_id = %id,
            disabled_by = %disabled_by,
            "Remediation disabled",
        );

        Ok(Self { id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remediation_application_status_from_rpc_success_no_metadata() {
        let rpc_status = rpc::forge::RemediationApplicationStatus {
            succeeded: true,
            metadata: None,
        };
        let status = RemediationApplicationStatus::try_from(rpc_status).unwrap();
        assert!(status.succeeded);
        assert!(status.metadata.is_none());
    }

    #[test]
    fn remediation_application_status_from_rpc_with_metadata() {
        let rpc_status = rpc::forge::RemediationApplicationStatus {
            succeeded: false,
            metadata: Some(rpc::Metadata {
                name: "test".to_string(),
                description: "desc".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "status".to_string(),
                    value: Some("failed".to_string()),
                }],
            }),
        };
        let status = RemediationApplicationStatus::try_from(rpc_status).unwrap();
        assert!(!status.succeeded);
        let metadata = status.metadata.unwrap();
        assert_eq!(metadata.name, "test");
        assert_eq!(metadata.labels.get("status"), Some(&"failed".to_string()));
    }
}
