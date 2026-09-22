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

use std::collections::HashMap;
use std::str::FromStr;

use carbide_uuid::infiniband::IBPartitionId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::StateSla;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::ib::{IBMtu, IBNetwork, IBQosConf, IBRateLimit, IBServiceLevel};
use crate::metadata::Metadata;
use crate::tenant::TenantOrganizationId;

mod slas;

#[derive(Clone, Debug, Default)]
pub struct IbPartitionSearchFilter {
    pub tenant_org_id: Option<String>,
    pub name: Option<String>,
}

/// Represents an InfiniBand Partition Key
/// Partition Keys are 16 bit values valid up to a value of 0x7fff
/// Partition Keys are serialized as strings, since the hex represenation is
/// their canonical representation.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartitionKey(u16);

impl PartitionKey {
    /// Returns the partition key associated with the default partition
    pub const fn for_default_partition() -> Self {
        Self(0x7fff)
    }

    /// Returns whether the partition key describes the default partition
    pub fn is_default_partition(self) -> bool {
        self == Self::for_default_partition()
    }
}

#[derive(thiserror::Error, Debug, Clone)]
#[error("partition key \"{0}\" is not valid")]
pub struct InvalidPartitionKeyError(String);

impl serde::Serialize for PartitionKey {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_string().serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for PartitionKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let str_value = String::deserialize(deserializer)?;
        let version =
            PartitionKey::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))?;
        Ok(version)
    }
}

impl TryFrom<u16> for PartitionKey {
    type Error = InvalidPartitionKeyError;

    fn try_from(pkey: u16) -> Result<Self, Self::Error> {
        if pkey != (pkey & 0x7fff) {
            return Err(InvalidPartitionKeyError(pkey.to_string()));
        }

        Ok(PartitionKey(pkey))
    }
}

impl FromStr for PartitionKey {
    type Err = InvalidPartitionKeyError;

    fn from_str(pkey: &str) -> Result<Self, Self::Err> {
        let pkey = pkey.to_lowercase();
        let base = if pkey.starts_with("0x") { 16 } else { 10 };
        let p = pkey.trim_start_matches("0x");
        // Apply the same 0x7fff range check as `TryFrom<u16>` so every
        // construction path agrees on what a valid pkey is.
        match u16::from_str_radix(p, base) {
            Ok(v) => PartitionKey::try_from(v),
            Err(_e) => Err(InvalidPartitionKeyError(pkey.to_string())),
        }
    }
}

impl TryFrom<String> for PartitionKey {
    type Error = InvalidPartitionKeyError;

    fn try_from(pkey: String) -> Result<Self, Self::Error> {
        PartitionKey::from_str(&pkey)
    }
}

impl TryFrom<&String> for PartitionKey {
    type Error = InvalidPartitionKeyError;

    fn try_from(pkey: &String) -> Result<Self, Self::Error> {
        PartitionKey::try_from(pkey.to_string())
    }
}

impl TryFrom<&str> for PartitionKey {
    type Error = InvalidPartitionKeyError;

    fn try_from(pkey: &str) -> Result<Self, Self::Error> {
        PartitionKey::try_from(pkey.to_string())
    }
}

impl std::fmt::Display for PartitionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "0x{:x}", self.0)
    }
}

impl From<PartitionKey> for u16 {
    fn from(v: PartitionKey) -> u16 {
        v.0
    }
}

#[derive(Debug, Clone)]
pub struct NewIBPartition {
    pub id: IBPartitionId,
    pub config: IBPartitionConfig,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IBPartitionConfig {
    pub name: String,
    pub pkey: Option<PartitionKey>,
    pub tenant_organization_id: TenantOrganizationId,
    pub mtu: Option<IBMtu>,
    pub rate_limit: Option<IBRateLimit>,
    pub service_level: Option<IBServiceLevel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IBPartitionStatus {
    pub partition: Option<String>,
    pub mtu: Option<IBMtu>,
    pub rate_limit: Option<IBRateLimit>,
    pub service_level: Option<IBServiceLevel>,
    pub pkey: Option<PartitionKey>,
}

#[derive(Debug, Clone)]
pub struct IBPartition {
    pub id: IBPartitionId,
    pub version: ConfigVersion,

    pub config: IBPartitionConfig,
    pub status: Option<IBPartitionStatus>,

    pub deleted: Option<DateTime<Utc>>,

    pub controller_state: Versioned<IBPartitionControllerState>,

    /// The result of the last attempt to change state
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    // Columns for these exist, but are unused in rust code
    // pub created: DateTime<Utc>,
    // pub updated: DateTime<Utc>,
    pub metadata: Metadata,
}

impl IBPartition {
    /// Returns whether the IB partition was deleted by the user
    pub fn is_marked_as_deleted(&self) -> bool {
        self.deleted.is_some()
    }
}

impl From<&IBPartition> for IBNetwork {
    fn from(ib: &IBPartition) -> IBNetwork {
        Self {
            name: ib.metadata.name.clone(),
            pkey: ib
                .status
                .as_ref()
                .and_then(|s| s.pkey)
                .map(u16::from)
                .unwrap_or(0u16),
            ipoib: true,
            associated_guids: None,
            membership: None,
            qos_conf: Some(IBQosConf {
                mtu: ib.config.mtu.clone().unwrap_or_default(),
                rate_limit: ib.config.rate_limit.clone().unwrap_or_default(),
                service_level: ib.config.service_level.clone().unwrap_or_default(),
            }),
        }
    }
}

impl<'r> FromRow<'r, PgRow> for IBPartition {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let controller_state: sqlx::types::Json<IBPartitionControllerState> =
            row.try_get("controller_state")?;
        let state_outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;

        let status: Option<sqlx::types::Json<IBPartitionStatus>> = row.try_get("status")?;
        let status = status.map(|s| s.0);

        let tenant_organization_id_str: &str = row.try_get("organization_id")?;
        let tenant_organization_id =
            TenantOrganizationId::try_from(tenant_organization_id_str.to_string())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        let pkey: Option<i32> = row.try_get("pkey")?;
        let mtu: i32 = row.try_get("mtu")?;
        let rate_limit: i32 = row.try_get("rate_limit")?;
        let service_level: i32 = row.try_get("service_level")?;
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;
        let description: String = row.try_get("description")?;
        let name: String = row.try_get("name")?;

        Ok(IBPartition {
            id: row.try_get("id")?,
            version: row.try_get("config_version")?,
            config: IBPartitionConfig {
                name: name.clone(), // Derprecated field
                pkey: pkey
                    .map(|p| PartitionKey::try_from(p as u16))
                    .transpose()
                    .map_err(|_| {
                        let err = eyre::eyre!("pkey {} is not valid", pkey.unwrap_or_default());
                        sqlx::Error::Decode(err.into())
                    })?,
                tenant_organization_id,
                mtu: IBMtu::try_from(mtu).ok(),
                rate_limit: IBRateLimit::try_from(rate_limit).ok(),
                service_level: IBServiceLevel::try_from(service_level).ok(),
            },
            status,
            metadata: Metadata {
                name,
                labels: labels.0,
                description,
            },
            deleted: row.try_get("deleted")?,

            controller_state: Versioned::new(
                controller_state.0,
                row.try_get("controller_state_version")?,
            ),
            controller_state_outcome: state_outcome.map(|x| x.0),
        })
    }
}

/// State of a IB subnet as tracked by the controller
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum IBPartitionControllerState {
    /// The IB subnet is created in Carbide, waiting for provisioning in IB Fabric.
    Provisioning,
    /// The IB subnet is ready for IB ports.
    Ready,
    /// There is error in IB subnet; IB ports can not be added into IB subnet if it's error.
    Error { cause: String },
    /// The IB subnet is in the process of deleting.
    Deleting,
}

/// Returns the SLA for the current state
pub fn state_sla(state: &IBPartitionControllerState, state_version: &ConfigVersion) -> StateSla {
    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));

    match state {
        IBPartitionControllerState::Provisioning => {
            StateSla::with_sla(slas::PROVISIONING, time_in_state)
        }
        IBPartitionControllerState::Ready => StateSla::no_sla(),
        IBPartitionControllerState::Error { .. } => StateSla::no_sla(),
        IBPartitionControllerState::Deleting => StateSla::with_sla(slas::DELETING, time_in_state),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    // ---- PartitionKey::from_str --------------------------------------------

    #[test]
    fn from_str_parses_each_input() {
        check_cases(
            [
                Case {
                    scenario: "hex with 0x prefix",
                    input: "0xf",
                    expect: Yields(0x000f),
                },
                Case {
                    scenario: "decimal of the same value",
                    input: "15",
                    expect: Yields(0x000f),
                },
                Case {
                    scenario: "zero hex",
                    input: "0x0",
                    expect: Yields(0),
                },
                Case {
                    scenario: "zero decimal",
                    input: "0",
                    expect: Yields(0),
                },
                Case {
                    scenario: "max default-partition hex",
                    input: "0x7fff",
                    expect: Yields(0x7fff),
                },
                Case {
                    scenario: "uppercase hex digits",
                    input: "0x7ABC",
                    expect: Yields(0x7abc),
                },
                Case {
                    scenario: "uppercase 0X prefix is lowercased first",
                    input: "0XF",
                    expect: Yields(0x000f),
                },
                Case {
                    // from_str enforces the same 0x7fff mask as TryFrom<u16>.
                    scenario: "hex above the valid pkey mask is rejected",
                    input: "0xffff",
                    expect: Fails,
                },
                Case {
                    scenario: "decimal above the mask is rejected",
                    input: "65535",
                    expect: Fails,
                },
                Case {
                    scenario: "empty string",
                    input: "",
                    expect: Fails,
                },
                Case {
                    scenario: "non-numeric text",
                    input: "nope",
                    expect: Fails,
                },
                Case {
                    scenario: "decimal overflowing u16",
                    input: "65536",
                    expect: Fails,
                },
                Case {
                    scenario: "hex overflowing u16",
                    input: "0x10000",
                    expect: Fails,
                },
                Case {
                    scenario: "bare 0x prefix with no digits",
                    input: "0x",
                    expect: Fails,
                },
                Case {
                    scenario: "decimal value carrying hex letters",
                    input: "1f",
                    expect: Fails,
                },
                Case {
                    scenario: "negative sign rejected",
                    input: "-1",
                    expect: Fails,
                },
            ],
            |s| PartitionKey::from_str(s).map(u16::from).map_err(drop),
        );
    }

    // ---- PartitionKey: TryFrom<&str> / String / &String --------------------

    #[test]
    fn try_from_str_like_inputs_match_from_str() {
        scenarios!(
            run = |s| PartitionKey::try_from(s).map(u16::from).map_err(drop);
            "&str hex" {
                "0x20" => Yields(0x20),
            }

            "&str decimal" {
                "32" => Yields(0x20),
            }

            "&str malformed" {
                "zz" => Fails,
            }
        );
    }

    #[test]
    fn try_from_owned_and_borrowed_string() {
        // TryFrom<String>
        scenarios!(
            run = |s| PartitionKey::try_from(s).map(u16::from).map_err(drop);
            "owned String hex" {
                "0xab".to_string() => Yields(0xab),
            }

            "owned String malformed" {
                "bad".to_string() => Fails,
            }
        );
        // TryFrom<&String>
        scenarios!(
            run = |s| PartitionKey::try_from(&s).map(u16::from).map_err(drop);
            "&String decimal" {
                "171".to_string() => Yields(0xab),
            }

            "&String malformed" {
                "bad".to_string() => Fails,
            }
        );
    }

    // ---- PartitionKey: TryFrom<u16> (the 0x7fff mask) ----------------------

    #[test]
    fn try_from_u16_enforces_the_mask() {
        scenarios!(
            run = |n| PartitionKey::try_from(n).map(u16::from).map_err(drop);
            "zero" {
                0u16 => Yields(0),
            }

            "small in-range value" {
                0x000f => Yields(0x000f),
            }

            "max valid (default partition)" {
                0x7fff => Yields(0x7fff),
            }

            "first value past the mask" {
                0x8000 => Fails,
            }

            "u16 max has the high bit set" {
                0xffff => Fails,
            }
        );
    }

    // ---- PartitionKey: Display ---------------------------------------------

    #[test]
    fn display_renders_lowercase_hex_with_prefix() {
        value_scenarios!(
            run = |n| PartitionKey::try_from(n).unwrap().to_string();
            "zero" {
                0u16 => "0x0".to_string(),
            }

            "single hex digit" {
                0x000f => "0xf".to_string(),
            }

            "multi-digit lowercased" {
                0x00ab => "0xab".to_string(),
            }

            "default partition key" {
                0x7fff => "0x7fff".to_string(),
            }
        );
    }

    // ---- PartitionKey: default-partition helpers ---------------------------

    #[test]
    fn for_default_partition_is_0x7fff() {
        assert_eq!(u16::from(PartitionKey::for_default_partition()), 0x7fff);
    }

    #[test]
    fn is_default_partition_predicate() {
        value_scenarios!(
            run = |n| PartitionKey::try_from(n).unwrap().is_default_partition();
            "the default key" {
                0x7fff => true,
            }

            "zero is not default" {
                0 => false,
            }

            "an ordinary key is not default" {
                0x000f => false,
            }

            "one below default" {
                0x7ffe => false,
            }
        );
    }

    // ---- PartitionKey: round-trip parse -> render --------------------------

    #[test]
    fn parse_then_display_round_trips_to_canonical_hex() {
        scenarios!(
            run = |s| {
                PartitionKey::from_str(s)
                    .map(|p| p.to_string())
                    .map_err(drop)
            };
            "decimal normalizes to hex" {
                "15" => Yields("0xf".to_string()),
            }

            "hex stays hex" {
                "0xf" => Yields("0xf".to_string()),
            }

            "uppercase folds to lowercase" {
                "0x7ABC" => Yields("0x7abc".to_string()),
            }
        );
    }

    // ---- PartitionKey: serde (string is the canonical form) ----------------

    #[test]
    fn serializes_as_canonical_hex_string() {
        scenarios!(
            run = |n| {
                let pkey = PartitionKey::try_from(n).unwrap();
                serde_json::to_string(&pkey).map_err(drop)
            };
            "single digit" {
                0x000f => Yields("\"0xf\"".to_string()),
            }

            "zero" {
                0 => Yields("\"0x0\"".to_string()),
            }

            "default partition" {
                0x7fff => Yields("\"0x7fff\"".to_string()),
            }
        );
    }

    #[test]
    fn deserializes_from_hex_or_decimal_strings() {
        scenarios!(
            run = |s| {
                serde_json::from_str::<PartitionKey>(s)
                    .map(u16::from)
                    .map_err(drop)
            };
            "hex string" {
                "\"0xf\"" => Yields(0x000f),
            }

            "decimal string of the same value" {
                "\"15\"" => Yields(0x000f),
            }

            "malformed string" {
                "\"nope\"" => Fails,
            }

            "non-string JSON is rejected" {
                "15" => Fails,
            }
        );
    }

    // ---- IBPartitionControllerState: serde (tagged, lowercase) -------------

    #[test]
    fn controller_state_serializes_with_lowercase_tag() {
        scenarios!(
            run = |state| serde_json::to_string(&state).map_err(drop);
            "provisioning" {
                IBPartitionControllerState::Provisioning => Yields(r#"{"state":"provisioning"}"#.to_string()),
            }

            "ready" {
                IBPartitionControllerState::Ready => Yields(r#"{"state":"ready"}"#.to_string()),
            }

            "deleting" {
                IBPartitionControllerState::Deleting => Yields(r#"{"state":"deleting"}"#.to_string()),
            }

            "error carries its cause" {
                IBPartitionControllerState::Error {
                    cause: "cause goes here".to_string(),
                } => Yields(r#"{"state":"error","cause":"cause goes here"}"#.to_string()),
            }
        );
    }

    #[test]
    fn controller_state_round_trips_through_json() {
        scenarios!(
            run = |state| {
                let json = serde_json::to_string(&state).map_err(drop)?;
                serde_json::from_str::<IBPartitionControllerState>(&json).map_err(drop)
            };
            "provisioning" {
                IBPartitionControllerState::Provisioning => Yields(IBPartitionControllerState::Provisioning),
            }

            "ready" {
                IBPartitionControllerState::Ready => Yields(IBPartitionControllerState::Ready),
            }

            "deleting" {
                IBPartitionControllerState::Deleting => Yields(IBPartitionControllerState::Deleting),
            }

            "error preserves its cause" {
                IBPartitionControllerState::Error {
                    cause: "boom".to_string(),
                } => Yields(IBPartitionControllerState::Error {
                    cause: "boom".to_string(),
                }),
            }
        );
    }

    #[test]
    fn controller_state_deserialize_rejects_unknown_tag() {
        Case {
            scenario: "unknown state tag",
            input: r#"{"state":"bogus"}"#,
            expect: Fails,
        }
        .check(|s| serde_json::from_str::<IBPartitionControllerState>(s).map_err(drop));
    }
}
