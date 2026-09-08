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

use std::cmp::Ordering;
use std::convert::Infallible;
use std::fmt;
use std::fmt::{Debug, Display, Formatter, Write};
use std::hash::Hash;
use std::str::FromStr;

use data_encoding::BASE32_DNSSEC;
use prost::DecodeError;
use prost::bytes::{Buf, BufMut};
use prost::encoding::{DecodeContext, WireType};
use serde::{Deserialize, Serialize};

use super::DbPrimaryUuid;

static MACHINE_ID_PREFIX: &str = "fm100";

use crate::typed_uuids::{TypedUuid, UuidSubtype};

macro_rules! impl_prost_message_for_machine_id {
    ($newtype:ty, $default_type:expr) => {
        impl prost::Message for $newtype {
            fn encode_raw(&self, buf: &mut impl prost::bytes::BufMut)
            where
                Self: Sized,
            {
                super::encode_machine_id(self.as_machine_id(), buf);
            }

            fn merge_field(
                &mut self,
                tag: u32,
                wire_type: prost::encoding::WireType,
                buf: &mut impl prost::bytes::Buf,
                ctx: prost::encoding::DecodeContext,
            ) -> Result<(), prost::DecodeError>
            where
                Self: Sized,
            {
                let mut id = *self.as_machine_id();
                super::merge_machine_id(&mut id, tag, wire_type, buf, ctx)?;
                *self = Self::try_from(id).map_err(|error| {
                    #[allow(deprecated)]
                    prost::DecodeError::new(error.to_string())
                })?;
                Ok(())
            }

            fn encoded_len(&self) -> usize {
                super::encoded_machine_id_len(self.as_machine_id())
            }

            #[allow(deprecated)]
            fn clear(&mut self) {
                *self = Self::default();
            }
        }

        impl Default for $newtype {
            /// Returns a synthetic value required for prost message decoding.
            ///
            /// This is an interoperability value, not an application machine ID.
            fn default() -> Self {
                Self::try_from(super::prost_default_machine_id($default_type))
                    .expect("the prost default machine type matches the subtype")
            }
        }
    };
}

// Macro used by below modules (`mod dpu;`, etc): needs to be declared before we declare them.
#[cfg(feature = "sqlx")]
macro_rules! impl_sqlx_for_machine_id_newtype {
    ($newtype:ty) => {
        impl sqlx::Encode<'_, sqlx::Postgres> for $newtype {
            fn encode_by_ref(
                &self,
                buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                buf.extend(self.to_string().as_bytes());
                Ok(sqlx::encode::IsNull::No)
            }
        }

        impl<'r, DB> sqlx::Decode<'r, DB> for $newtype
        where
            DB: sqlx::Database,
            String: sqlx::Decode<'r, DB>,
        {
            fn decode(
                value: <DB as sqlx::database::Database>::ValueRef<'r>,
            ) -> Result<Self, sqlx::error::BoxDynError> {
                let str_id: String = String::decode(value)?;
                Ok(str_id.parse::<super::MachineId>()?.try_into()?)
            }
        }

        impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for $newtype {
            fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
                sqlx::Row::try_get(row, 0)
            }
        }

        impl<DB> sqlx::Type<DB> for $newtype
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

        impl sqlx::postgres::PgHasArrayType for $newtype {
            fn array_type_info() -> sqlx::postgres::PgTypeInfo {
                <&str as sqlx::postgres::PgHasArrayType>::array_type_info()
            }

            fn array_compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <&str as sqlx::postgres::PgHasArrayType>::array_compatible(ty)
            }
        }
    };
}

mod dpu;
mod host;
mod predicted_host;
mod stable_host;

pub use dpu::DpuMachineId;
pub use host::{HostMachineId, HostMachineIdSubtype};
pub use predicted_host::PredictedHostMachineId;
#[cfg(feature = "sqlx")]
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgHasArrayType, PgTypeInfo},
    {Database, Postgres, Row},
};
pub use stable_host::StableHostMachineId;

/// Marker type for MachineInterfaceId
pub struct MachineInterfaceIdMarker;

impl UuidSubtype for MachineInterfaceIdMarker {
    const TYPE_NAME: &'static str = "MachineInterfaceId";
}

/// MachineInterfaceId is a strongly typed UUID for machine interfaces.
pub type MachineInterfaceId = TypedUuid<MachineInterfaceIdMarker>;

/// This is a fixed-size hash of the machine hardware.
pub type HardwareHash = [u8; 32];
/// This is the base32-encoded representation of the hardware hash. It is a fixed size instead of a
/// String so that we can implement the Copy trait.
pub type HardwareIdBase32 = [u8; MACHINE_ID_HARDWARE_ID_BASE32_LENGTH];

/// The `MachineId` uniquely identifies a machine that is managed by the Forge system
///
/// `MachineId`s are derived from a hardware fingerprint, and are thereby
/// globally unique.
///
/// MachineIds are using an encoding which makes them valid DNS names.
/// This requires the use of lowercase characters only.
///
/// Examples for MachineIds can be:
/// - fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - fm100dtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - fm100hsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - fm100dsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - fm100ptjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct MachineId {
    /// The hardware source from which the Machine ID was derived
    source: MachineIdSource,
    /// The Machine hash which was derived via hashing from the hardware piece
    /// that is indicated in `source`, encoded via base32. Must be valid utf-8.
    hardware_id: HardwareIdBase32,
    /// The Type of the Machine
    ty: MachineType,
}

impl Ord for MachineId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ty
            .id_char()
            .cmp(&other.ty.id_char())
            .then_with(|| self.source.id_char().cmp(&other.source.id_char()))
            .then_with(|| self.hardware_id.cmp(&other.hardware_id))
    }
}

impl PartialOrd for MachineId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Implement [`prost::Message`] manually so that we can be wire-compatible with the
// `.common.MachineId` protobuf message, which is what we actually serialize.
impl prost::Message for MachineId {
    fn encode_raw(&self, buf: &mut impl BufMut)
    where
        Self: Sized,
    {
        encode_machine_id(self, buf);
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
        merge_machine_id(self, tag, wire_type, buf, ctx)
    }

    fn encoded_len(&self) -> usize {
        encoded_machine_id_len(self)
    }

    #[allow(deprecated)]
    fn clear(&mut self) {
        *self = MachineId::default();
    }
}

fn encode_machine_id(id: &MachineId, buf: &mut impl BufMut) {
    prost::Message::encode_raw(&legacy_rpc::MachineId::from(*id), buf);
}

fn merge_machine_id(
    id: &mut MachineId,
    tag: u32,
    wire_type: WireType,
    buf: &mut impl Buf,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    let mut legacy_message = legacy_rpc::MachineId::from(*id);
    prost::Message::merge_field(&mut legacy_message, tag, wire_type, buf, ctx)?;
    *id = MachineId::from_str(&legacy_message.id).map_err(|_| {
        #[allow(deprecated)]
        DecodeError::new(format!("Invalid machine id: {}", legacy_message.id))
    })?;
    Ok(())
}

fn encoded_machine_id_len(id: &MachineId) -> usize {
    prost::Message::encoded_len(&legacy_rpc::MachineId::from(*id))
}

mod legacy_rpc {
    /// Backwards compatiblity shim for [`super::MachineId`] to be sent as a protobuf message in a
    /// way that is compatible with the `.common.MachineId` message, which is defined as:
    ///
    /// ```ignore
    /// message MachineId {
    ///     string id = 1;
    /// }
    /// ```
    ///
    /// This allows us to use [`super::MachineId`] directly instead of having to convert it
    /// manually every time, while still interacting with peers that expect a `.common.MachineId`
    /// to be serialized.
    #[derive(prost::Message)]
    pub(super) struct MachineId {
        #[prost(string, tag = "1")]
        pub(super) id: String,
    }

    impl From<super::MachineId> for MachineId {
        fn from(value: crate::machine::MachineId) -> Self {
            Self {
                id: value.to_string(),
            }
        }
    }
}

impl Default for MachineId {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::default()
    }
}

impl Debug for MachineId {
    // The derived Debug implementation is messy, just output the string representation even when
    // debugging.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

// Make MachineId bindable directly into a sqlx query
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for MachineId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> sqlx::Decode<'r, DB> for MachineId
where
    DB: sqlx::Database,
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_id: String = String::decode(value)?;
        Ok(MachineId::from_str(&str_id).map_err(|e| sqlx::Error::Decode(Box::new(e)))?)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for MachineId {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let id: MachineId = row.try_get(0)?;
        Ok(id)
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for MachineId
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
impl PgHasArrayType for MachineId {
    fn array_type_info() -> PgTypeInfo {
        <&str as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <&str as PgHasArrayType>::array_compatible(ty)
    }
}

pub trait MachineIdSubtypeTrait:
    Display
    + Copy
    + Clone
    + Send
    + Sync
    + Eq
    + PartialEq
    + Hash
    + Into<MachineId>
    + TryFrom<MachineId, Error: Into<InvalidMachineType>>
{
    fn machine_type(&self) -> MachineType;
    fn as_machine_id(&self) -> &MachineId;
    fn machine_id_subtype(&self) -> MachineIdSubtype;

    fn to_machine_id(&self) -> MachineId {
        *Self::as_machine_id(self)
    }

    fn host_or_dpu_id(&self) -> HostOrDpuId {
        HostOrDpuId::from(self.machine_id_subtype())
    }

    fn is_dpu(&self) -> bool {
        matches!(self.machine_type(), MachineType::Dpu)
    }
}

pub trait AsMachineId: Send + Sync {
    fn as_machine_id(&self) -> &MachineId;
}

impl<T: MachineIdSubtypeTrait> AsMachineId for T {
    fn as_machine_id(&self) -> &MachineId {
        <Self as MachineIdSubtypeTrait>::as_machine_id(self)
    }
}

impl MachineIdSubtypeTrait for MachineId {
    fn machine_type(&self) -> MachineType {
        Self::machine_type(self)
    }

    fn as_machine_id(&self) -> &MachineId {
        self
    }

    fn machine_id_subtype(&self) -> MachineIdSubtype {
        MachineId::machine_id_subtype(self)
    }
}

impl From<&MachineId> for MachineId {
    fn from(id: &MachineId) -> Self {
        *id
    }
}

/// Equivalent to [`MachineType`] but carries the strongly-typed subtype with each variant
pub enum MachineIdSubtype {
    Dpu(DpuMachineId),
    StableHost(StableHostMachineId),
    PredictedHost(PredictedHostMachineId),
}

/// Similar to [`MachineIdSubtype`] but for cases where stable and predicted host ID's should be
/// combined into a single [`HostMachineId`] variant.
pub enum HostOrDpuId {
    Host(HostMachineId),
    Dpu(DpuMachineId),
}

impl From<MachineIdSubtype> for HostOrDpuId {
    fn from(value: MachineIdSubtype) -> Self {
        match value {
            MachineIdSubtype::Dpu(id) => HostOrDpuId::Dpu(id),
            MachineIdSubtype::StableHost(id) => HostOrDpuId::Host(id.into()),
            MachineIdSubtype::PredictedHost(id) => HostOrDpuId::Host(id.into()),
        }
    }
}

impl MachineId {
    pub fn new(source: MachineIdSource, hardware_hash: HardwareHash, ty: MachineType) -> MachineId {
        // BASE32_DNSSEC is chosen to just generate lowercase characters and
        // numbers - which will result in valid DNS names for MachineIds.
        let encoded = BASE32_DNSSEC.encode(&hardware_hash);
        assert_eq!(encoded.len(), MACHINE_ID_HARDWARE_ID_BASE32_LENGTH);

        Self {
            source,
            hardware_id: encoded.as_bytes().try_into().unwrap(),
            ty,
        }
    }

    /// The hardware source from which the Machine ID was derived
    pub fn source(&self) -> MachineIdSource {
        self.source
    }

    /// The type of the Machine
    pub fn machine_type(&self) -> MachineType {
        self.ty
    }

    /// Note: Never use this! Tonic's codegen requires all types to implement Default, but there is
    /// no logical reason to construct a "default" MachineId in real code, so we simply construct a
    /// bogus one here.
    #[allow(clippy::should_implement_trait)]
    #[deprecated(
        note = "Do not use `MachineId::default()` directly; only implemented for prost interop"
    )]
    pub fn default() -> Self {
        prost_default_machine_id(MachineType::Host)
    }

    pub(crate) fn is_matching_prefix(s: &str) -> bool {
        s.starts_with(MACHINE_ID_PREFIX)
    }

    /// Equivalent to [`Self::machine_type`] but returns a [`MachineIdSubtype`], which carries the
    /// strongly-typed subtype with each variant
    pub fn machine_id_subtype(&self) -> MachineIdSubtype {
        match self.ty {
            MachineType::Dpu => MachineIdSubtype::Dpu(DpuMachineId(*self)),
            MachineType::Host => {
                MachineIdSubtype::StableHost(StableHostMachineId(HostMachineId(*self)))
            }
            MachineType::PredictedHost => {
                MachineIdSubtype::PredictedHost(PredictedHostMachineId(HostMachineId(*self)))
            }
        }
    }

    /// Similar to [`Self::machine_id_subtype`] but for cases where stable and predicted host ID's
    /// should be combined into a single [`HostMachineId`] variant.
    pub fn host_or_dpu_id(&self) -> HostOrDpuId {
        match self.ty {
            MachineType::Dpu => HostOrDpuId::Dpu(DpuMachineId(*self)),
            MachineType::Host | MachineType::PredictedHost => {
                HostOrDpuId::Host(HostMachineId(*self))
            }
        }
    }
}

fn prost_default_machine_id(ty: MachineType) -> MachineId {
    MachineId::new(MachineIdSource::ProductBoardChassisSerial, [0; 32], ty)
}

impl DbPrimaryUuid for MachineId {
    fn db_primary_uuid_name() -> &'static str {
        "machine_id"
    }
}

/// The hardware source from which the Machine ID is derived
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum MachineIdSource {
    /// The Machine ID was generated by hashing the TPM EkCertificate data.
    Tpm,
    /// The Machine ID was generated by the concatenation of product, board and chassis serial
    /// and hashing the resulting value.
    /// If any of those values is not available in DMI data, an empty
    /// string will be used instead. At least one serial number must have been
    /// available to generate this ID.
    ProductBoardChassisSerial,
}

impl MachineIdSource {
    /// Returns the character that identifies the source type
    pub const fn id_char(self) -> char {
        match self {
            MachineIdSource::Tpm => 't',
            MachineIdSource::ProductBoardChassisSerial => 's',
        }
    }

    /// Parses the `MachineIdSource` from a character
    pub fn from_id_char(c: char) -> Option<Self> {
        match c {
            c if c == Self::Tpm.id_char() => Some(Self::Tpm),
            c if c == Self::ProductBoardChassisSerial.id_char() => {
                Some(Self::ProductBoardChassisSerial)
            }
            _ => None,
        }
    }
}

/// Extra flags that are associated with the machine ID
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum MachineType {
    // The Machine is a DPU
    Dpu,
    /// The Machine is a Forge managed host
    Host,
    /// The Machine is a host whose existence had been predicated by a DPU
    /// being detected by Forge.
    /// However the actual Machine ID of the host is not yet known, since the
    /// Machine hardware details are not yet known. Therefore a **temporary**
    /// ID is created. The temporary ID is derived from the DPU Machine ID,
    /// and carries its hardware identifier, which will be encoded in the
    /// `source` and `hardware_id` fields.
    ///
    /// Once the hardware fingerprint of the host is known, the Machine will
    /// obtain a new `MachineId` with `MachineType::Host`
    PredictedHost,
}

impl MachineType {
    /// Returns `true` if the Machine is a DPU
    pub fn is_dpu(self) -> bool {
        self == MachineType::Dpu
    }

    /// Returns the [`MachineType`] this ID string matches, if any.
    pub fn from_id_string(s: &str) -> Option<Self> {
        MachineId::from_str(s).ok().map(|i| i.ty)
    }

    /// Returns `true` if the Machine is a Host
    ///
    /// This only returns `true` for hosts which actually have been discovered,
    /// and not for temporary (predicted) hosts.
    pub fn is_host(self) -> bool {
        self == MachineType::Host
    }

    pub fn is_predicted_host(self) -> bool {
        self == MachineType::PredictedHost
    }

    /// Description of this machine type when used by metrics (lowercased, intended to be machine-readable)
    pub fn metrics_value(&self) -> &'static str {
        match self {
            MachineType::Dpu => "dpu",
            MachineType::Host => "host",
            MachineType::PredictedHost => "predictedhost",
        }
    }

    pub fn id_prefix(self) -> &'static str {
        match self {
            MachineType::Dpu => "fm100d",
            MachineType::Host => "fm100h",
            MachineType::PredictedHost => "fm100p",
        }
    }
}

impl std::fmt::Display for MachineType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MachineType::Dpu => f.write_str("DPU"),
            MachineType::Host => f.write_str("Host"),
            MachineType::PredictedHost => f.write_str("Host (Predicted)"),
        }
    }
}

impl MachineType {
    /// Returns the character that identifies the flag
    pub const fn id_char(self) -> char {
        match self {
            MachineType::Dpu => 'd',
            MachineType::Host => 'h',
            MachineType::PredictedHost => 'p',
        }
    }

    /// Parses the `MachineType` from a character
    pub fn from_id_char(c: char) -> Option<Self> {
        match c {
            c if c == Self::Dpu.id_char() => Some(Self::Dpu),
            c if c == Self::Host.id_char() => Some(Self::Host),
            c if c == Self::PredictedHost.id_char() => Some(Self::PredictedHost),
            _ => None,
        }
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `fm` is for forge-machine
        // `1` is a version identifier
        // The next 2 bytes `00` are reserved
        f.write_str(MACHINE_ID_PREFIX)?;
        // Write the machine type
        f.write_char(self.ty.id_char())?;
        // The next character determines how the MachineId is derived (`MachineIdSource`)
        f.write_char(self.source.id_char())?;
        // Then follows the source data.
        // SAFETY: `hardware_id` is private and populated only from `BASE32_DNSSEC::encode`, whose
        // output is ASCII and therefore valid UTF-8.
        unsafe { f.write_str(std::str::from_utf8_unchecked(self.hardware_id.as_slice())) }
    }
}

/// The length that is used for the prefix in Machine IDs
pub const MACHINE_ID_PREFIX_LENGTH: usize = 7;

/// The length of the hardware ID substring embedded in the Machine ID
///
/// Since it's a base32 encoded SHA256 (32byte), this makes 52 bytes
pub const MACHINE_ID_HARDWARE_ID_BASE32_LENGTH: usize = 52;

/// The length of a valid MachineID
///
/// It is made up of the prefix length (5 bytes) plus the encoded hardware ID length
pub const MACHINE_ID_LENGTH: usize =
    MACHINE_ID_PREFIX_LENGTH + MACHINE_ID_HARDWARE_ID_BASE32_LENGTH;

#[derive(thiserror::Error, Debug, Clone)]
pub enum MachineIdParseError {
    #[error("the machine ID has an invalid length of {0}")]
    Length(usize),
    #[error("the machine ID {0} has an invalid prefix")]
    Prefix(String),
    #[error("the machine ID {0} has an invalid encoding")]
    Encoding(String),
}

/// Represents a failure of a particular subtype of MachineId (HostMachineId, DpuMachineId, etc) to
/// parse. Can either be due to the machine ID being invalid altogether, or of the wrong type.
#[derive(thiserror::Error, Debug)]
pub enum MachineIdSubtypeParseError {
    #[error("not a valid machine ID: {0}")]
    Invalid(#[from] MachineIdParseError),
    #[error("machine ID is of wrong type: {0}")]
    WrongType(#[from] InvalidMachineType),
}

impl FromStr for MachineId {
    type Err = MachineIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != MACHINE_ID_LENGTH {
            return Err(MachineIdParseError::Length(s.len()));
        }
        // Check for version 1 and 2 reserved bytes
        if !s.starts_with(MACHINE_ID_PREFIX) {
            return Err(MachineIdParseError::Prefix(s.to_string()));
        }

        // Everything after the prefix needs to be valid base32
        let hardware_id = &s.as_bytes()[MACHINE_ID_PREFIX_LENGTH..];

        let mut hardware_hash: HardwareHash = [0u8; 32];
        match BASE32_DNSSEC.decode_mut(hardware_id, &mut hardware_hash) {
            Err(_) => return Err(MachineIdParseError::Encoding(s.to_string())),
            Ok(size) if size != 32 => return Err(MachineIdParseError::Encoding(s.to_string())),
            _ => {}
        }

        let ty = MachineType::from_id_char(s.as_bytes()[5] as char)
            .ok_or_else(|| MachineIdParseError::Prefix(s.to_string()))?;
        let source = MachineIdSource::from_id_char(s.as_bytes()[6] as char)
            .ok_or_else(|| MachineIdParseError::Prefix(s.to_string()))?;

        Ok(MachineId::new(source, hardware_hash, ty))
    }
}

impl Serialize for MachineId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MachineId {
    fn deserialize<D>(deserializer: D) -> Result<MachineId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let id = MachineId::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(id)
    }
}

/// Error returned when a machine ID does not have the required machine type.
#[derive(thiserror::Error, Debug)]
#[error("expected {expected}, got {actual}")]
pub struct InvalidMachineType {
    expected: &'static str,
    actual: MachineId,
}

impl From<Infallible> for InvalidMachineType {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum ParseFailure {
        Length,
        Prefix,
        Encoding,
    }

    fn parse_machine_id(input: &str) -> Result<String, ParseFailure> {
        MachineId::from_str(input)
            .map(|id| id.to_string())
            .map_err(|err| match err {
                MachineIdParseError::Length(_) => ParseFailure::Length,
                MachineIdParseError::Prefix(_) => ParseFailure::Prefix,
                MachineIdParseError::Encoding(_) => ParseFailure::Encoding,
            })
    }

    #[test]
    fn test_machine_id_parse_cases() {
        const VALID_MACHINE_ID: &str =
            "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

        scenarios!(
            run = parse_machine_id;
            "valid host TPM machine ID" {
                VALID_MACHINE_ID => Yields(VALID_MACHINE_ID.to_string()),
            }

            "one character short" {
                "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hc" => FailsWith(ParseFailure::Length),
            }

            "empty string" {
                "" => FailsWith(ParseFailure::Length),
            }

            "invalid prefix casing" {
                "FM100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid machine type" {
                "fm100xt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid source" {
                "fm100dx038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid base32 payload" {
                "fm100ht038bg3qsho433vkg684heguv28!qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Encoding),
            }
        );
    }

    #[test]
    fn test_machine_type_mappings() {
        value_scenarios!(
            run = |ty| {
                (
                    ty.id_char(),
                    ty.id_prefix(),
                    ty.metrics_value(),
                    ty.to_string(),
                    ty.is_dpu(),
                    ty.is_host(),
                    ty.is_predicted_host(),
                )
            };
            "DPU" {
                MachineType::Dpu => ('d', "fm100d", "dpu", "DPU".to_string(), true, false, false),
            }

            "host" {
                MachineType::Host => (
                    'h',
                    "fm100h",
                    "host",
                    "Host".to_string(),
                    false,
                    true,
                    false,
                ),
            }

            "predicted host" {
                MachineType::PredictedHost => (
                    'p',
                    "fm100p",
                    "predictedhost",
                    "Host (Predicted)".to_string(),
                    false,
                    false,
                    true,
                ),
            }
        );
    }

    #[test]
    fn test_machine_type_from_id_char() {
        value_scenarios!(
            run = MachineType::from_id_char;
            "DPU" {
                'd' => Some(MachineType::Dpu),
            }

            "host" {
                'h' => Some(MachineType::Host),
            }

            "predicted host" {
                'p' => Some(MachineType::PredictedHost),
            }

            "unknown" {
                'x' => None,
            }
        );
    }

    #[test]
    fn test_machine_id_source_from_id_char() {
        value_scenarios!(
            run = MachineIdSource::from_id_char;
            "TPM" {
                't' => Some(MachineIdSource::Tpm),
            }

            "product board chassis serial" {
                's' => Some(MachineIdSource::ProductBoardChassisSerial),
            }

            "unknown" {
                'x' => None,
            }
        );
    }

    #[test]
    fn test_machine_id_order_matches_string_order() {
        let machine_ids = [
            MachineType::Dpu,
            MachineType::Host,
            MachineType::PredictedHost,
        ]
        .into_iter()
        .flat_map(|ty| {
            [
                MachineIdSource::Tpm,
                MachineIdSource::ProductBoardChassisSerial,
            ]
            .into_iter()
            .flat_map(move |source| {
                [[0; 32], [0x55; 32], [0xff; 32]]
                    .into_iter()
                    .map(move |hardware_hash| MachineId::new(source, hardware_hash, ty))
            })
        })
        .collect::<Vec<_>>();

        for left in &machine_ids {
            for right in &machine_ids {
                assert_eq!(
                    left.cmp(right),
                    left.to_string().cmp(&right.to_string()),
                    "direct ordering differs for {left} and {right}",
                );
            }
        }
    }

    #[test]
    fn test_machine_id_subtype_accessors() {
        let hardware_hash = [42; 32];
        let dpu_id = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Dpu);
        let stable_host_id = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Host);
        let predicted_host_id = MachineId::new(
            MachineIdSource::Tpm,
            hardware_hash,
            MachineType::PredictedHost,
        );

        assert!(matches!(
            dpu_id.machine_id_subtype(),
            MachineIdSubtype::Dpu(id) if id.as_machine_id() == &dpu_id
        ));
        assert!(matches!(
            stable_host_id.machine_id_subtype(),
            MachineIdSubtype::StableHost(id) if id.as_machine_id() == &stable_host_id
        ));
        assert!(matches!(
            predicted_host_id.machine_id_subtype(),
            MachineIdSubtype::PredictedHost(id) if id.as_machine_id() == &predicted_host_id
        ));

        assert!(matches!(
            dpu_id.host_or_dpu_id(),
            HostOrDpuId::Dpu(id) if id.as_machine_id() == &dpu_id
        ));
        for id in [stable_host_id, predicted_host_id] {
            let HostOrDpuId::Host(host_id) = id.host_or_dpu_id() else {
                panic!("expected {id} to be classified as a host");
            };
            assert_eq!(host_id.as_machine_id(), &id);
            assert_eq!(host_id.is_stable_host(), id == stable_host_id);
            assert!(match host_id.host_machine_id_subtype() {
                HostMachineIdSubtype::Stable(subtype) => {
                    subtype.as_machine_id() == &id && id == stable_host_id
                }
                HostMachineIdSubtype::Predicted(subtype) => {
                    subtype.as_machine_id() == &id && id == predicted_host_id
                }
            });
        }
    }

    #[test]
    fn test_machine_id_newtype_conversion_matrix() {
        let hardware_hash = [42; 32];
        let stable = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Host);
        let predicted = MachineId::new(
            MachineIdSource::Tpm,
            hardware_hash,
            MachineType::PredictedHost,
        );
        let dpu = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Dpu);

        value_scenarios!(
            run = |id| HostMachineId::try_from(id).is_ok();
            "stable host is a host" { stable => true }
            "predicted host is a host" { predicted => true }
            "DPU is not a host" { dpu => false }
        );
        value_scenarios!(
            run = |id| StableHostMachineId::try_from(id).is_ok();
            "stable host is stable" { stable => true }
            "predicted host is not stable" { predicted => false }
            "DPU is not stable" { dpu => false }
        );
        value_scenarios!(
            run = |id| PredictedHostMachineId::try_from(id).is_ok();
            "predicted host is predicted" { predicted => true }
            "stable host is not predicted" { stable => false }
            "DPU is not predicted" { dpu => false }
        );
        value_scenarios!(
            run = |id| DpuMachineId::try_from(id).is_ok();
            "DPU is a DPU" { dpu => true }
            "stable host is not a DPU" { stable => false }
            "predicted host is not a DPU" { predicted => false }
        );

        value_scenarios!(
            run = |id| {
                let host_id = HostMachineId::try_from(id).unwrap();
                (
                    StableHostMachineId::try_from(host_id).is_ok(),
                    PredictedHostMachineId::try_from(host_id).is_ok(),
                )
            };
            "host narrows to stable host" { stable => (true, false) }
            "host narrows to predicted host" { predicted => (false, true) }
        );

        assert_eq!(
            PredictedHostMachineId::try_from(HostMachineId::try_from(stable).unwrap())
                .unwrap_err()
                .to_string(),
            format!("expected predicted host machine ID, got {stable}"),
        );
        assert_eq!(
            StableHostMachineId::try_from(HostMachineId::try_from(predicted).unwrap())
                .unwrap_err()
                .to_string(),
            format!("expected stable host machine ID, got {predicted}"),
        );
    }

    #[test]
    fn test_machine_id_newtype_parsing_and_json() {
        let hardware_hash = [42; 32];
        let stable = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Host);
        let predicted = MachineId::new(
            MachineIdSource::Tpm,
            hardware_hash,
            MachineType::PredictedHost,
        );
        let dpu = MachineId::new(MachineIdSource::Tpm, hardware_hash, MachineType::Dpu);

        fn acceptance<T>(id: MachineId) -> (bool, bool)
        where
            T: FromStr + serde::de::DeserializeOwned,
        {
            (
                id.to_string().parse::<T>().is_ok(),
                serde_json::from_str::<T>(&serde_json::to_string(&id).unwrap()).is_ok(),
            )
        }

        value_scenarios!(
            run = |id| (
                acceptance::<HostMachineId>(id),
                acceptance::<StableHostMachineId>(id),
                acceptance::<PredictedHostMachineId>(id),
                acceptance::<DpuMachineId>(id),
            );
            "stable host parsing and JSON" {
                stable => ((true, true), (true, true), (false, false), (false, false))
            }
            "predicted host parsing and JSON" {
                predicted => ((true, true), (false, false), (true, true), (false, false))
            }
            "DPU parsing and JSON" {
                dpu => ((false, false), (false, false), (false, false), (true, true))
            }
        );

        let typed = StableHostMachineId::try_from(stable).unwrap();

        assert_eq!(
            serde_json::to_string(&typed).unwrap(),
            serde_json::to_string(&stable).unwrap()
        );
        assert_eq!(
            serde_json::from_str::<StableHostMachineId>(&serde_json::to_string(&stable).unwrap())
                .unwrap(),
            typed
        );
    }
}
