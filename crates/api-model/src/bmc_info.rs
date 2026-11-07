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
use std::fmt::{Display, Formatter};
use std::net::IpAddr;
use std::str::FromStr;

use carbide_uuid::machine::MachineInterfaceId;
use eyre::{Report, eyre};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};
use version_compare::Cmp;

use crate::errors::{ModelError, ModelResult};
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BmcInfo {
    pub machine_interface_id: Option<MachineInterfaceId>,
    pub ip: Option<IpAddr>,
    pub port: Option<u16>,
    pub mac: Option<MacAddress>,
    pub version: Option<String>,
    pub firmware_version: Option<String>,
}

impl BmcInfo {
    pub fn supports_bfb_install(&self) -> bool {
        self.firmware_version.as_ref().is_some_and(|v| {
            // `firmware_version` is normalized to a numeric version by
            // `dpu_bmc_version` (the generation prefix "bf-"/"bf4-" is stripped);
            // strip here too in case it arrives raw from another source. BFB
            // install requires firmware >= 24.10; BF4 firmware is year-based
            // (>= 26.x) so it always clears this gate.
            let version = v.to_lowercase().replace("bf4-", "").replace("bf-", "");
            version_compare::compare_to(version, "24.10", Cmp::Ge).is_ok_and(|r| r)
        })
    }
}

impl<'r> FromRow<'r, PgRow> for BmcInfo {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let bmc_info: String = row.try_get("bmc_info")?;
        serde_json::from_str(&bmc_info).map_err(|e| sqlx::Error::ColumnDecode {
            index: "bmc_info".to_owned(),
            source: e.into(),
        })
    }
}

impl BmcInfo {
    pub fn ip_addr(&self) -> Result<IpAddr, Report> {
        self.ip.ok_or(eyre! {"missing BMC address"})
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "user_roles")]
#[sqlx(rename_all = "lowercase")]
pub enum UserRoles {
    User,
    Administrator,
    Operator,
    Noaccess,
}

impl Display for UserRoles {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let string = match self {
            UserRoles::User => "user",
            UserRoles::Administrator => "administrator",
            UserRoles::Operator => "operator",
            UserRoles::Noaccess => "noaccess",
        };

        write!(f, "{string}")
    }
}

impl FromStr for UserRoles {
    type Err = ModelError;

    fn from_str(input: &str) -> ModelResult<Self> {
        match input {
            "user" => Ok(UserRoles::User),
            "administrator" => Ok(UserRoles::Administrator),
            "operator" => Ok(UserRoles::Operator),
            "noaccess" => Ok(UserRoles::Noaccess),
            x => Err(ModelError::DatabaseTypeConversionError(format!(
                "Unknown role found in database: {x}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    fn bmc_with_firmware(version: Option<&str>) -> BmcInfo {
        BmcInfo {
            firmware_version: version.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn supports_bfb_install_firmware_matrix() {
        check_values(
            [
                Check {
                    scenario: "normalized BF4 firmware clears the minimum",
                    input: Some("26.04-8"),
                    expect: true,
                },
                Check {
                    scenario: "raw BF4 firmware clears the minimum",
                    input: Some("BF4-26.04-8"),
                    expect: true,
                },
                Check {
                    scenario: "newer raw BF3 firmware clears the minimum",
                    input: Some("BF-25.10-9"),
                    expect: true,
                },
                Check {
                    scenario: "BF3 firmware at the minimum is supported",
                    input: Some("BF-24.10-0"),
                    expect: true,
                },
                Check {
                    scenario: "normalized BF3 firmware clears the minimum",
                    input: Some("25.10-9"),
                    expect: true,
                },
                Check {
                    scenario: "older BF3 firmware is unsupported",
                    input: Some("BF-24.04-1"),
                    expect: false,
                },
                Check {
                    scenario: "absent firmware is unsupported",
                    input: None,
                    expect: false,
                },
            ],
            |version| bmc_with_firmware(version).supports_bfb_install(),
        );
    }
}
