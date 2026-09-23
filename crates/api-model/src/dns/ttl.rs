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

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A zone's default record TTL in seconds, the equivalent of a zone file's
/// `$TTL` directive. A zone without one serves the site default of 300
/// seconds. The SOA's own fields are separate: `minimum` remains the
/// negative-caching TTL.
///
/// The range is operational, not protocol. `nico-dns` has no positive cache,
/// so the record TTL is the only thing between a busy resolver and `nico-api`
/// on every query; the floor keeps a chatty client from becoming database
/// load. The ceiling bounds how long a stale answer survives after an
/// address is released or a name changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct ZoneTtl(u32);

/// Why a value is not a valid [`ZoneTtl`]. Carries the offending value as
/// `i64` so a negative stored integer is reported as itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("zone TTL {0} is outside {min}..={max} seconds", min = ZoneTtl::MIN_SECS, max = ZoneTtl::MAX_SECS)]
pub struct ZoneTtlError(i64);

impl ZoneTtl {
    /// Shortest permitted default TTL.
    pub const MIN_SECS: u32 = 30;
    /// Longest permitted default TTL: one day.
    pub const MAX_SECS: u32 = 86_400;

    /// The TTL in seconds.
    pub fn as_secs(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for ZoneTtl {
    type Error = ZoneTtlError;

    fn try_from(secs: u32) -> Result<Self, Self::Error> {
        if (Self::MIN_SECS..=Self::MAX_SECS).contains(&secs) {
            Ok(Self(secs))
        } else {
            Err(ZoneTtlError(i64::from(secs)))
        }
    }
}

impl From<ZoneTtl> for u32 {
    fn from(ttl: ZoneTtl) -> Self {
        ttl.0
    }
}

impl fmt::Display for ZoneTtl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl sqlx::Type<sqlx::Postgres> for ZoneTtl {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i32 as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i32 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for ZoneTtl {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        // The range check keeps the value well inside i32, so this cast is total.
        <i32 as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&(self.0 as i32), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ZoneTtl {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let raw = <i32 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        u32::try_from(raw)
            .map_err(|_| ZoneTtlError(i64::from(raw)))
            .and_then(Self::try_from)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn zone_ttl_accepts_the_operational_range_only() {
        scenarios!(
            run = |secs: u32| ZoneTtl::try_from(secs).map_err(drop);
            "bounds inclusive" {
                30 => Yields(ZoneTtl(30)),
                86_400 => Yields(ZoneTtl(86_400)),
                300 => Yields(ZoneTtl(300)),
            }
            "outside" {
                0 => Fails,
                29 => Fails,
                86_401 => Fails,
            }
        );
    }

    #[test]
    fn zone_ttl_error_names_the_offending_value() {
        assert_eq!(
            ZoneTtl::try_from(5).expect_err("below floor").to_string(),
            "zone TTL 5 is outside 30..=86400 seconds"
        );
    }

    #[test]
    fn zone_ttl_serde_validates() {
        let ttl: ZoneTtl = serde_json::from_str("600").expect("in range");
        assert_eq!(ttl.as_secs(), 600);
        assert_eq!(serde_json::to_string(&ttl).expect("serialize"), "600");
        assert!(serde_json::from_str::<ZoneTtl>("5").is_err());
    }
}
