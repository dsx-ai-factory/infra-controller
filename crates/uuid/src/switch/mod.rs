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
use std::fmt;
use std::fmt::{Debug, Display, Formatter, Write};
use std::str::FromStr;

use data_encoding::BASE32_DNSSEC;
use prost::DecodeError;
use prost::bytes::{Buf, BufMut};
use prost::encoding::{DecodeContext, WireType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(feature = "sqlx")]
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgHasArrayType, PgTypeInfo},
    {Database, Postgres, Row},
};

use crate::DbPrimaryUuid;

static SWITCH_ID_PREFIX: &str = "sw100";

/// This is a fixed-size hash of the switch hardware.
pub type HardwareHash = [u8; 32];
/// This is the base32-encoded representation of the hardware hash. It is a fixed size instead of a
/// String so that we can implement the Copy trait.
pub type HardwareIdBase32 = [u8; SWITCH_ID_HARDWARE_ID_BASE32_LENGTH];

/// The `SwitchId` uniquely identifies a switch that is managed by the Forge system
///
/// `SwitchId`s are derived from a hardware fingerprint, and are thereby
/// globally unique.
///
/// SwitchIds are using an encoding which makes them valid DNS names.
/// This requires the use of lowercase characters only.
///
/// Examples for SwitchIds can be:
/// - sw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - sw100rtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - sw100hsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - sw100rsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - sw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct SwitchId {
    /// The hardware source from which the Switch ID was derived
    source: SwitchIdSource,
    /// The Switch hash which was derived via hashing from the hardware piece
    /// that is indicated in `source`, encoded via base32. Must be valid utf-8.
    hardware_id: HardwareIdBase32,
    /// The Type of the Switch
    ty: SwitchType,
}

impl Ord for SwitchId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl PartialOrd for SwitchId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Default for SwitchId {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::default()
    }
}

impl Debug for SwitchId {
    // The derived Debug implementation is messy, just output the string representation even when
    // debugging.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

// Make SwitchId bindable directly into a sqlx query
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for SwitchId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> sqlx::Decode<'r, DB> for SwitchId
where
    DB: sqlx::database::Database,
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_id: String = String::decode(value)?;
        Ok(SwitchId::from_str(&str_id).map_err(|e| sqlx::Error::Decode(Box::new(e)))?)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for SwitchId {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let id: SwitchId = row.try_get::<SwitchId, _>(0)?;
        Ok(id)
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for SwitchId
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
impl PgHasArrayType for SwitchId {
    fn array_type_info() -> PgTypeInfo {
        <&str as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <&str as PgHasArrayType>::array_compatible(ty)
    }
}

impl SwitchId {
    pub fn new(source: SwitchIdSource, hardware_hash: HardwareHash, ty: SwitchType) -> SwitchId {
        // BASE32_DNSSEC is chosen to just generate lowercase characters and
        // numbers - which will result in valid DNS names for SwitchIds.
        let encoded = BASE32_DNSSEC.encode(&hardware_hash);
        assert_eq!(encoded.len(), SWITCH_ID_HARDWARE_ID_BASE32_LENGTH);

        Self {
            source,
            hardware_id: encoded.as_bytes().try_into().unwrap(),
            ty,
        }
    }

    /// The hardware source from which the Switch ID was derived
    pub fn source(&self) -> SwitchIdSource {
        self.source
    }

    /// The type of the Switch
    pub fn switch_type(&self) -> SwitchType {
        self.ty
    }

    /// Generate Remote ID based on switchID.
    /// Remote Id is inserted by dhcrelay on DPU in each DHCP request sent by host.
    /// This field is used only for DPU.
    pub fn remote_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.to_string().as_bytes());
        let hash: [u8; 32] = hasher.finalize().into();
        BASE32_DNSSEC.encode(&hash)
    }

    /// NOTE: NEVER USE THIS!
    /// Tonic's codegen requires all types to implement Default, but there is
    /// no logical reason to construct a "default" SwitchId in real code, so
    /// we simply construct a bogus one here.
    #[allow(clippy::should_implement_trait)]
    #[deprecated(
        note = "Do not use `SwitchId::default()` directly; only implemented for prost interop"
    )]
    pub fn default() -> Self {
        Self::new(
            SwitchIdSource::ProductBoardChassisSerial,
            [0; 32],
            SwitchType::NvLink,
        )
    }

    pub(crate) fn is_matching_prefix(s: &str) -> bool {
        s.starts_with(SWITCH_ID_PREFIX)
    }
}

impl DbPrimaryUuid for SwitchId {
    fn db_primary_uuid_name() -> &'static str {
        "switch_id"
    }
}

/// The hardware source from which the Switch ID is derived
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SwitchIdSource {
    /// The Switch ID was generated by hashing the TPM EkCertificate data.
    Tpm,
    /// The Switch ID was generated by the concatenation of product, board and chassis serial
    /// and hashing the resulting value.
    /// If any of those values is not available in DMI data, an empty
    /// string will be used instead. At least one serial number must have been
    /// available to generate this ID.
    ProductBoardChassisSerial,
}

impl SwitchIdSource {
    /// Returns the character that identifies the source type
    pub const fn id_char(self) -> char {
        match self {
            SwitchIdSource::Tpm => 't',
            SwitchIdSource::ProductBoardChassisSerial => 's',
        }
    }

    /// Parses the `SwitchIdSource` from a character
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

/// Extra flags that are associated with the switch ID
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SwitchType {
    NvLink,
}

impl SwitchType {
    pub fn is_nvlink(self) -> bool {
        self == SwitchType::NvLink
    }
}

impl std::fmt::Display for SwitchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwitchType::NvLink => f.write_str("NvLink"),
        }
    }
}

impl SwitchType {
    /// Returns the character that identifies the flag
    pub const fn id_char(self) -> char {
        match self {
            SwitchType::NvLink => 'n',
        }
    }

    /// Parses the `SwitchType` from a character
    pub fn from_id_char(c: char) -> Option<Self> {
        match c {
            c if c == Self::NvLink.id_char() => Some(Self::NvLink),
            _ => None,
        }
    }
}

impl std::fmt::Display for SwitchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `sw` is for switch
        // `1` is a version identifier
        // The next 2 bytes `00` are reserved
        f.write_str(SWITCH_ID_PREFIX)?;
        // Write the switch type
        f.write_char(self.ty.id_char())?;
        // The next character determines how the SwitchId is derived (`SwitchIdSource`)
        f.write_char(self.source.id_char())?;
        // Then follows the source data.
        // SAFETY: `hardware_id` is private and populated only from `BASE32_DNSSEC::encode`, whose
        // output is ASCII and therefore valid UTF-8.
        unsafe { f.write_str(std::str::from_utf8_unchecked(self.hardware_id.as_slice())) }
    }
}

impl From<uuid::Uuid> for SwitchId {
    fn from(value: uuid::Uuid) -> Self {
        // This is a fallback implementation - in practice, SwitchId should be created
        // from hardware hashes, not random UUIDs
        let mut hasher = Sha256::new();
        hasher.update(value.as_bytes());
        let hash: [u8; 32] = hasher.finalize().into();

        Self::new(
            SwitchIdSource::Tpm, // Default source
            hash,
            SwitchType::NvLink, // Default type
        )
    }
}

/// The length that is used for the prefix in Switch IDs
pub const SWITCH_ID_PREFIX_LENGTH: usize = 7;

/// The length of the hardware ID substring embedded in the Switch ID
///
/// Since it's a base32 encoded SHA256 (32byte), this makes 52 bytes
pub const SWITCH_ID_HARDWARE_ID_BASE32_LENGTH: usize = 52;

/// The length of a valid SwitchID
///
/// It is made up of the prefix length (5 bytes) plus the encoded hardware ID length
pub const SWITCH_ID_LENGTH: usize = SWITCH_ID_PREFIX_LENGTH + SWITCH_ID_HARDWARE_ID_BASE32_LENGTH;

#[derive(thiserror::Error, Debug, Clone)]
pub enum SwitchIdParseError {
    #[error("the switch ID has an invalid length of {0}")]
    Length(usize),
    #[error("the switch ID {0} has an invalid prefix")]
    Prefix(String),
    #[error("the switch ID {0} has an invalid encoding")]
    Encoding(String),
}

impl FromStr for SwitchId {
    type Err = SwitchIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != SWITCH_ID_LENGTH {
            return Err(SwitchIdParseError::Length(s.len()));
        }
        // Check for version 1 and 2 reserved bytes
        if !s.starts_with(SWITCH_ID_PREFIX) {
            return Err(SwitchIdParseError::Prefix(s.to_string()));
        }

        // Everything after the prefix needs to be valid base32
        let hardware_id = &s.as_bytes()[SWITCH_ID_PREFIX_LENGTH..];

        let mut hardware_hash: HardwareHash = [0u8; 32];
        match BASE32_DNSSEC.decode_mut(hardware_id, &mut hardware_hash) {
            Err(_) => return Err(SwitchIdParseError::Encoding(s.to_string())),
            Ok(size) if size != 32 => return Err(SwitchIdParseError::Encoding(s.to_string())),
            _ => {}
        }

        let ty = SwitchType::from_id_char(s.as_bytes()[5] as char)
            .ok_or_else(|| SwitchIdParseError::Prefix(s.to_string()))?;
        let source = SwitchIdSource::from_id_char(s.as_bytes()[6] as char)
            .ok_or_else(|| SwitchIdParseError::Prefix(s.to_string()))?;

        Ok(SwitchId::new(source, hardware_hash, ty))
    }
}

impl Serialize for SwitchId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SwitchId {
    fn deserialize<D>(deserializer: D) -> Result<SwitchId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let id = SwitchId::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(id)
    }
}

// Implement [`prost::Message`] manually so that we can be wire-compatible with the
// `.common.SwitchId` protobuf message, which is what we actually serialize. Do this by
// constructing a `legacy_rpc::SwitchId` and delegate all  [`prost::Message`] methods to it.
impl prost::Message for SwitchId {
    fn encode_raw(&self, buf: &mut impl BufMut)
    where
        Self: Sized,
    {
        legacy_rpc::SwitchId::from(*self).encode_raw(buf);
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
        let mut legacy_message = legacy_rpc::SwitchId::from(*self);
        legacy_message.merge_field(tag, wire_type, buf, ctx)?;
        *self = SwitchId::from_str(&legacy_message.id).map_err(|_| {
            // Deprecation: if they remove DecodeError::new, they hopefully will provide some other way
            // to impl prost::Message.
            #[allow(deprecated)]
            DecodeError::new(format!("Invalid power shelf id: {}", legacy_message.id))
        })?;
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        legacy_rpc::SwitchId::from(*self).encoded_len()
    }

    #[allow(deprecated)]
    fn clear(&mut self) {
        *self = SwitchId::default();
    }
}

mod legacy_rpc {
    /// Backwards compatibility shim for [`super::SwitchId`] to be sent as a protobuf message
    /// in a way that is compatible with the `.common.SwitchId` message, which is defined as:
    ///
    /// ```ignore
    /// message SwitchId {
    ///     string id = 1;
    /// }
    /// ```
    ///
    /// This allows us to use [`super::SwitchId`] directly instead of having to convert it
    /// manually every time, while still interacting with peers that expect a `.common.SwitchId`
    /// to be serialized.
    #[derive(prost::Message)]
    pub(super) struct SwitchId {
        #[prost(string, tag = "1")]
        pub(super) id: String,
    }

    impl From<super::SwitchId> for SwitchId {
        fn from(value: crate::switch::SwitchId) -> Self {
            Self {
                id: value.to_string(),
            }
        }
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

    fn parse_switch_id(input: &str) -> Result<String, ParseFailure> {
        SwitchId::from_str(input)
            .map(|id| id.to_string())
            .map_err(|err| match err {
                SwitchIdParseError::Length(_) => ParseFailure::Length,
                SwitchIdParseError::Prefix(_) => ParseFailure::Prefix,
                SwitchIdParseError::Encoding(_) => ParseFailure::Encoding,
            })
    }

    #[test]
    fn test_switch_id_parse_cases() {
        const VALID_SWITCH_ID: &str = "sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

        scenarios!(
            run = parse_switch_id;
            "valid NVLink TPM switch ID" {
                VALID_SWITCH_ID => Yields(VALID_SWITCH_ID.to_string()),
            }

            "one character short" {
                "sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hc" => FailsWith(ParseFailure::Length),
            }

            "empty string" {
                "" => FailsWith(ParseFailure::Length),
            }

            "invalid prefix casing" {
                "SW100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid switch type" {
                "sw100xt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid source" {
                "sw100nx038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid base32 payload" {
                "sw100nt038bg3qsho433vkg684heguv28!qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Encoding),
            }
        );
    }

    #[test]
    fn test_switch_type_mappings() {
        value_scenarios!(
            run = |ty| (ty.id_char(), ty.to_string());
            "NVLink" {
                SwitchType::NvLink => ('n', "NvLink".to_string()),
            }
        );
    }

    #[test]
    fn test_switch_type_from_id_char() {
        value_scenarios!(
            run = SwitchType::from_id_char;
            "NVLink" {
                'n' => Some(SwitchType::NvLink),
            }

            "unknown" {
                'x' => None,
            }
        );
    }

    #[test]
    fn test_switch_id_source_from_id_char() {
        value_scenarios!(
            run = SwitchIdSource::from_id_char;
            "TPM" {
                't' => Some(SwitchIdSource::Tpm),
            }

            "product board chassis serial" {
                's' => Some(SwitchIdSource::ProductBoardChassisSerial),
            }

            "unknown" {
                'x' => None,
            }
        );
    }
}
