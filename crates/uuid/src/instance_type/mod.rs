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

use serde::{Deserialize, Serialize};
#[cfg(feature = "sqlx")]
use sqlx::{
    Database, Decode, Encode, Error, Postgres, Row,
    encode::IsNull,
    error,
    postgres::{PgHasArrayType, PgRow, PgTypeInfo},
};
use uuid::Uuid;

#[derive(thiserror::Error, Debug, Clone)]
pub enum InstanceTypeIdParseError {
    #[error("InstanceTypeId has an invalid value {0}")]
    Invalid(String),
    #[error("InstanceTypeId value must not be empty")]
    Empty,
}

impl InstanceTypeIdParseError {
    pub fn value(self) -> String {
        match self {
            InstanceTypeIdParseError::Invalid(v) => v,
            InstanceTypeIdParseError::Empty => String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstanceTypeId {
    value: String,
}

/* ********************************************* */
/*  Basic trait implementations for conversions  */
/* ********************************************* */

impl fmt::Display for InstanceTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl FromStr for InstanceTypeId {
    type Err = InstanceTypeIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(InstanceTypeIdParseError::Empty);
        }

        Ok(InstanceTypeId {
            value: s.to_string(),
        })
    }
}

impl From<Uuid> for InstanceTypeId {
    fn from(u: Uuid) -> Self {
        InstanceTypeId {
            value: u.to_string(),
        }
    }
}

/* ********************************************* */
/*           SQLX trait implementations          */
/* ********************************************* */

#[cfg(feature = "sqlx")]
impl Encode<'_, sqlx::Postgres> for InstanceTypeId {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, error::BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(IsNull::No)
    }

    fn encode(
        self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, error::BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> Decode<'r, DB> for InstanceTypeId
where
    DB: Database,
    String: Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let str_id: String = String::decode(value)?;
        Ok(InstanceTypeId::from_str(&str_id).map_err(|e| Error::Decode(Box::new(e)))?)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, PgRow> for InstanceTypeId {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(InstanceTypeId {
            value: row.try_get("id")?,
        })
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for InstanceTypeId
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
impl PgHasArrayType for InstanceTypeId {
    fn array_type_info() -> PgTypeInfo {
        <&str as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <&str as PgHasArrayType>::array_compatible(ty)
    }
}

/* ********************************************* */
/*          Serde trait implementations          */
/* ********************************************* */

impl Serialize for InstanceTypeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for InstanceTypeId {
    fn deserialize<D>(deserializer: D) -> Result<InstanceTypeId, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let id =
            InstanceTypeId::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum ParseFailure {
        Empty,
    }

    fn parse_instance_type_id(input: &str) -> Result<String, ParseFailure> {
        InstanceTypeId::from_str(input)
            .map(|id| id.to_string())
            .map_err(|err| match err {
                InstanceTypeIdParseError::Invalid(_) => unreachable!("parser only rejects empty"),
                InstanceTypeIdParseError::Empty => ParseFailure::Empty,
            })
    }

    fn deserialize_instance_type_id(input: &str) -> Result<String, ()> {
        serde_json::from_str::<InstanceTypeId>(input)
            .map(|id| id.to_string())
            .map_err(|_| ())
    }

    #[test]
    fn test_instance_type_id_parse_cases() {
        scenarios!(
            run = parse_instance_type_id;
            "arbitrary instance type name" {
                "gb200-nvl72" => Yields("gb200-nvl72".to_string()),
            }

            "UUID-backed instance type name" {
                "00000000-0000-0000-0000-000000000000" => Yields("00000000-0000-0000-0000-000000000000".to_string()),
            }

            "empty value" {
                "" => FailsWith(ParseFailure::Empty),
            }
        );
    }

    #[test]
    fn test_instance_type_id_from_uuid() {
        value_scenarios!(
            run = |uuid| InstanceTypeId::from(uuid).to_string();
            "nil UUID" {
                Uuid::nil() => "00000000-0000-0000-0000-000000000000".to_string(),
            }
        );
    }

    #[test]
    fn test_instance_type_id_serde_cases() {
        scenarios!(
            run = deserialize_instance_type_id;
            "valid string" {
                "\"gb200-nvl72\"" => Yields("gb200-nvl72".to_string()),
            }

            "empty string" {
                "\"\"" => Fails,
            }

            "non-string JSON" {
                "42" => Fails,
            }
        );

        let serialized = serde_json::to_string(
            &InstanceTypeId::from_str("gb200-nvl72").expect("valid instance type ID"),
        )
        .expect("failed to serialize instance type ID");
        assert_eq!(serialized, "\"gb200-nvl72\"");
    }

    #[test]
    fn test_instance_type_id_parse_error_value() {
        value_scenarios!(
            run = InstanceTypeIdParseError::value;
            "invalid" {
                InstanceTypeIdParseError::Invalid("bad".to_string()) => "bad".to_string(),
            }

            "empty" {
                InstanceTypeIdParseError::Empty => String::new(),
            }
        );
    }
}
