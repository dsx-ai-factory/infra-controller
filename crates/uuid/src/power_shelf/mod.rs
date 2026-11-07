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

/// This is a fixed-size hash of the power shelf hardware.
pub type HardwareHash = [u8; 32];
/// This is the base32-encoded representation of the hardware hash. It is a fixed size instead of a
/// String so that we can implement the Copy trait.
pub type HardwareIdBase32 = [u8; POWER_SHELF_ID_HARDWARE_ID_BASE32_LENGTH];

static POWER_SHELF_ID_PREFIX: &str = "ps100";

/// The `PowerShelfId` uniquely identifies a power shelf that is managed by the Forge system
///
/// `PowerShelfId`s are derived from a hardware fingerprint, and are thereby
/// globally unique.
///
/// PowerShelfIds are using an encoding which makes them valid DNS names.
/// This requires the use of lowercase characters only.
///
/// Examples for PowerShelfIds can be:
/// - ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - ps100rtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
/// - ps100hsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - ps100rsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g
/// - ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct PowerShelfId {
    /// The hardware source from which the Power Shelf ID was derived
    source: PowerShelfIdSource,
    /// The Power Shelf hash which was derived via hashing from the hardware piece
    /// that is indicated in `source`, encoded via base32. Must be valid utf-8.
    hardware_id: HardwareIdBase32,
    /// The Type of the Power Shelf
    ty: PowerShelfType,
}

impl Ord for PowerShelfId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl PartialOrd for PowerShelfId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Default for PowerShelfId {
    #[allow(deprecated)]
    fn default() -> Self {
        Self::default()
    }
}

impl Debug for PowerShelfId {
    // The derived Debug implementation is messy, just output the string representation even when
    // debugging.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

// Make PowerShelfId bindable directly into a sqlx query
#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for PowerShelfId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> sqlx::Decode<'r, DB> for PowerShelfId
where
    DB: sqlx::database::Database,
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_id: String = String::decode(value)?;
        Ok(PowerShelfId::from_str(&str_id).map_err(|e| sqlx::Error::Decode(Box::new(e)))?)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for PowerShelfId {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let id: PowerShelfId = row.try_get::<PowerShelfId, _>(0)?;
        Ok(id)
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for PowerShelfId
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
impl PgHasArrayType for PowerShelfId {
    fn array_type_info() -> PgTypeInfo {
        <&str as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <&str as PgHasArrayType>::array_compatible(ty)
    }
}

impl PowerShelfId {
    pub fn new(
        source: PowerShelfIdSource,
        hardware_hash: HardwareHash,
        ty: PowerShelfType,
    ) -> PowerShelfId {
        // BASE32_DNSSEC is chosen to just generate lowercase characters and
        // numbers - which will result in valid DNS names for PowerShelfIds.
        let encoded = BASE32_DNSSEC.encode(&hardware_hash);
        assert_eq!(encoded.len(), POWER_SHELF_ID_HARDWARE_ID_BASE32_LENGTH);

        Self {
            source,
            hardware_id: encoded.as_bytes().try_into().unwrap(),
            ty,
        }
    }

    /// The hardware source from which the Power Shelf ID was derived
    pub fn source(&self) -> PowerShelfIdSource {
        self.source
    }

    /// The type of the Power Shelf
    pub fn power_shelf_type(&self) -> PowerShelfType {
        self.ty
    }

    /// Generate Remote ID based on powerShelfID.
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
    /// no logical reason to construct a "default" PowerShelfId in real code, so
    /// we simply construct a bogus one here.
    #[allow(clippy::should_implement_trait)]
    #[deprecated(
        note = "Do not use `PowerShelfId::default()` directly; only implemented for prost interop"
    )]
    pub fn default() -> Self {
        Self::new(
            PowerShelfIdSource::ProductBoardChassisSerial,
            [0; 32],
            PowerShelfType::Host,
        )
    }

    pub(crate) fn is_matching_prefix(s: &str) -> bool {
        s.starts_with(POWER_SHELF_ID_PREFIX)
    }
}

impl DbPrimaryUuid for PowerShelfId {
    fn db_primary_uuid_name() -> &'static str {
        "power_shelf_id"
    }
}

impl From<uuid::Uuid> for PowerShelfId {
    fn from(value: uuid::Uuid) -> Self {
        // This is a fallback implementation - in practice, PowerShelfId should be created
        // from hardware hashes, not random UUIDs
        let mut hasher = Sha256::new();
        hasher.update(value.as_bytes());
        let hash: [u8; 32] = hasher.finalize().into();

        Self::new(
            PowerShelfIdSource::Tpm, // Default source
            hash,
            PowerShelfType::Rack, // Default type
        )
    }
}

/// The hardware source from which the Power Shelf ID is derived
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PowerShelfIdSource {
    /// The Power Shelf ID was generated by hashing the TPM EkCertificate data.
    Tpm,
    /// The Power Shelf ID was generated by the concatenation of product, board and chassis serial
    /// and hashing the resulting value.
    /// If any of those values is not available in DMI data, an empty
    /// string will be used instead. At least one serial number must have been
    /// available to generate this ID.
    ProductBoardChassisSerial,
}

impl PowerShelfIdSource {
    /// Returns the character that identifies the source type
    pub const fn id_char(self) -> char {
        match self {
            PowerShelfIdSource::Tpm => 't',
            PowerShelfIdSource::ProductBoardChassisSerial => 's',
        }
    }

    /// Parses the `PowerShelfIdSource` from a character
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

/// Extra flags that are associated with the power shelf ID
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PowerShelfType {
    /// The Power Shelf is a Rack
    Rack,
    /// The Power Shelf is a Host
    Host,
}

impl PowerShelfType {
    /// Returns `true` if the Power Shelf is a Rack
    pub fn is_rack(self) -> bool {
        self == PowerShelfType::Rack
    }

    /// Returns `true` if the Power Shelf is a Host
    pub fn is_host(self) -> bool {
        self == PowerShelfType::Host
    }
}

impl std::fmt::Display for PowerShelfType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowerShelfType::Rack => f.write_str("Rack"),
            PowerShelfType::Host => f.write_str("Host"),
        }
    }
}

impl PowerShelfType {
    /// Returns the character that identifies the flag
    pub const fn id_char(self) -> char {
        match self {
            PowerShelfType::Rack => 'r',
            PowerShelfType::Host => 'h',
        }
    }

    /// Parses the `PowerShelfType` from a character
    pub fn from_id_char(c: char) -> Option<Self> {
        match c {
            c if c == Self::Rack.id_char() => Some(Self::Rack),
            c if c == Self::Host.id_char() => Some(Self::Host),
            _ => None,
        }
    }
}

impl std::fmt::Display for PowerShelfId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ps` is for power-shelf
        // `1` is a version identifier
        // The next 2 bytes `00` are reserved
        f.write_str(POWER_SHELF_ID_PREFIX)?;
        // Write the power shelf type
        f.write_char(self.ty.id_char())?;
        // The next character determines how the PowerShelfId is derived (`PowerShelfIdSource`)
        f.write_char(self.source.id_char())?;
        // Then follows the source data.
        // SAFETY: `hardware_id` is private and populated only from `BASE32_DNSSEC::encode`, whose
        // output is ASCII and therefore valid UTF-8.
        unsafe { f.write_str(std::str::from_utf8_unchecked(self.hardware_id.as_slice())) }
    }
}

/// The length that is used for the prefix in Power Shelf IDs
pub const POWER_SHELF_ID_PREFIX_LENGTH: usize = 7;

/// The length of the hardware ID substring embedded in the Power Shelf ID
///
/// Since it's a base32 encoded SHA256 (32byte), this makes 52 bytes
pub const POWER_SHELF_ID_HARDWARE_ID_BASE32_LENGTH: usize = 52;

/// The length of a valid PowerShelfID
///
/// It is made up of the prefix length (5 bytes) plus the encoded hardware ID length
pub const POWER_SHELF_ID_LENGTH: usize =
    POWER_SHELF_ID_PREFIX_LENGTH + POWER_SHELF_ID_HARDWARE_ID_BASE32_LENGTH;

#[derive(thiserror::Error, Debug, Clone)]
pub enum PowerShelfIdParseError {
    #[error("the power shelf ID has an invalid length of {0}")]
    Length(usize),
    #[error("the power shelf ID {0} has an invalid prefix")]
    Prefix(String),
    #[error("the power shelf ID {0} has an invalid encoding")]
    Encoding(String),
}

impl FromStr for PowerShelfId {
    type Err = PowerShelfIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != POWER_SHELF_ID_LENGTH {
            return Err(PowerShelfIdParseError::Length(s.len()));
        }
        // Check for version 1 and 2 reserved bytes
        if !s.starts_with(POWER_SHELF_ID_PREFIX) {
            return Err(PowerShelfIdParseError::Prefix(s.to_string()));
        }

        // Everything after the prefix needs to be valid base32
        let hardware_id = &s.as_bytes()[POWER_SHELF_ID_PREFIX_LENGTH..];

        let mut hardware_hash: HardwareHash = [0u8; 32];
        match BASE32_DNSSEC.decode_mut(hardware_id, &mut hardware_hash) {
            Err(_) => return Err(PowerShelfIdParseError::Encoding(s.to_string())),
            Ok(size) if size != 32 => return Err(PowerShelfIdParseError::Encoding(s.to_string())),
            _ => {}
        }

        let ty = PowerShelfType::from_id_char(s.as_bytes()[5] as char)
            .ok_or_else(|| PowerShelfIdParseError::Prefix(s.to_string()))?;
        let source = PowerShelfIdSource::from_id_char(s.as_bytes()[6] as char)
            .ok_or_else(|| PowerShelfIdParseError::Prefix(s.to_string()))?;

        Ok(PowerShelfId::new(source, hardware_hash, ty))
    }
}

impl Serialize for PowerShelfId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PowerShelfId {
    fn deserialize<D>(deserializer: D) -> Result<PowerShelfId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let id =
            PowerShelfId::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(id)
    }
}

// Implement [`prost::Message`] manually so that we can be wire-compatible with the
// `.common.PowerShelfId` protobuf message, which is what we actually serialize. Do this by
// constructing a `legacy_rpc::PowerShelfId` and delegate all  [`prost::Message`] methods to it.
impl prost::Message for PowerShelfId {
    fn encode_raw(&self, buf: &mut impl BufMut)
    where
        Self: Sized,
    {
        legacy_rpc::PowerShelfId::from(*self).encode_raw(buf);
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
        let mut legacy_message = legacy_rpc::PowerShelfId::from(*self);
        legacy_message.merge_field(tag, wire_type, buf, ctx)?;
        *self = PowerShelfId::from_str(&legacy_message.id).map_err(|_| {
            // Deprecation: if they remove DecodeError::new, they hopefully will provide some other way
            // to impl prost::Message.
            #[allow(deprecated)]
            DecodeError::new(format!("Invalid power shelf id: {}", legacy_message.id))
        })?;
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        legacy_rpc::PowerShelfId::from(*self).encoded_len()
    }

    #[allow(deprecated)]
    fn clear(&mut self) {
        *self = PowerShelfId::default();
    }
}

mod legacy_rpc {
    /// Backwards compatibility shim for [`super::PowerShelfId`] to be sent as a protobuf message
    /// in a way that is compatible with the `.common.PowerShelfId` message, which is defined as:
    ///
    /// ```ignore
    /// message PowerShelfId {
    ///     string id = 1;
    /// }
    /// ```
    ///
    /// This allows us to use [`super::PowerShelfId`] directly instead of having to convert it
    /// manually every time, while still interacting with peers that expect a `.common.PowerShelfId`
    /// to be serialized.
    #[derive(prost::Message)]
    pub(super) struct PowerShelfId {
        #[prost(string, tag = "1")]
        pub(super) id: String,
    }

    impl From<super::PowerShelfId> for PowerShelfId {
        fn from(value: crate::power_shelf::PowerShelfId) -> Self {
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

    fn parse_power_shelf_id(input: &str) -> Result<String, ParseFailure> {
        PowerShelfId::from_str(input)
            .map(|id| id.to_string())
            .map_err(|err| match err {
                PowerShelfIdParseError::Length(_) => ParseFailure::Length,
                PowerShelfIdParseError::Prefix(_) => ParseFailure::Prefix,
                PowerShelfIdParseError::Encoding(_) => ParseFailure::Encoding,
            })
    }

    #[test]
    fn test_power_shelf_id_parse_cases() {
        const VALID_POWER_SHELF_ID: &str =
            "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

        scenarios!(
            run = parse_power_shelf_id;
            "valid host TPM power shelf ID" {
                VALID_POWER_SHELF_ID => Yields(VALID_POWER_SHELF_ID.to_string()),
            }

            "one character short" {
                "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hc" => FailsWith(ParseFailure::Length),
            }

            "empty string" {
                "" => FailsWith(ParseFailure::Length),
            }

            "invalid prefix casing" {
                "PS100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid power shelf type" {
                "ps100xt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid source" {
                "ps100dx038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Prefix),
            }

            "invalid base32 payload" {
                "ps100ht038bg3qsho433vkg684heguv28!qaggmrsh2ugn1qk096n2c6hcg" => FailsWith(ParseFailure::Encoding),
            }
        );
    }

    #[test]
    fn test_power_shelf_type_mappings() {
        value_scenarios!(
            run = |ty| (ty.id_char(), ty.to_string(), ty.is_rack(), ty.is_host());
            "rack" {
                PowerShelfType::Rack => ('r', "Rack".to_string(), true, false),
            }

            "host" {
                PowerShelfType::Host => ('h', "Host".to_string(), false, true),
            }
        );
    }

    #[test]
    fn test_power_shelf_type_from_id_char() {
        value_scenarios!(
            run = PowerShelfType::from_id_char;
            "rack" {
                'r' => Some(PowerShelfType::Rack),
            }

            "host" {
                'h' => Some(PowerShelfType::Host),
            }

            "unknown" {
                'x' => None,
            }
        );
    }

    #[test]
    fn test_power_shelf_id_source_from_id_char() {
        value_scenarios!(
            run = PowerShelfIdSource::from_id_char;
            "TPM" {
                't' => Some(PowerShelfIdSource::Tpm),
            }

            "product board chassis serial" {
                's' => Some(PowerShelfIdSource::ProductBoardChassisSerial),
            }

            "unknown" {
                'x' => None,
            }
        );
    }
}
