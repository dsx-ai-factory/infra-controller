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

/// Conversions for the operator-authored attestation profiles.
pub mod profile {
    use config_version::ConfigVersion;
    use model::attestation::profile::{
        AttestationPolicyDocument, AttestationProfile, AttesterSelection, AttesterSelectionMode,
        ComponentIdMatch, DeleteAttestationProfile, NewAttestationProfile,
        UpdateAttestationProfile, validate_hardware_class,
    };

    use crate as rpc;
    use crate::errors::RpcDataConversionError;

    fn parse_if_version_match(
        value: Option<String>,
    ) -> Result<Option<ConfigVersion>, RpcDataConversionError> {
        value
            .map(|version| {
                version
                    .parse()
                    .map_err(|_| RpcDataConversionError::InvalidConfigVersion(version))
            })
            .transpose()
    }

    fn parse_hardware_class(value: String) -> Result<String, RpcDataConversionError> {
        validate_hardware_class(&value)
            .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?;
        Ok(value)
    }

    /// Builds the stored policy from a request's selection, refusing anything
    /// the selection rules do not allow.
    fn parse_policy_document(
        selection: Option<rpc::forge::AttesterSelection>,
    ) -> Result<AttestationPolicyDocument, RpcDataConversionError> {
        let selection: AttesterSelection = selection
            .ok_or(RpcDataConversionError::MissingArgument("selection"))?
            .try_into()?;
        let policy_document = AttestationPolicyDocument::new(selection);
        policy_document.validate().map_err(|error| {
            RpcDataConversionError::InvalidArgument(format!(
                "attestation profile selection is not valid: {error}"
            ))
        })?;
        Ok(policy_document)
    }

    impl TryFrom<rpc::forge::ComponentIdMatch> for ComponentIdMatch {
        type Error = RpcDataConversionError;

        fn try_from(value: rpc::forge::ComponentIdMatch) -> Result<Self, Self::Error> {
            match value.pattern {
                Some(rpc::forge::component_id_match::Pattern::Exact(id)) => Ok(Self::Exact(id)),
                Some(rpc::forge::component_id_match::Pattern::Prefix(prefix)) => {
                    Ok(Self::Prefix(prefix))
                }
                None => Err(RpcDataConversionError::MissingArgument("pattern")),
            }
        }
    }

    impl From<ComponentIdMatch> for rpc::forge::ComponentIdMatch {
        fn from(value: ComponentIdMatch) -> Self {
            let pattern = match value {
                ComponentIdMatch::Exact(id) => rpc::forge::component_id_match::Pattern::Exact(id),
                ComponentIdMatch::Prefix(prefix) => {
                    rpc::forge::component_id_match::Pattern::Prefix(prefix)
                }
            };
            Self {
                pattern: Some(pattern),
            }
        }
    }

    impl From<AttesterSelectionMode> for rpc::forge::AttesterSelectionMode {
        fn from(value: AttesterSelectionMode) -> Self {
            match value {
                AttesterSelectionMode::None => Self::None,
                AttesterSelectionMode::All => Self::All,
                AttesterSelectionMode::Allowlist => Self::Allowlist,
                AttesterSelectionMode::Denylist => Self::Denylist,
            }
        }
    }

    impl From<rpc::forge::AttesterSelectionMode> for AttesterSelectionMode {
        fn from(value: rpc::forge::AttesterSelectionMode) -> Self {
            match value {
                rpc::forge::AttesterSelectionMode::None => Self::None,
                rpc::forge::AttesterSelectionMode::All => Self::All,
                rpc::forge::AttesterSelectionMode::Allowlist => Self::Allowlist,
                rpc::forge::AttesterSelectionMode::Denylist => Self::Denylist,
            }
        }
    }

    impl TryFrom<rpc::forge::AttesterSelection> for AttesterSelection {
        type Error = RpcDataConversionError;

        fn try_from(value: rpc::forge::AttesterSelection) -> Result<Self, Self::Error> {
            // `mode` is `optional` in the proto precisely so an omitted mode is
            // distinguishable from the zero value, which would silently mean
            // ALLOWLIST.
            let mode = value
                .mode
                .ok_or(RpcDataConversionError::MissingArgument("mode"))?;
            let mode = rpc::forge::AttesterSelectionMode::try_from(mode)
                .map_err(|_| RpcDataConversionError::InvalidArgument(format!("mode {mode}")))?;

            Ok(Self {
                mode: mode.into(),
                component_ids: value
                    .component_ids
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<_, _>>()?,
            })
        }
    }

    impl From<AttesterSelection> for rpc::forge::AttesterSelection {
        fn from(value: AttesterSelection) -> Self {
            Self {
                mode: Some(rpc::forge::AttesterSelectionMode::from(value.mode).into()),
                component_ids: value.component_ids.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl From<AttestationProfile> for rpc::forge::AttestationProfile {
        fn from(value: AttestationProfile) -> Self {
            Self {
                hardware_class: value.hardware_class,
                version: value.version.to_string(),
                selection: Some(value.policy_document.selection.into()),
                updated_at: Some(value.updated_at.into()),
                updated_by: value.updated_by,
            }
        }
    }

    impl TryFrom<rpc::forge::CreateAttestationProfileRequest> for NewAttestationProfile {
        type Error = RpcDataConversionError;

        fn try_from(
            value: rpc::forge::CreateAttestationProfileRequest,
        ) -> Result<Self, Self::Error> {
            Ok(Self {
                hardware_class: parse_hardware_class(value.hardware_class)?,
                policy_document: parse_policy_document(value.selection)?,
            })
        }
    }

    impl TryFrom<rpc::forge::UpdateAttestationProfileRequest> for UpdateAttestationProfile {
        type Error = RpcDataConversionError;

        fn try_from(
            value: rpc::forge::UpdateAttestationProfileRequest,
        ) -> Result<Self, Self::Error> {
            Ok(Self {
                hardware_class: parse_hardware_class(value.hardware_class)?,
                policy_document: parse_policy_document(value.selection)?,
                if_version_match: parse_if_version_match(value.if_version_match)?,
            })
        }
    }

    impl TryFrom<rpc::forge::DeleteAttestationProfileRequest> for DeleteAttestationProfile {
        type Error = RpcDataConversionError;

        fn try_from(
            value: rpc::forge::DeleteAttestationProfileRequest,
        ) -> Result<Self, Self::Error> {
            Ok(Self {
                hardware_class: parse_hardware_class(value.hardware_class)?,
                if_version_match: parse_if_version_match(value.if_version_match)?,
            })
        }
    }
}

/// Model for SPDM attestation via Redfish
pub mod spdm {

    use model::attestation::spdm::{SpdmAttestationStatus, SpdmDeviceAttestationDetails};

    use crate as rpc;

    impl From<SpdmDeviceAttestationDetails> for rpc::forge::SpdmAttestationDetails {
        fn from(value: SpdmDeviceAttestationDetails) -> Self {
            rpc::forge::SpdmAttestationDetails {
                machine_id: Some(value.machine_id),
                completed_at: value.completed_at.map(|x| x.into()),
                started_at: Some(value.started_at.into()),
                cancelled_at: value.cancelled_at.map(|x| x.into()),
                state: format!("{:?}", value.state),
                device_id: value.device_id,
            }
        }
    }

    impl From<SpdmAttestationStatus> for rpc::forge::SpdmAttestationStatus {
        fn from(value: SpdmAttestationStatus) -> Self {
            match value {
                SpdmAttestationStatus::InProgress => Self::SpdmAttInProgress,
                SpdmAttestationStatus::Cancelled => Self::SpdmAttCancelled,
                SpdmAttestationStatus::Passed => Self::SpdmAttPassed,
                SpdmAttestationStatus::Failed => Self::SpdmAttFailed,
            }
        }
    }
}
