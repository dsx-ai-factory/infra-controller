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

pub use uuid::Error as UuidError;
use uuid::Uuid;

pub trait UuidSubtype {
    const TYPE_NAME: &'static str;

    // This is used by the FromRow implementation to load the type from a row,
    // so if the UUID we're loading from lives in a column with a different
    // name, this should be customized.
    const DB_COLUMN_NAME: &'static str = "id";
}

/// This is a more strongly-typed UUID that can be used for things that are
/// stored in the database with a UUID, but where we'd like to keep track of
/// what sort of resource they're a UUID for.
///
/// In order to use it, just make a new marker type and implement `UuidSubtype`
/// for it:
///
/// type ExampleId = TypedUuid<ExampleFlavor>;
/// struct ExampleFlavor {};
/// impl UuidSubtype for ExampleFlavor {
///     const TYPE_NAME: &str = "ExampleId"
/// }
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TypedUuid<T: UuidSubtype> {
    uuid: Uuid,
    #[serde(skip)]
    _marker: std::marker::PhantomData<T>,
}

impl<T: UuidSubtype + Send + Sync> prost::Message for TypedUuid<T> {
    fn encode_raw(&self, buf: &mut impl prost::bytes::BufMut) {
        let tmp = crate::CommonUuidPlaceholder {
            value: self.uuid.to_string(),
        };
        // Delegate to prost for the actual encoding of the shim.
        prost::Message::encode_raw(&tmp, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: prost::encoding::WireType,
        buf: &mut impl prost::bytes::Buf,
        ctx: prost::encoding::DecodeContext,
    ) -> Result<(), prost::DecodeError> {
        // Decode through the shim type, which has the identical wire layout.
        let mut tmp = crate::CommonUuidPlaceholder::default();
        prost::Message::merge_field(&mut tmp, tag, wire_type, buf, ctx)?;
        // Deprecation: if they remove DecodeError::new, they hopefully will provide some other way
        // to impl prost::Message.
        #[allow(deprecated)]
        let parsed = uuid::Uuid::parse_str(&tmp.value)
            .map_err(|_| prost::DecodeError::new(format!("invalid UUID: {}", tmp.value)))?;
        *self = parsed.into();
        Ok(())
    }

    fn encoded_len(&self) -> usize {
        let tmp = crate::CommonUuidPlaceholder {
            value: self.uuid.to_string(),
        };
        prost::Message::encoded_len(&tmp)
    }

    fn clear(&mut self) {
        *self = uuid::Uuid::default().into();
    }
}

impl<T> TypedUuid<T>
where
    T: UuidSubtype,
{
    /// Creates a nil (all zeros) TypedUuid. This is a const function so it can
    /// be used to initialize constants.
    pub const fn nil() -> Self {
        Self {
            uuid: Uuid::nil(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Creates a new random v4 TypedUuid.
    pub fn new() -> Self {
        Uuid::new_v4().into()
    }

    fn try_parse(input: &str) -> Result<Self, UuidError> {
        Uuid::try_parse(input).map(|uuid| Self {
            uuid,
            _marker: std::marker::PhantomData,
        })
    }

    /// Returns a new TypedUuid with its underlying u128 value offset by `n`.
    /// Useful for creating sequential UUIDs in tests.
    pub fn offset(self, n: u128) -> Self {
        Uuid::from_u128(self.uuid.as_u128() + n).into()
    }
}

// We manually implement Copy to avoid the auto-derived implementation's bound
// on T also being Copy (our T is a PhantomData so we don't need or want that).
impl<T> Copy for TypedUuid<T> where T: UuidSubtype {}

impl<T> Clone for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Default for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn default() -> Self {
        uuid::Uuid::default().into()
    }
}

impl<T> std::fmt::Display for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.uuid.fmt(f)
    }
}

impl<T> PartialEq for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn eq(&self, other: &Self) -> bool {
        self.uuid.eq(&other.uuid)
    }
}

impl<T> Eq for TypedUuid<T> where T: UuidSubtype {}

impl<T> PartialOrd for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.uuid.cmp(&other.uuid)
    }
}

impl<T> std::hash::Hash for TypedUuid<T>
where
    T: 'static + UuidSubtype,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
        // We also hash over T's TypeId, just in case someone's comparing hashes
        // that came from two different TypedUuid subtypes with an identical
        // inner UUID.
        std::any::TypeId::of::<T>().hash(state);
    }
}

impl<T> std::fmt::Debug for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(T::TYPE_NAME)
            .field("uuid", &self.uuid)
            .finish()
    }
}

impl<T> From<TypedUuid<T>> for Uuid
where
    T: UuidSubtype,
{
    fn from(typed_id: TypedUuid<T>) -> Self {
        typed_id.uuid
    }
}

impl<T> From<Uuid> for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn from(uuid: Uuid) -> Self {
        Self {
            uuid,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> From<TypedUuid<T>> for String
where
    T: UuidSubtype,
{
    fn from(typed: TypedUuid<T>) -> String {
        typed.to_string()
    }
}

impl<T> std::str::FromStr for TypedUuid<T>
where
    T: UuidSubtype,
{
    type Err = UuidError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::try_parse(input)
    }
}

#[cfg(feature = "sqlx")]
impl<T, DB> sqlx::Type<DB> for TypedUuid<T>
where
    T: UuidSubtype,
    DB: sqlx::Database,
    Uuid: sqlx::Type<DB>,
{
    fn type_info() -> <DB as sqlx::Database>::TypeInfo {
        Uuid::type_info()
    }
}

#[cfg(feature = "sqlx")]
impl<'a, T, DB> sqlx::Decode<'a, DB> for TypedUuid<T>
where
    T: UuidSubtype,
    DB: sqlx::Database,
    Uuid: sqlx::Decode<'a, DB>,
{
    fn decode(
        value: <DB as sqlx::Database>::ValueRef<'a>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        Uuid::decode(value).map(Self::from)
    }
}

#[cfg(feature = "sqlx")]
impl<'a, T, DB> sqlx::Encode<'a, DB> for TypedUuid<T>
where
    T: UuidSubtype,
    DB: sqlx::Database,
    Uuid: sqlx::Encode<'a, DB>,
{
    fn encode_by_ref(
        &self,
        buf: &mut <DB as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        self.uuid.encode_by_ref(buf)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, R, T> sqlx::FromRow<'r, R> for TypedUuid<T>
where
    T: UuidSubtype,
    R: sqlx::Row,
    Self: sqlx::Type<<R>::Database>,
    Self: sqlx::Decode<'r, <R>::Database>,
    &'static str: sqlx::ColumnIndex<R>,
{
    fn from_row(row: &'r R) -> Result<Self, sqlx::Error> {
        row.try_get(T::DB_COLUMN_NAME)
    }
}

#[cfg(feature = "sqlx")]
impl<T> sqlx::postgres::PgHasArrayType for TypedUuid<T>
where
    T: UuidSubtype,
{
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::types::Uuid::array_type_info()
    }
}

impl<T: UuidSubtype> crate::DbPrimaryUuid for TypedUuid<T> {
    fn db_primary_uuid_name() -> &'static str {
        T::DB_COLUMN_NAME
    }
}

/// typed_uuid_tests generates standard tests for all IDS
/// deriving from the TypedUuid type. This includes:
///
/// - UUID round-trip conversion.
/// - String round-trip (to_string/FromStr).
/// - JSON serialization round-trip.
/// - Ordering (nil < max).
/// - Default value (equals nil UUID).
/// - Copy semantics.
/// - Hash consistency.
/// - Debug output includes type name.
/// - DB column name.
/// - Into<String> conversion.
///
/// Usage:
///   typed_uuid_tests!(YourUuid, "<TYPE_NAME>", "<DB_COLUMN_NAME>");
///   typed_uuid_tests!(YourUuid, "YourUuid", "your_id");
#[macro_export]
macro_rules! typed_uuid_tests {
    ($type:ty, $type_name:expr, $db_column:expr) => {
        #[test]
        fn test_uuid_round_trip() {
            let orig = uuid::Uuid::new_v4();
            let id = <$type>::from(orig);
            let back = uuid::Uuid::from(id);
            assert_eq!(orig, back);
        }

        #[test]
        fn test_string_round_trip() {
            use std::str::FromStr;
            let orig = uuid::Uuid::new_v4();
            let id = <$type>::from(orig);
            let as_string = id.to_string();
            let parsed = <$type>::from_str(&as_string).expect("failed to parse");
            assert_eq!(id, parsed);
        }

        #[test]
        fn test_json_round_trip() {
            let id = <$type>::new();
            let json = serde_json::to_string(&id).expect("failed to serialize");
            let parsed: $type = serde_json::from_str(&json).expect("failed to deserialize");
            assert_eq!(id, parsed);
            // Ensure it serializes as a plain string, not a nested object.
            assert!(json.starts_with('"') && json.ends_with('"'));
        }

        #[test]
        fn test_ordering() {
            let id1 = <$type>::from(uuid::Uuid::nil());
            let id2 = <$type>::from(uuid::Uuid::max());
            assert!(id1 < id2);
        }

        #[test]
        fn test_default() {
            let id = <$type>::default();
            assert_eq!(uuid::Uuid::from(id), uuid::Uuid::nil());
        }

        #[test]
        fn test_copy() {
            let id1 = <$type>::new();
            let id2 = id1; // This should copy.
            assert_eq!(id1, id2);
        }

        #[test]
        fn test_hash_consistency() {
            use std::collections::HashSet;
            let uuid = uuid::Uuid::new_v4();
            let id1 = <$type>::from(uuid);
            let id2 = <$type>::from(uuid);
            let mut set = HashSet::new();
            set.insert(id1);
            assert!(set.contains(&id2));
        }

        #[test]
        fn test_debug_includes_type_name() {
            let id = <$type>::from(uuid::Uuid::nil());
            let debug = format!("{:?}", id);
            assert!(
                debug.contains($type_name),
                "Debug output '{}' should contain '{}'",
                debug,
                $type_name
            );
        }

        #[test]
        // TODO(chet): It might be nice to actually make this
        // an sqlx test that creates a test table with a column
        // and make sure everything checks out.
        fn test_db_column_name() {
            use $crate::DbPrimaryUuid;
            assert_eq!(
                <$type>::db_primary_uuid_name(),
                $db_column,
                "DB_COLUMN_NAME should be '{}'",
                $db_column
            );
        }

        #[test]
        fn test_into_string() {
            let uuid = uuid::Uuid::new_v4();
            let id = <$type>::from(uuid);
            let s: String = id.into();
            assert_eq!(s, uuid.to_string());
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    type ThingyId = TypedUuid<ThingyFlavor>;
    struct ThingyFlavor {}
    impl UuidSubtype for ThingyFlavor {
        const TYPE_NAME: &'static str = "ThingyId";
    }

    typed_uuid_tests!(ThingyId, "ThingyId", "id");

    #[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
    struct ThingyWithId {
        id: ThingyId,
        name: String,
    }

    #[test]
    fn test_json_struct_embedding() {
        let before = ThingyWithId {
            id: ThingyId::from(Uuid::nil()),
            name: String::from("Hello"),
        };

        let serialized =
            serde_json::to_string(&before).expect("Couldn't serialize ThingyWithId to JSON");

        // Ensure the serialized representation doesn't have any extra nesting.
        // It should look like a regular UUID on the wire.
        assert_eq!(
            serialized.as_str(),
            r#"{"id":"00000000-0000-0000-0000-000000000000","name":"Hello"}"#
        );

        let after: ThingyWithId = serde_json::from_str(serialized.as_str())
            .expect("Couldn't deserialize ThingyWithId from JSON");
        assert_eq!(&before, &after);
    }
}
