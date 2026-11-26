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

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct InventoryId(String);

impl AsRef<str> for InventoryId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for InventoryId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for InventoryId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for InventoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_ref())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct EpochId(String);

impl AsRef<str> for EpochId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for EpochId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for EpochId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for EpochId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_ref())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct MatId(String);

impl AsRef<str> for MatId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for MatId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MatId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for MatId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_ref())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct MachineId(String);

impl AsRef<str> for MachineId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for MachineId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MachineId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_ref())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct Generation(u64);

impl Generation {
    pub const INITIAL: Self = Self(1);

    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl From<u64> for Generation {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Guid(u64);

impl fmt::Display for Guid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for Guid {
    type Err = InvalidGuid;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value.strip_prefix("0x").unwrap_or(value);
        u64::from_str_radix(hex, 16)
            .map(Self)
            .map_err(|_| InvalidGuid(value.to_string()))
    }
}

impl From<u64> for Guid {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<[u8; 8]> for Guid {
    fn from(bytes: [u8; 8]) -> Self {
        Self(u64::from_be_bytes(bytes))
    }
}

impl Serialize for Guid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Guid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct GuidVisitor;

        impl<'de> serde::de::Visitor<'de> for GuidVisitor {
            type Value = Result<Guid, InvalidGuid>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(value.parse())
            }

            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                let value = std::str::from_utf8(value)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Bytes(value), &self))?;
                self.visit_str(value)
            }
        }

        deserializer
            .deserialize_string(GuidVisitor)?
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid InfiniBand GUID {0}")]
pub struct InvalidGuid(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InventoryLease(u64);

impl InventoryLease {
    pub(crate) const LOCAL: Self = Self(0);
    pub(crate) const FIRST_REMOTE: Self = Self(1);

    pub(crate) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InventorySnapshot {
    pub inventory_id: InventoryId,
    pub epoch_id: EpochId,
    pub generation: Generation,
    #[serde(rename = "machines")]
    pub machines: Vec<InventoryMachine>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InventoryMachine {
    pub mat_id: MatId,
    #[serde(default)]
    pub machine_id: Option<MachineId>,
    #[serde(default)]
    pub infiniband_ports: Option<Vec<InventoryPort>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InventoryPort {
    pub guid: Guid,
    pub state: InfinibandPortState,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InfinibandPortState {
    Active,
    Down,
}

impl InfinibandPortState {
    pub(crate) fn physical_state(self) -> &'static str {
        match self {
            Self::Active => "Link Up",
            Self::Down => "Link Down",
        }
    }

    pub(crate) fn logical_state(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Down => "Down",
        }
    }
}

/// Supplies inventory without HTTP when the mock is embedded into another process.
///
/// Machine-a-tron implements this trait using its control state. The standalone UFM mock does
/// not have a local provider and obtains inventory from configured HTTP sources instead.
pub trait InventoryProvider: Send + Sync {
    fn inventory_snapshot(&self) -> InventorySnapshot;
}

#[cfg(test)]
mod serde_input_tests {
    use super::*;

    #[test]
    fn guid_accepts_string_and_byte_inputs() {
        use serde::de::value::{
            BorrowedStrDeserializer, BytesDeserializer, Error, StrDeserializer, StringDeserializer,
        };

        let input = "0x1";
        let cases = [
            (
                "borrowed string",
                Guid::deserialize(BorrowedStrDeserializer::<Error>::new(input)),
            ),
            (
                "transient string",
                Guid::deserialize(StrDeserializer::<Error>::new(input)),
            ),
            (
                "owned string",
                Guid::deserialize(StringDeserializer::<Error>::new(input.to_string())),
            ),
            (
                "bytes",
                Guid::deserialize(BytesDeserializer::<Error>::new(input.as_bytes())),
            ),
        ];
        for (scenario, result) in cases {
            let value = result.expect(scenario);
            assert_eq!(value, Guid::from(1), "{scenario}");
        }

        let error = Guid::deserialize(BytesDeserializer::<Error>::new(&[0xff]))
            .expect_err("invalid UTF-8 is rejected");
        assert_eq!(
            error.to_string(),
            "invalid value: byte array, expected a string",
        );
    }
}
