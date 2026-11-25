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

pub mod compute_allocation;
pub mod device;
pub mod dhcp_option_set;
pub mod dns_zone;
pub mod domain;
pub mod dpa_interface;
pub mod dpu_remediations;
pub mod extension_service;
/// Typed identities for the external-validation workflow.
pub mod external_validation;
pub mod infiniband;
pub mod instance;
pub mod instance_type;
pub mod ipxe_template;
pub mod machine;
pub mod machine_validation;
pub mod measured_boot;
pub mod network;
pub mod network_security_group;
pub mod nvlink;
pub mod operating_system;
pub mod power_shelf;
pub mod rack;
pub mod secret;
pub mod site_prefix;
pub mod spx;
pub mod switch;
pub mod typed_uuids;
pub mod vpc;
pub mod vpc_peering;

/// DbPrimaryUuid is a trait intended for primary keys which
/// derive the sqlx UUID type. The intent is the db_primary_uuid_name
/// function should return the name of the column for the primary
/// UUID-typed key, which allows dynamic compositon of a SQL query.
///
/// This was originally introduced as part of the measured boot
/// generics (and lived in src/measured_boot/), but moved here.
pub trait DbPrimaryUuid {
    fn db_primary_uuid_name() -> &'static str;
}

/// `DbTable` identifies the table and columns for records decoded by
/// `sqlx::FromRow`, allowing queries to use the record's column contract
/// without including unrelated columns from the table.
///
/// Records with direct field-to-column mappings can derive this trait with
/// `carbide_macros::DbTable`; other records can implement it explicitly.
///
/// This was originally introduced as part of the measured boot
/// generics (and lived in src/measured_boot/), but moved here.
pub trait DbTable {
    fn db_table_name() -> &'static str;

    /// `db_table_columns` returns the trusted, unqualified column names
    /// required by this record's decoder, in a fixed order. Format the
    /// returned `DbColumns` directly for `SELECT` and `RETURNING` clauses;
    /// these are SQL identifiers, not bound data. Do not include `*`:
    /// unrelated added columns must not change a cached query's result type.
    fn db_table_columns() -> DbColumns;

    /// `db_aliased_table` returns the table reference for a `FROM` or `JOIN`
    /// clause, such as `tenant_identity_config AS tic`. Without a declared
    /// alias, it returns `db_table_name()` unchanged.
    /// To declare an alias, manual implementations must override both
    /// aliased helpers with that same alias.
    fn db_aliased_table() -> &'static str {
        Self::db_table_name()
    }

    /// `db_aliased_columns` returns the decoder's columns qualified by the
    /// alias declared in `db_aliased_table`, in the same order as
    /// `db_table_columns`. Qualification does not rename result fields.
    /// Without a declared alias, it returns the unqualified columns.
    fn db_aliased_columns() -> DbColumns {
        Self::db_table_columns()
    }
}

/// `DbColumns` formats trusted SQL column references without allocating an
/// intermediate joined string. References are written verbatim in slice order,
/// separated by `", "`; an empty list formats as an empty string.
pub struct DbColumns(&'static [&'static str]);

impl DbColumns {
    /// `new` wraps static SQL column references without validating or quoting them.
    /// Callers must supply trusted column references required by their
    /// record's decoder, optionally qualified by a table alias, not user
    /// input or `*`.
    pub const fn new(columns: &'static [&'static str]) -> Self {
        Self(columns)
    }

    /// `as_slice` returns the original column references in their declared order.
    pub const fn as_slice(&self) -> &'static [&'static str] {
        self.0
    }
}

impl std::fmt::Display for DbColumns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some((first, rest)) = self.0.split_first() else {
            return Ok(());
        };
        f.write_str(first)?;
        for column in rest {
            f.write_str(", ")?;
            f.write_str(column)?;
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum UuidConversionError {
    #[error("invalid UUID for {ty}: {value}")]
    InvalidUuid { ty: &'static str, value: String },
    #[error("missing ID for {0}")]
    MissingId(&'static str),
    #[error("invalid MachineId: {0}")]
    InvalidMachineId(String),
    #[error("UUID parse error: {0}")]
    UuidError(#[from] uuid::Error),
}

#[derive(
    Ord,
    PartialOrd,
    serde::Deserialize,
    serde::Serialize,
    Clone,
    PartialEq,
    Eq,
    Hash,
    ::prost::Message,
)]
pub(crate) struct CommonUuidPlaceholder {
    #[prost(string, tag = "1")]
    pub value: ::prost::alloc::string::String,
}

// Length of a tag-1 protobuf string field, including proto3's omission of empty strings.
fn string_field_len(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    prost::encoding::key_len(1) + prost::encoding::encoded_len_varint(len as u64) + len
}

fn deserialize_from_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    D: serde::Deserializer<'de>,
{
    struct StringVisitor<T>(std::marker::PhantomData<T>);

    impl<'de, T> serde::de::Visitor<'de> for StringVisitor<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        type Value = Result<T, T::Err>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value.parse())
        }

        fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
            match std::str::from_utf8(value) {
                Ok(value) => self.visit_str(value),
                Err(_) => Err(E::invalid_value(serde::de::Unexpected::Bytes(value), &self)),
            }
        }
    }

    // Convert domain errors after the string read to preserve diagnostic positions.
    deserializer
        .deserialize_string(StringVisitor(std::marker::PhantomData))?
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn protobuf_string_identifiers_match_their_reference_message() {
        fn check<T>(id: T, text: &str)
        where
            T: prost::Message + Default + PartialEq + std::fmt::Debug,
        {
            let reference = CommonUuidPlaceholder {
                value: text.to_owned(),
            };
            let encoded = id.encode_to_vec();
            assert_eq!(id.encoded_len(), encoded.len());
            assert_eq!(encoded, prost::Message::encode_to_vec(&reference));
            assert_eq!(T::decode(encoded.as_slice()).unwrap(), id);
        }

        let machine = "fm100dt0000000000000000000000000000000000000000000000000000";
        check(machine.parse::<machine::MachineId>().unwrap(), machine);
        check(machine.parse::<machine::DpuMachineId>().unwrap(), machine);
        let switch = "sw100nt0000000000000000000000000000000000000000000000000000";
        check(switch.parse::<switch::SwitchId>().unwrap(), switch);
        let shelf = "ps100rt0000000000000000000000000000000000000000000000000000";
        check(shelf.parse::<power_shelf::PowerShelfId>().unwrap(), shelf);
        let uuid = "00000000-0000-0000-0000-000000000000";
        check(uuid.parse::<instance::InstanceId>().unwrap(), uuid);
        for text in ["", "rack-α", &"r".repeat(128)] {
            check(rack::RackId::new(text), text);
            check(rack::RackGroupId::new(text), text);
            check(rack::RackProfileId::new(text), text);
        }
    }

    #[cfg(feature = "sqlx")]
    #[test]
    fn sql_identifiers_append_canonical_text() {
        fn encode<T>(text: &str) -> Vec<u8>
        where
            T: std::str::FromStr + for<'q> sqlx::Encode<'q, sqlx::Postgres>,
            T::Err: std::fmt::Debug,
        {
            let mut buffer = sqlx::postgres::PgArgumentBuffer::default();
            buffer.extend_from_slice(b"prefix:");
            assert!(matches!(
                sqlx::Encode::encode_by_ref(&text.parse::<T>().unwrap(), &mut buffer).unwrap(),
                sqlx::encode::IsNull::No
            ));
            buffer.to_vec()
        }

        type EncodeFn = fn(&str) -> Vec<u8>;
        let machine = "fm100dt0000000000000000000000000000000000000000000000000000";
        let switch = "sw100nt0000000000000000000000000000000000000000000000000000";
        let shelf = "ps100rt0000000000000000000000000000000000000000000000000000";
        for (scenario, text, encode) in [
            ("machine", machine, encode::<machine::MachineId> as EncodeFn),
            (
                "machine subtype",
                machine,
                encode::<machine::DpuMachineId> as EncodeFn,
            ),
            ("switch", switch, encode::<switch::SwitchId> as EncodeFn),
            (
                "power shelf",
                shelf,
                encode::<power_shelf::PowerShelfId> as EncodeFn,
            ),
            ("device", switch, encode::<device::DeviceId> as EncodeFn),
            (
                "trusted machine",
                machine,
                encode::<measured_boot::TrustedMachineId> as EncodeFn,
            ),
            (
                "all trusted machines",
                "*",
                encode::<measured_boot::TrustedMachineId> as EncodeFn,
            ),
            (
                "instance type",
                "type-α",
                encode::<instance_type::InstanceTypeId> as EncodeFn,
            ),
            (
                "security group",
                "group-α",
                encode::<network_security_group::NetworkSecurityGroupId> as EncodeFn,
            ),
        ] {
            assert_eq!(
                encode(text),
                format!("prefix:{text}").into_bytes(),
                "{scenario}"
            );
        }
    }

    #[test]
    fn structured_id_deserialization_accepts_reader_and_byte_inputs() {
        use serde::Deserialize;
        use serde::de::value::{BytesDeserializer, Error};

        let text = "fm100dt0000000000000000000000000000000000000000000000000000";
        let expected = text.parse::<machine::MachineId>().unwrap();
        let json = format!("\"{text}\"");
        assert_eq!(
            serde_json::from_reader::<_, machine::MachineId>(json.as_bytes()).unwrap(),
            expected
        );
        assert_eq!(
            machine::MachineId::deserialize(BytesDeserializer::<Error>::new(text.as_bytes()))
                .unwrap(),
            expected
        );
        let bytes = &[0xff];
        let expected_error =
            String::deserialize(BytesDeserializer::<Error>::new(bytes)).unwrap_err();
        let actual_error =
            machine::MachineId::deserialize(BytesDeserializer::<Error>::new(bytes)).unwrap_err();
        assert_eq!(actual_error.to_string(), expected_error.to_string());
    }

    #[test]
    fn manual_db_table_without_alias_uses_existing_metadata() {
        struct Record;

        impl DbTable for Record {
            fn db_table_name() -> &'static str {
                "records"
            }

            fn db_table_columns() -> DbColumns {
                DbColumns::new(&["id"])
            }
        }

        assert_eq!(Record::db_aliased_table(), Record::db_table_name());
        assert_eq!(
            Record::db_aliased_columns().as_slice(),
            Record::db_table_columns().as_slice(),
        );
    }

    #[test]
    fn db_columns_display_uses_comma_space_separators() {
        value_scenarios!(run = |columns: DbColumns| columns.to_string();
            "no columns" {
                DbColumns::new(&[]) => String::new(),
            }
            "one column has no separator" {
                DbColumns::new(&["id"]) => "id".to_string(),
            }
            "multiple columns retain their order" {
                DbColumns::new(&["z_value", "first"]) => "z_value, first".to_string(),
            }
        );
    }
}
