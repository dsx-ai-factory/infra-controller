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

//! Stable device identities shared by inventory selection and mutation checks.

use std::fmt;

use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use super::info::MlxDeviceInfo;

/// `MlxDeviceSelector` selects a device by its reported base MAC or base GUID.
/// Matching does not establish reset completion or flash eligibility.
/// Its externally tagged JSON is persisted by Core and supervisor journals and
/// contributes to request fingerprints. Changing `BaseMac`, `BaseGuid`, or their
/// value encodings requires migrating stored selectors and fingerprints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MlxDeviceSelector {
    /// Exact reported base MAC, which must be nonzero and unicast.
    #[serde(rename = "BaseMac")]
    Mac(MacAddress),
    /// Exact reported base GUID, preserved as an opaque string. Requires 1–256
    /// UTF-8 bytes without surrounding whitespace or controls; `--` and `N/A`
    /// are absence placeholders and cannot select a device.
    #[serde(rename = "BaseGuid")]
    Guid(String),
}

impl fmt::Display for MlxDeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mac(mac_address) => write!(f, "base MAC {mac_address}"),
            Self::Guid(guid) => write!(f, "base GUID {guid}"),
        }
    }
}

/// `MlxDeviceSelectorError` distinguishes invalid identities from missing or
/// ambiguous matches without depending on a controller or transport error type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MlxDeviceSelectorError {
    /// The wire message did not select a MAC or GUID identity.
    MissingIdentity,
    /// The supplied MAC did not use one of the supported exact formats.
    MalformedMac,
    /// The typed MAC is zero or multicast and cannot identify one device.
    InvalidMac,
    /// The GUID is empty, oversized, padded, contains controls, or is a placeholder.
    InvalidGuid,
    /// No observed device has the requested identity.
    NotFound(MlxDeviceSelector),
    /// More than one observed device has the requested identity.
    Ambiguous(MlxDeviceSelector),
}

impl fmt::Display for MlxDeviceSelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingIdentity => write!(f, "card selector identity is required"),
            Self::MalformedMac => write!(f, "card selector base MAC is malformed"),
            Self::InvalidMac => write!(f, "card selector base MAC must be nonzero and unicast"),
            Self::InvalidGuid => write!(
                f,
                "card selector base GUID must contain 1-256 UTF-8 bytes without surrounding whitespace or control characters, and cannot be -- or N/A"
            ),
            Self::NotFound(identity) => write!(
                f,
                "card selector {identity} does not match any observed device"
            ),
            Self::Ambiguous(identity) => write!(
                f,
                "card selector {identity} is ambiguous: matches multiple observed devices"
            ),
        }
    }
}

impl std::error::Error for MlxDeviceSelectorError {}

impl MlxDeviceSelector {
    /// `from_mac_str` parses 12 hexadecimal digits or six two-digit octets
    /// separated consistently by `:` or `-`, then rejects zero and multicast.
    /// Signed octets, padding, extra octets, and mixed separators are invalid.
    pub fn from_mac_str(mac_address: &str) -> Result<Self, MlxDeviceSelectorError> {
        // `MacAddress` accepts signed octets and ignores extra split fields.
        // Check the complete spelling before it becomes an identity.
        let bytes = mac_address.as_bytes();
        let valid_format = match bytes.len() {
            12 => bytes.iter().all(u8::is_ascii_hexdigit),
            17 => {
                matches!(bytes[2], b':' | b'-')
                    && bytes.iter().enumerate().all(|(index, byte)| {
                        if index % 3 == 2 {
                            *byte == bytes[2]
                        } else {
                            byte.is_ascii_hexdigit()
                        }
                    })
            }
            _ => false,
        };
        if !valid_format {
            return Err(MlxDeviceSelectorError::MalformedMac);
        }
        Self::Mac(
            mac_address
                .parse()
                .map_err(|_| MlxDeviceSelectorError::MalformedMac)?,
        )
        .validate()
    }

    /// `validate` rejects unusable identities without changing valid values.
    pub fn validate(self) -> Result<Self, MlxDeviceSelectorError> {
        match &self {
            Self::Mac(mac_address) => {
                let bytes = mac_address.bytes();
                if bytes == [0; 6] || bytes[0] & 1 != 0 {
                    return Err(MlxDeviceSelectorError::InvalidMac);
                }
            }
            Self::Guid(guid) => {
                if guid.is_empty()
                    || guid.len() > 256
                    || guid.trim() != guid.as_str()
                    || guid.chars().any(char::is_control)
                    || matches!(guid.as_str(), "--" | "N/A")
                {
                    return Err(MlxDeviceSelectorError::InvalidGuid);
                }
            }
        }
        Ok(self)
    }

    /// `matches` compares the selected identity literally with a device report.
    /// Missing identity fields do not match; GUID case and formatting matter.
    pub fn matches(&self, device: &MlxDeviceInfo) -> bool {
        match self {
            Self::Mac(mac_address) => device.base_mac.as_ref() == Some(mac_address),
            Self::Guid(guid) => device.base_guid.as_ref() == Some(guid),
        }
    }

    /// `device_index` returns the sole matching device's index in report order.
    /// No match or multiple matches return an error, so callers cannot silently
    /// select the first device when an identity is ambiguous.
    pub fn device_index(&self, devices: &[MlxDeviceInfo]) -> Result<usize, MlxDeviceSelectorError> {
        let mut matches = devices
            .iter()
            .enumerate()
            .filter(|(_, device)| self.matches(device));
        let Some((index, _)) = matches.next() else {
            return Err(MlxDeviceSelectorError::NotFound(self.clone()));
        };
        if matches.next().is_some() {
            return Err(MlxDeviceSelectorError::Ambiguous(self.clone()));
        }
        Ok(index)
    }
}
