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

use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
#[cfg(feature = "sqlx")]
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    {Database, Postgres, Row},
};

/// A value that is accompanied by a version field
///
/// This small wrapper is intended to pass around any kind of value and the
/// associated version as a single parameter.
#[derive(Debug, Clone)]
pub struct Versioned<T> {
    /// The value that is versioned
    pub value: T,
    /// The value that is associated with this version
    pub version: ConfigVersion,
}

impl<T> Versioned<T> {
    /// Creates a new a `Versioned` wrapper around the value
    pub fn new(value: T, version: ConfigVersion) -> Self {
        Self { value, version }
    }

    /// Converts a `Versioned<T>` into a `Versioned<&T>`
    ///
    /// This is helpful to pass around versioned data cheaply (without having to
    /// deep copy it).
    pub fn as_ref(&self) -> Versioned<&T> {
        Versioned::new(&self.value, self.version)
    }

    // Split the value and version out, consuming the Versioned.
    // Necessary for DB update calls that check the version.
    pub fn take(self) -> (T, ConfigVersion) {
        (self.value, self.version)
    }
}

impl<T> Deref for Versioned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for Versioned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

/// The version of any configuration that is applied in the Forge system
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ConfigVersion {
    /// A monotonically incrementing number that alone uniquely identify the version
    /// of a configuration. Version number 0 is never used.
    version_nr: u64,
    /// A timestamp that is updated on each version number increment.
    /// This one mainly helps with observability: If we ever detect some
    /// subsytem dealing with an outdated configuration, we at least know when
    /// it last updated its configuration and how much it is behind.
    timestamp: DateTime<Utc>,
}

/// Represents an operation to change (typically increment) a ConfigVersion, for cases that resemble
/// "compare-and-swap", ie. change to the `new` only if the current matches `current`. Typically
/// constructed via [`ConfigVersion::incremental_change`]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ConfigVersionChange {
    pub current: ConfigVersion,
    pub new: ConfigVersion,
}

impl std::fmt::Display for ConfigVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Note that we could implement `to_version_string()` in terms of `Display`.
        // However this could mean that someone changing the Rust debug format
        // might accidentally touch the DB format. Therefore we keep a separate
        // method for this, and delegate the other way around.
        let version_str = self.version_string();
        f.write_str(&version_str)
    }
}

impl ConfigVersion {
    /// Returns the version number
    ///
    /// This is a monotonically incrementing number that is increased by each
    /// change to an entity
    pub fn version_nr(&self) -> u64 {
        self.version_nr
    }

    /// Returns the timestamp when the version was changed
    pub fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    /// Creates an initial version number for any entity
    pub fn initial() -> Self {
        Self {
            version_nr: 1,
            timestamp: now(),
        }
    }

    pub fn new(version_nr: u64) -> Self {
        Self {
            version_nr,
            timestamp: now(),
        }
    }

    // Creates an invalid version that should not match any valid version
    pub fn invalid() -> Self {
        Self {
            version_nr: 0,
            timestamp: tzero(),
        }
    }

    /// Increments the version
    ///
    /// This returns a new `ConfigVersion` instance, which carries the updated
    /// version number. We avoid version number 0 by returning 1 if incrementing
    /// results in version number being 0.
    pub fn increment(&self) -> Self {
        let mut nr = self.version_nr.wrapping_add(1);
        nr |= (nr == 0) as u64;
        Self {
            version_nr: nr,
            timestamp: now(),
        }
    }

    /// Returns a serialized format of `ConfigVersion`
    ///
    /// This is the database format we are using. Do not modify
    pub fn version_string(&self) -> String {
        // Note that we use microseconds here to get a reasonable precision
        // while being able to represent 584k years
        format!(
            "V{}-T{}",
            self.version_nr,
            self.timestamp.timestamp_micros()
        )
    }

    /// Amount of time since we entered this state version
    /// Returned value will be negative if state change is in the future.
    /// Use `to_std()` to get a Duration.
    pub fn since_state_change(&self) -> TimeDelta {
        Utc::now() - self.timestamp()
    }

    /// Human readable amount of time since we entered the given state version
    /// e.g. "2 hours and 14 minutes", or "12 seconds"
    pub fn since_state_change_humanized(&self) -> String {
        format_duration(self.since_state_change())
    }

    /// Returns the smaller version of 2 version fields, purely based on the
    /// timestamp encoded in the version
    pub fn min_by_timestamp(&self, other: &Self) -> Self {
        match self.timestamp.cmp(&other.timestamp) {
            std::cmp::Ordering::Less => *self,
            std::cmp::Ordering::Equal => match self.version_nr.cmp(&other.version_nr) {
                std::cmp::Ordering::Less => *self,
                std::cmp::Ordering::Equal => *self,
                std::cmp::Ordering::Greater => *other,
            },
            std::cmp::Ordering::Greater => *other,
        }
    }

    /// Construct a ConfigVersionChange that increments this ConfigVersion by 1
    pub fn incremental_change(&self) -> ConfigVersionChange {
        ConfigVersionChange {
            current: *self,
            new: ConfigVersion {
                version_nr: self.version_nr + 1,
                timestamp: Utc::now(),
            },
        }
    }
}

/// Returns the current timestamp rounded to the next microsecond
///
/// We use this method since we only serialize the timestamp using microsecond
/// precision, and we don't want to have the timestamp look different after a
/// serialization and deserialization cycle.
fn now() -> DateTime<Utc> {
    let mut now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch");
    let round = now.as_nanos() % 1000;
    now -= Duration::from_nanos(round as _);

    let naive = DateTime::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .expect("out-of-range number of seconds and/or invalid nanosecond");
    Utc.from_utc_datetime(&naive.naive_utc())
}

/// Returns the start of time per Unix convention
fn tzero() -> DateTime<Utc> {
    let mut now = SystemTime::UNIX_EPOCH
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch");
    let round = now.as_nanos() % 1000;
    now -= Duration::from_nanos(round as _);

    let naive = DateTime::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .expect("out-of-range number of seconds and/or invalid nanosecond");
    Utc.from_utc_datetime(&naive.naive_utc())
}

/// Error that is returned when parsing a version fails
#[derive(Debug, thiserror::Error)]
pub enum ConfigVersionParseError {
    #[error("invalid version format: {0}")]
    VersionFormat(String),
    #[error("invalid date time: {0}, {1}")]
    DateTime(i64, u32),
}

impl FromStr for ConfigVersion {
    type Err = ConfigVersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split('-');

        let version_nr_str = match parts.next() {
            Some(nr_str) => nr_str,
            None => return Err(ConfigVersionParseError::VersionFormat(s.to_string())),
        };

        let timestamp_str = match parts.next() {
            Some(timestamp_str) => timestamp_str,
            None => return Err(ConfigVersionParseError::VersionFormat(s.to_string())),
        };

        if parts.next().is_some() {
            return Err(ConfigVersionParseError::VersionFormat(s.to_string()));
        }

        if version_nr_str.is_empty()
            || version_nr_str.as_bytes()[0] != b'V'
            || timestamp_str.is_empty()
            || timestamp_str.as_bytes()[0] != b'T'
        {
            return Err(ConfigVersionParseError::VersionFormat(s.to_string()));
        }

        let version_nr: u64 = version_nr_str[1..]
            .parse()
            .map_err(|_| ConfigVersionParseError::VersionFormat(s.to_string()))?;
        let timestamp: u64 = timestamp_str[1..]
            .parse()
            .map_err(|_| ConfigVersionParseError::VersionFormat(s.to_string()))?;

        let secs = timestamp / 1_000_000;
        let usecs = timestamp % 1_000_000;

        let datetime = match DateTime::from_timestamp(secs as i64, (usecs * 1000) as u32) {
            Some(ndt) => ndt,
            None => {
                return Err(ConfigVersionParseError::DateTime(
                    secs as i64,
                    (usecs * 1000) as u32,
                ));
            }
        };

        let timestamp = Utc.from_utc_datetime(&datetime.naive_utc());

        Ok(Self {
            version_nr,
            timestamp,
        })
    }
}

impl serde::Serialize for ConfigVersion {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.version_string().serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for ConfigVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let str_value = String::deserialize(deserializer)?;
        let version =
            ConfigVersion::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(version)
    }
}

#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for ConfigVersion
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
impl sqlx::Encode<'_, sqlx::Postgres> for ConfigVersion {
    fn encode_by_ref(
        &self,
        buf: &mut <Postgres as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        buf.extend(self.to_string().as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}

#[cfg(feature = "sqlx")]
impl<'r, DB> sqlx::Decode<'r, DB> for ConfigVersion
where
    DB: sqlx::Database,
    String: sqlx::Decode<'r, DB>,
{
    fn decode(
        value: <DB as sqlx::database::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let source = String::decode(value)?;
        let config_version =
            Self::from_str(&source).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        Ok(config_version)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ConfigVersion {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let config_version = row.try_get(0)?;
        Ok(config_version)
    }
}

const STATE_VERSION_PARSE_ERROR: &str = "state version parse error";

/// Human readable amount of time since we entered the given state version
pub fn since_state_change_humanized(ver: &str) -> String {
    let Ok(state_version_t) = ver.parse::<ConfigVersion>() else {
        return STATE_VERSION_PARSE_ERROR.to_string();
    };
    state_version_t.since_state_change_humanized()
}

pub fn format_duration(d: TimeDelta) -> String {
    let seconds = d.num_seconds();
    const SECONDS_IN_MINUTE: i64 = 60;
    const SECONDS_IN_HOUR: i64 = SECONDS_IN_MINUTE * 60;
    const SECONDS_IN_DAY: i64 = 24 * SECONDS_IN_HOUR;

    let days = seconds / SECONDS_IN_DAY;
    let hours = (seconds % SECONDS_IN_DAY) / SECONDS_IN_HOUR;
    let minutes = (seconds % SECONDS_IN_HOUR) / SECONDS_IN_MINUTE;
    let seconds = seconds % SECONDS_IN_MINUTE;

    let mut parts = vec![];
    if days > 0 {
        parts.push(plural(days, "day"));
    }
    if hours > 0 {
        parts.push(plural(hours, "hour"));
    }
    if minutes > 0 {
        parts.push(plural(minutes, "minute"));
    }
    if parts.is_empty() {
        // Only include seconds if less than 1 minute
        parts.push(plural(seconds, "second"));
    }
    match parts.len() {
        0 => String::from("0 seconds"),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap();
            format!("{} and {}", parts.join(", "), last)
        }
    }
}

fn plural(val: i64, period: &str) -> String {
    if val == 1 {
        format!("{val} {period}")
    } else {
        format!("{val} {period}s")
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use chrono::TimeDelta;

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum ParseFailure {
        VersionFormat,
        DateTime,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct VersionSummary {
        version_nr: u64,
        timestamp_micros: i64,
        version_string: String,
        display: String,
    }

    #[derive(Clone, Copy)]
    enum VersionOperation {
        Initial,
        New,
        Invalid,
        Increment,
        OverflowIncrement,
        IncrementalChange,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OperationSummary {
        version_nr: u64,
        timestamp_micros: Option<i64>,
        current_version_nr: Option<u64>,
        new_version_nr: Option<u64>,
    }

    fn summarize(version: ConfigVersion) -> VersionSummary {
        VersionSummary {
            version_nr: version.version_nr(),
            timestamp_micros: version.timestamp().timestamp_micros(),
            version_string: version.version_string(),
            display: version.to_string(),
        }
    }

    fn parse_version(input: &str) -> Result<VersionSummary, ParseFailure> {
        ConfigVersion::from_str(input)
            .map(summarize)
            .map_err(|err| match err {
                ConfigVersionParseError::VersionFormat(_) => ParseFailure::VersionFormat,
                ConfigVersionParseError::DateTime(_, _) => ParseFailure::DateTime,
            })
    }

    fn deserialize_config_version(input: &str) -> Result<VersionSummary, ()> {
        serde_json::from_str::<ConfigVersion>(input)
            .map(summarize)
            .map_err(|_| ())
    }

    fn summarize_operation_version(version: ConfigVersion) -> OperationSummary {
        OperationSummary {
            version_nr: version.version_nr(),
            // These operations stamp wall-clock timestamps, so table rows compare
            // the deterministic version fields instead.
            timestamp_micros: None,
            current_version_nr: None,
            new_version_nr: None,
        }
    }

    fn apply_operation(operation: VersionOperation) -> OperationSummary {
        match operation {
            VersionOperation::Initial => summarize_operation_version(ConfigVersion::initial()),
            VersionOperation::New => summarize_operation_version(ConfigVersion::new(7)),
            VersionOperation::Invalid => {
                let version = ConfigVersion::invalid();
                OperationSummary {
                    version_nr: version.version_nr(),
                    timestamp_micros: Some(version.timestamp().timestamp_micros()),
                    current_version_nr: None,
                    new_version_nr: None,
                }
            }
            VersionOperation::Increment => summarize_operation_version(
                ConfigVersion {
                    version_nr: 41,
                    timestamp: tzero(),
                }
                .increment(),
            ),
            VersionOperation::OverflowIncrement => summarize_operation_version(
                ConfigVersion {
                    version_nr: u64::MAX,
                    timestamp: tzero(),
                }
                .increment(),
            ),
            VersionOperation::IncrementalChange => {
                let version = ConfigVersion {
                    version_nr: 41,
                    timestamp: tzero(),
                };
                let change = version.incremental_change();
                OperationSummary {
                    version_nr: version.version_nr(),
                    timestamp_micros: None,
                    current_version_nr: Some(change.current.version_nr()),
                    new_version_nr: Some(change.new.version_nr()),
                }
            }
        }
    }

    #[test]
    fn serialize_and_deserialize_config_version_as_string() {
        let config_version = ConfigVersion::initial();
        let vs = config_version.version_string();
        let parsed: ConfigVersion = vs.parse().unwrap();
        assert_eq!(parsed, config_version);

        let next = config_version.increment();
        assert_eq!(next.version_nr, 2);
        let vs = next.version_string();
        let parsed_next: ConfigVersion = vs.parse().unwrap();
        assert_eq!(parsed_next, next);
    }

    #[test]
    fn serialize_and_deserialize_config_version_with_serde() {
        let config_version = ConfigVersion::initial();
        let vs = serde_json::to_string(&config_version).unwrap();
        let parsed: ConfigVersion = serde_json::from_str(&vs).unwrap();
        assert_eq!(parsed, config_version);

        let next = config_version.increment();
        assert_eq!(next.version_nr, 2);
        let vs = serde_json::to_string(&next).unwrap();
        let parsed_next: ConfigVersion = serde_json::from_str(&vs).unwrap();
        assert_eq!(parsed_next, next);
    }

    #[test]
    fn it_formats_durations() {
        value_scenarios!(
            run = |seconds| format_duration(TimeDelta::seconds(seconds));
            "seconds" {
                0 => "0 seconds".to_string(),
                1 => "1 second".to_string(),
                44 => "44 seconds".to_string(),
                -44 => "-44 seconds".to_string(),
            }

            "minutes" {
                60 => "1 minute".to_string(),
                61 => "1 minute".to_string(),
            }

            "larger units" {
                3600 * 2 => "2 hours".to_string(),
                86400 => "1 day".to_string(),
                3600 + 60 => "1 hour and 1 minute".to_string(),
                86400 + 3600 + 60 + 1 => "1 day, 1 hour and 1 minute".to_string(),
            }
        );
    }

    #[test]
    fn parse_config_version_cases() {
        scenarios!(parse_version:
            "valid versions" {
                "V5-T0" => Yields(VersionSummary {
                    version_nr: 5,
                    timestamp_micros: 0,
                    version_string: "V5-T0".to_string(),
                    display: "V5-T0".to_string(),
                }),
                " V7-T1000000 " => Yields(VersionSummary {
                    version_nr: 7,
                    timestamp_micros: 1_000_000,
                    version_string: "V7-T1000000".to_string(),
                    display: "V7-T1000000".to_string(),
                }),
            }

            "format errors" {
                "" => FailsWith(ParseFailure::VersionFormat),
                "V1" => FailsWith(ParseFailure::VersionFormat),
                "1-T0" => FailsWith(ParseFailure::VersionFormat),
                "Vx-T0" => FailsWith(ParseFailure::VersionFormat),
                "V1-Tx" => FailsWith(ParseFailure::VersionFormat),
                "V1-T0-extra" => FailsWith(ParseFailure::VersionFormat),
            }

            "datetime errors" {
                "V1-T18446744073709551615" => FailsWith(ParseFailure::DateTime),
            }
        );
    }

    #[test]
    fn serde_config_version_cases() {
        scenarios!(deserialize_config_version:
            "valid version strings" {
                "\"V5-T0\"" => Yields(VersionSummary {
                    version_nr: 5,
                    timestamp_micros: 0,
                    version_string: "V5-T0".to_string(),
                    display: "V5-T0".to_string(),
                }),
            }

            "invalid json values" {
                "\"bad\"" => Fails,
                "42" => Fails,
            }
        );
    }

    #[test]
    fn config_version_operation_cases() {
        value_scenarios!(apply_operation:
            "constructors" {
                VersionOperation::Initial => OperationSummary {
                    version_nr: 1,
                    timestamp_micros: None,
                    current_version_nr: None,
                    new_version_nr: None,
                },
                VersionOperation::New => OperationSummary {
                    version_nr: 7,
                    timestamp_micros: None,
                    current_version_nr: None,
                    new_version_nr: None,
                },
                VersionOperation::Invalid => OperationSummary {
                    version_nr: 0,
                    timestamp_micros: Some(0),
                    current_version_nr: None,
                    new_version_nr: None,
                },
            }

            "increments" {
                VersionOperation::Increment => OperationSummary {
                    version_nr: 42,
                    timestamp_micros: None,
                    current_version_nr: None,
                    new_version_nr: None,
                },
                VersionOperation::OverflowIncrement => OperationSummary {
                    version_nr: 1,
                    timestamp_micros: None,
                    current_version_nr: None,
                    new_version_nr: None,
                },
                VersionOperation::IncrementalChange => OperationSummary {
                    version_nr: 41,
                    timestamp_micros: None,
                    current_version_nr: Some(41),
                    new_version_nr: Some(42),
                },
            }
        );
    }

    #[test]
    fn versioned_wrapper_methods() {
        let version = ConfigVersion {
            version_nr: 3,
            timestamp: tzero(),
        };
        let mut versioned = Versioned::new("payload".to_string(), version);

        assert_eq!(&*versioned, "payload");
        versioned.push_str("-updated");

        let borrowed = versioned.as_ref();
        assert_eq!(borrowed.value.as_str(), "payload-updated");
        assert_eq!(borrowed.version, version);

        let (value, taken_version) = versioned.take();
        assert_eq!(value, "payload-updated");
        assert_eq!(taken_version, version);
    }

    #[test]
    fn since_state_change_humanized_parse_cases() {
        value_scenarios!(
            run = |input| since_state_change_humanized(input) == STATE_VERSION_PARSE_ERROR;
            "parse status" {
                "bad" => true,
                "V1-T0" => false,
            }
        );
    }

    #[test]
    fn test_min_by_timestamp() {
        // Same timestamp, same version
        let v1 = ConfigVersion::initial();
        assert_eq!(v1.min_by_timestamp(&v1), v1);

        // Difference is only in timestamp
        let v2 = ConfigVersion {
            version_nr: v1.version_nr,
            timestamp: v1.timestamp + chrono::Duration::milliseconds(1),
        };
        assert_eq!(v1.min_by_timestamp(&v2), v1);
        assert_eq!(v2.min_by_timestamp(&v1), v1);

        // Same timestamp, different version
        let v3 = ConfigVersion {
            version_nr: v1.version_nr + 1,
            timestamp: v1.timestamp,
        };
        assert_eq!(v1.min_by_timestamp(&v3), v1);
        assert_eq!(v3.min_by_timestamp(&v1), v1);
    }
}
