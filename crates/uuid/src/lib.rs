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
pub mod domain;
pub mod dpa_interface;
pub mod dpu_remediations;
pub mod extension_service;
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
}

/// `DbColumns` formats trusted SQL column names without allocating an
/// intermediate joined string. Names are written verbatim in slice order,
/// separated by `", "`; an empty list formats as an empty string.
pub struct DbColumns(&'static [&'static str]);

impl DbColumns {
    /// `new` wraps static SQL identifiers without validating or quoting them.
    /// Callers must supply trusted, unqualified names required by their
    /// record's decoder, not user input or `*`.
    pub const fn new(columns: &'static [&'static str]) -> Self {
        Self(columns)
    }

    /// `as_slice` returns the original column names in their declared order.
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

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::DbColumns;

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
