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
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;

use prost::DecodeError;
use prost::bytes::{Buf, BufMut};
use prost::encoding::{DecodeContext, WireType};
use serde::{Deserialize, Serialize};
#[cfg(feature = "sqlx")]
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgHasArrayType, PgTypeInfo},
    {Database, Postgres, Row},
};

use crate::machine::{MachineId, MachineIdParseError};
use crate::power_shelf::{PowerShelfId, PowerShelfIdParseError};
use crate::switch::{SwitchId, SwitchIdParseError};

#[derive(Copy, Clone, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub enum DeviceId {
    Machine(MachineId),
    Switch(SwitchId),
    PowerShelf(PowerShelfId),
}

// Implement [`prost::Message`] manually so that we can be wire-compatible with the
// `.common.DeviceId` protobuf message, which is what we actually serialize. Do this by
// constructing a `proto::DeviceId` and delegate all [`prost::Message`] methods to it.
impl prost::Message for DeviceId {
    fn encode_raw(&self, buf: &mut impl BufMut)
    where
        Self: Sized,
    {
        proto::DeviceId::from(*self).encode_raw(buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError>
    where
        Self: Sized,
    {
        let mut message = proto::DeviceId::from(*self);
        message.merge_field(tag, wire_type, buf, ctx)?;
        *self = message.try_into()?;
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        proto::DeviceId::from(*self).encoded_len()
    }

    #[allow(deprecated)]
    fn clear(&mut self) {
        *self = DeviceId::default();
    }
}

mod proto {
    /// Private generated-equivalent representation of the `.common.DeviceId`
    /// message:
    ///
    /// ```ignore
    /// message DeviceId {
    ///     oneof value {
    ///         MachineId machine_id = 1;
    ///         SwitchId switch_id = 2;
    ///         PowerShelfId power_shelf_id = 3;
    ///     }
    /// }
    /// ```
    ///
    /// This lets RPC code use [`super::DeviceId`] directly while retaining the
    /// nested-message wire representation required by the oneof.
    #[derive(Clone, prost::Message)]
    pub(super) struct DeviceId {
        #[prost(oneof = "device_id::Value", tags = "1, 2, 3")]
        value: Option<device_id::Value>,
    }

    mod device_id {
        #[derive(Clone, prost::Oneof)]
        pub(super) enum Value {
            #[prost(message, tag = "1")]
            Machine(crate::machine::MachineId),
            #[prost(message, tag = "2")]
            Switch(crate::switch::SwitchId),
            #[prost(message, tag = "3")]
            PowerShelf(crate::power_shelf::PowerShelfId),
        }
    }

    impl From<super::DeviceId> for DeviceId {
        fn from(value: super::DeviceId) -> Self {
            use device_id::Value;

            Self {
                value: Some(match value {
                    super::DeviceId::Machine(id) => Value::Machine(id),
                    super::DeviceId::Switch(id) => Value::Switch(id),
                    super::DeviceId::PowerShelf(id) => Value::PowerShelf(id),
                }),
            }
        }
    }

    impl TryFrom<DeviceId> for super::DeviceId {
        type Error = prost::DecodeError;

        fn try_from(value: DeviceId) -> Result<Self, Self::Error> {
            match value.value {
                Some(device_id::Value::Machine(id)) => Ok(Self::Machine(id)),
                Some(device_id::Value::Switch(id)) => Ok(Self::Switch(id)),
                Some(device_id::Value::PowerShelf(id)) => Ok(Self::PowerShelf(id)),
                None => Err(missing_value_error()),
            }
        }
    }

    fn missing_value_error() -> prost::DecodeError {
        #[allow(deprecated)]
        prost::DecodeError::new("DeviceId value is missing")
    }
}

impl Default for DeviceId {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::default()
    }
}

impl Debug for DeviceId {
    // The derived Debug implementation is messy, just output the string representation even when
    // debugging.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

// Make DeviceId bindable directly into a sqlx query
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for DeviceId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> sqlx::Decode<'r, DB> for DeviceId
where
    DB: sqlx::Database,
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_id: String = String::decode(value)?;
        Ok(DeviceId::from_str(&str_id).map_err(|e| sqlx::Error::Decode(Box::new(e)))?)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for DeviceId {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let id: DeviceId = row.try_get(0)?;
        Ok(id)
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for DeviceId
where
    DB: sqlx::Database,
    String: sqlx::Type<DB>,
{
    fn type_info() -> <DB as sqlx::Database>::TypeInfo {
        String::type_info()
    }

    fn compatible(ty: &DB::TypeInfo) -> bool {
        String::compatible(ty)
    }
}

#[cfg(feature = "sqlx")]
impl PgHasArrayType for DeviceId {
    fn array_type_info() -> PgTypeInfo {
        <&str as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <&str as PgHasArrayType>::array_compatible(ty)
    }
}

impl DeviceId {
    /// Note: Never use this! Tonic's codegen requires all types to implement Default, but there is
    /// no logical reason to construct a "default" DeviceId in real code, so we simply construct a
    /// bogus one here.
    #[allow(clippy::should_implement_trait)]
    #[deprecated(
        note = "Do not use `DeviceId::default()` directly; only implemented for prost interop"
    )]
    pub fn default() -> Self {
        #[allow(deprecated)]
        Self::Machine(MachineId::default())
    }

    /// The kind of device this id names, independent of the id value itself.
    /// Useful for kind-labeled messages, since [`DeviceId`]'s own [`Display`]
    /// prints only the bare id without its kind.
    pub fn kind(&self) -> DeviceKind {
        match self {
            DeviceId::Machine(_) => DeviceKind::Machine,
            DeviceId::Switch(_) => DeviceKind::Switch,
            DeviceId::PowerShelf(_) => DeviceKind::PowerShelf,
        }
    }
}

/// The kind of a [`DeviceId`], without its id value. Its [`Display`] renders a
/// human-readable label (e.g. for error messages).
#[derive(Copy, Clone, Eq, Hash, PartialEq, Ord, PartialOrd, Debug)]
pub enum DeviceKind {
    Machine,
    Switch,
    PowerShelf,
}

impl Display for DeviceKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DeviceKind::Machine => write!(f, "machine"),
            DeviceKind::Switch => write!(f, "switch"),
            DeviceKind::PowerShelf => write!(f, "power-shelf"),
        }
    }
}

impl From<MachineId> for DeviceId {
    fn from(value: MachineId) -> Self {
        Self::Machine(value)
    }
}

impl From<SwitchId> for DeviceId {
    fn from(value: SwitchId) -> Self {
        Self::Switch(value)
    }
}

impl From<PowerShelfId> for DeviceId {
    fn from(value: PowerShelfId) -> Self {
        Self::PowerShelf(value)
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceId::Machine(m) => <MachineId as Display>::fmt(m, f),
            DeviceId::Switch(s) => <SwitchId as Display>::fmt(s, f),
            DeviceId::PowerShelf(ps) => <PowerShelfId as Display>::fmt(ps, f),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum DeviceIdParseError {
    #[error("invalid machine id: {0}")]
    Machine(#[from] MachineIdParseError),
    #[error("invalid switch id: {0}")]
    Switch(#[from] SwitchIdParseError),
    #[error("invalid power shelf id: {0}")]
    PowerShelf(#[from] PowerShelfIdParseError),
    #[error("unable to determine device type from id: {0}")]
    UnknownId(String),
}

impl FromStr for DeviceId {
    type Err = DeviceIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if MachineId::is_matching_prefix(s) {
            Ok(Self::Machine(MachineId::from_str(s)?))
        } else if SwitchId::is_matching_prefix(s) {
            Ok(Self::Switch(SwitchId::from_str(s)?))
        } else if PowerShelfId::is_matching_prefix(s) {
            Ok(Self::PowerShelf(PowerShelfId::from_str(s)?))
        } else {
            Err(DeviceIdParseError::UnknownId(s.to_string()))
        }
    }
}

impl Serialize for DeviceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D>(deserializer: D) -> Result<DeviceId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let id = DeviceId::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use prost::Message;

    use super::*;

    const MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
    const SWITCH_ID: &str = "sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
    const POWER_SHELF_ID: &str = "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

    #[derive(Clone, PartialEq, Message)]
    struct ReferenceDeviceId {
        #[prost(oneof = "reference_device_id::Value", tags = "1, 2, 3")]
        value: Option<reference_device_id::Value>,
    }

    mod reference_device_id {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub(super) enum Value {
            #[prost(message, tag = "1")]
            Machine(crate::machine::MachineId),
            #[prost(message, tag = "2")]
            Switch(crate::switch::SwitchId),
            #[prost(message, tag = "3")]
            PowerShelf(crate::power_shelf::PowerShelfId),
        }
    }

    fn reference_message(id: DeviceId) -> ReferenceDeviceId {
        use reference_device_id::Value;

        ReferenceDeviceId {
            value: Some(match id {
                DeviceId::Machine(id) => Value::Machine(id),
                DeviceId::Switch(id) => Value::Switch(id),
                DeviceId::PowerShelf(id) => Value::PowerShelf(id),
            }),
        }
    }

    fn parse(input: &str) -> Result<(String, &'static str), &'static str> {
        DeviceId::from_str(input)
            .map(|id| {
                let kind = match id {
                    DeviceId::Machine(_) => "machine",
                    DeviceId::Switch(_) => "switch",
                    DeviceId::PowerShelf(_) => "power shelf",
                };
                (id.to_string(), kind)
            })
            .map_err(|error| match error {
                DeviceIdParseError::Machine(_) => "invalid machine",
                DeviceIdParseError::Switch(_) => "invalid switch",
                DeviceIdParseError::PowerShelf(_) => "invalid power shelf",
                DeviceIdParseError::UnknownId(_) => "unknown device",
            })
    }

    #[test]
    fn parses_each_device_kind() {
        scenarios!(
            run = parse;
            "supported prefixes" {
                MACHINE_ID => Yields((MACHINE_ID.to_string(), "machine")),
                SWITCH_ID => Yields((SWITCH_ID.to_string(), "switch")),
                POWER_SHELF_ID => Yields((POWER_SHELF_ID.to_string(), "power shelf")),
            }

            "invalid identifiers" {
                "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hc" => FailsWith("invalid machine"),
                "unknown-id" => FailsWith("unknown device"),
            }
        );
    }

    #[test]
    fn serde_round_trips_each_device_kind() {
        value_scenarios!(
            run = |input| {
                let id = DeviceId::from_str(input).unwrap();
                serde_json::from_str::<DeviceId>(&serde_json::to_string(&id).unwrap()).unwrap()
            };
            "device IDs" {
                MACHINE_ID => DeviceId::from_str(MACHINE_ID).unwrap(),
                SWITCH_ID => DeviceId::from_str(SWITCH_ID).unwrap(),
                POWER_SHELF_ID => DeviceId::from_str(POWER_SHELF_ID).unwrap(),
            }
        );
    }

    #[test]
    fn protobuf_round_trips_each_oneof_variant() {
        value_scenarios!(
            run = |input| {
                let id = DeviceId::from_str(input).unwrap();
                let encoded = id.encode_to_vec();
                let decoded = DeviceId::decode(encoded.as_slice()).unwrap();
                let reference_decoded = ReferenceDeviceId::decode(encoded.as_slice()).unwrap();
                (encoded, decoded, reference_decoded)
            };
            "wire-compatible oneof variants" {
                MACHINE_ID => {
                    let id = DeviceId::from_str(MACHINE_ID).unwrap();
                    let reference = reference_message(id);
                    (reference.encode_to_vec(), id, reference)
                },
                SWITCH_ID => {
                    let id = DeviceId::from_str(SWITCH_ID).unwrap();
                    let reference = reference_message(id);
                    (reference.encode_to_vec(), id, reference)
                },
                POWER_SHELF_ID => {
                    let id = DeviceId::from_str(POWER_SHELF_ID).unwrap();
                    let reference = reference_message(id);
                    (reference.encode_to_vec(), id, reference)
                },
            }
        );
    }
}
