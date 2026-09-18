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

use std::collections::HashSet;
use std::net::IpAddr;

use mac_address::MacAddress;
use model::metadata::Metadata;
use rpc::forge;
use uuid::Uuid;

use crate::CarbideError;

#[derive(Clone, Copy)]
pub(super) enum ExpectedComponent {
    Machine,
    PowerShelf,
    Switch,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum UpdateField {
    BmcUsername,
    BmcPassword,
    BmcIpAddress,
    BmcRetainCredentials,
    RackId,
    MetadataName,
    MetadataDescription,
    MetadataLabels,
    ChassisSerialNumber,
    FallbackDpuSerialNumbers,
    SkuId,
    IsDpfEnabled,
    DisableLockdown,
    DefaultPauseIngestionAndPoweron,
    DpuMode,
    BmcIpAllocation,
    HostNics,
    ShelfSerialNumber,
    SwitchSerialNumber,
    NvosMacAddresses,
    NvosUsername,
    NvosPassword,
    NvosIpAddress,
}

/// `UpdateMask` selects fields supported by the expected component PATCH RPCs.
/// Parsing rejects missing masks and unsupported paths, deduplicates fields,
/// and accepts empty masks. Callers separately validate credential-pair
/// selection and values before merging.
pub(super) struct UpdateMask(HashSet<UpdateField>);

impl UpdateMask {
    pub(super) fn parse(
        mask: Option<Vec<String>>,
        component: ExpectedComponent,
    ) -> Result<Self, CarbideError> {
        use ExpectedComponent::*;
        use UpdateField::*;

        let mask = mask
            .ok_or_else(|| CarbideError::InvalidArgument("update_mask is required".to_string()))?;
        let mut fields = HashSet::with_capacity(mask.len());
        for path in mask {
            let field = match (path.as_str(), component) {
                ("bmc_username", _) => BmcUsername,
                ("bmc_password", _) => BmcPassword,
                ("bmc_ip_address", _) => BmcIpAddress,
                ("bmc_retain_credentials", _) => BmcRetainCredentials,
                ("rack_id", _) => RackId,
                ("metadata.name", _) => MetadataName,
                ("metadata.description", _) => MetadataDescription,
                ("metadata.labels", _) => MetadataLabels,
                ("chassis_serial_number", Machine) => ChassisSerialNumber,
                ("fallback_dpu_serial_numbers", Machine) => FallbackDpuSerialNumbers,
                ("sku_id", Machine) => SkuId,
                ("is_dpf_enabled", Machine) => IsDpfEnabled,
                ("host_lifecycle_profile.disable_lockdown", Machine) => DisableLockdown,
                ("default_pause_ingestion_and_poweron", Machine) => DefaultPauseIngestionAndPoweron,
                ("dpu_mode", Machine) => DpuMode,
                ("bmc_ip_allocation", Machine) => BmcIpAllocation,
                ("host_nics", Machine) => HostNics,
                ("shelf_serial_number", PowerShelf) => ShelfSerialNumber,
                ("switch_serial_number", Switch) => SwitchSerialNumber,
                ("nvos_mac_addresses", Switch) => NvosMacAddresses,
                ("nvos_username", Switch) => NvosUsername,
                ("nvos_password", Switch) => NvosPassword,
                ("nvos_ip_address", Switch) => NvosIpAddress,
                _ => {
                    return Err(CarbideError::InvalidArgument(format!(
                        "unsupported expected-component update path: {path}"
                    )));
                }
            };
            fields.insert(field);
        }
        Ok(Self(fields))
    }

    pub(super) fn contains(&self, field: UpdateField) -> bool {
        self.0.contains(&field)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn validate_bmc_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), CarbideError> {
        self.validate_credentials_pair(
            "BMC",
            (UpdateField::BmcUsername, Some(username)),
            (UpdateField::BmcPassword, Some(password)),
        )
    }

    pub(super) fn validate_nvos_credentials(
        &self,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<(), CarbideError> {
        self.validate_credentials_pair(
            "NVOS",
            (UpdateField::NvosUsername, username),
            (UpdateField::NvosPassword, password),
        )
    }

    fn validate_credentials_pair(
        &self,
        kind: &str,
        (username_field, username): (UpdateField, Option<&str>),
        (password_field, password): (UpdateField, Option<&str>),
    ) -> Result<(), CarbideError> {
        match (self.contains(username_field), self.contains(password_field)) {
            (false, false) => Ok(()),
            (true, true)
                if username.is_some_and(|value| !value.is_empty())
                    && password.is_some_and(|value| !value.is_empty()) =>
            {
                Ok(())
            }
            (true, true) => Err(CarbideError::InvalidArgument(format!(
                "{kind} username and password must be present and nonempty"
            ))),
            _ => Err(CarbideError::InvalidArgument(format!(
                "{kind} username and password must be updated together"
            ))),
        }
    }

    pub(super) fn update_metadata(
        &self,
        patch: Option<forge::Metadata>,
        current: &mut Metadata,
    ) -> Result<(), CarbideError> {
        let patch = patch.unwrap_or_default();
        if self.contains(UpdateField::MetadataName) {
            current.name = patch.name;
        }
        if self.contains(UpdateField::MetadataDescription) {
            current.description = patch.description;
        }
        if self.contains(UpdateField::MetadataLabels) {
            let metadata: Metadata = forge::Metadata {
                labels: patch.labels,
                ..Default::default()
            }
            .try_into()?;
            current.labels = metadata.labels;
        }
        Ok(())
    }
}

pub(super) fn required_value<T>(value: Option<T>, field: &str) -> Result<T, CarbideError> {
    value.ok_or_else(|| CarbideError::InvalidArgument(format!("{field} is required when selected")))
}

pub(super) fn required_id(
    id: Option<rpc::common::Uuid>,
    field: &str,
) -> Result<Uuid, CarbideError> {
    let id = id.ok_or_else(|| CarbideError::InvalidArgument(format!("{field} is required")))?;
    Uuid::parse_str(&id.value)
        .map_err(|_| CarbideError::InvalidArgument(format!("invalid {field}")))
}

/// `validate_bmc_mac` treats an empty MAC as no identity assertion. A supplied
/// MAC must equal the stored identity; PATCH cannot change it.
pub(super) fn validate_bmc_mac(submitted: &str, current: MacAddress) -> Result<(), CarbideError> {
    if submitted.is_empty() {
        return Ok(());
    }
    let submitted: MacAddress = submitted.parse().map_err(CarbideError::from)?;
    if submitted != current {
        return Err(CarbideError::InvalidArgument(
            "expected-component patch cannot change BMC MAC address".to_string(),
        ));
    }
    Ok(())
}

/// `parse_bmc_ip` maps an empty string to a cleared address. Callers check
/// field selection and presence before using this explicit clear operation.
pub(super) fn parse_bmc_ip(address: &str) -> Result<Option<IpAddr>, CarbideError> {
    if address.is_empty() {
        return Ok(None);
    }
    address
        .parse()
        .map(Some)
        .map_err(|_| CarbideError::InvalidArgument("invalid bmc_ip_address".to_string()))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn bmc_mac_identity_cannot_change() {
        let current = MacAddress::new([2, 0, 0, 0, 0, 1]);
        assert!(matches!(
            validate_bmc_mac("02:00:00:00:00:02", current),
            Err(CarbideError::InvalidArgument(_))
        ));
    }

    #[test]
    fn masks_require_presence_and_component_supported_paths() {
        scenarios!(run = |(component, paths): (ExpectedComponent, Option<&[&str]>)| {
            UpdateMask::parse(paths.map(|paths| paths.iter().map(|path| (*path).to_string()).collect()), component).map(|_| ()).map_err(drop)
        };
            "empty mask is an explicit no-op" {
                (ExpectedComponent::Machine, Some(&[] as &[&str])) => Yields(()),
            }
            "omitted mask cannot imply replacement" {
                (ExpectedComponent::Machine, None) => Fails,
            }
            "supported component field" {
                (ExpectedComponent::Switch, Some(&["nvos_username"])) => Yields(()),
            }
            "other component field" {
                (ExpectedComponent::PowerShelf, Some(&["nvos_username"])) => Fails,
            }
            "whole-resource and immutable fields" {
                (ExpectedComponent::Machine, Some(&["*"])) => Fails,
                (ExpectedComponent::Machine, Some(&["id"])) => Fails,
            }
        );
    }

    #[test]
    fn credential_pairs_validate_presence_before_merge() {
        scenarios!(run = |(paths, username, password): (&[&str], Option<&str>, Option<&str>)| {
            let fields = UpdateMask::parse(Some(paths.iter().map(|path| (*path).to_string()).collect()), ExpectedComponent::Switch).unwrap();
            fields.validate_nvos_credentials(username, password).map_err(drop)
        };
            "unselected values are ignored" {
                (&[] as &[&str], Some("unselected"), None) => Yields(()),
            }
            "complete replacement" {
                (&["nvos_username", "nvos_password"], Some("user"), Some("pass")) => Yields(()),
            }
            "both fields must be selected even when both values exist" {
                (&["nvos_username"], Some("user"), Some("pass")) => Fails,
                (&["nvos_password"], Some("user"), Some("pass")) => Fails,
            }
            "selection cannot supply an absent value" {
                (&["nvos_username", "nvos_password"], None, Some("pass")) => Fails,
            }
            "empty credentials cannot remove a pair" {
                (&["nvos_username", "nvos_password"], Some(""), Some("pass")) => Fails,
                (&["nvos_username", "nvos_password"], Some("user"), Some("")) => Fails,
            }
        );
    }
}
