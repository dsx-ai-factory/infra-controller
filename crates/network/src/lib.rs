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

use std::str::FromStr;

use mac_address::{MacAddress, MacParseError};
use serde::Deserialize;
use serde::de::Deserializer;

pub mod base_mac;
pub mod ip;

/// virtualization is a module specific to shared code around
/// network virtualization, where shared means shared between
/// different components, where components currently means
/// Carbide API and the [DPU] agent.
pub mod virtualization;

#[doc(inline)]
pub use base_mac::BaseMac;
// Lets the database round-trip tests use `#[crate::sqlx_test]` to get a per-test
// Postgres pool from the shared harness (DATABASE_URL via .envrc).
#[cfg(all(test, feature = "sqlx"))]
pub(crate) use carbide_macros::sqlx_test;

const STRIPPED_MAC_LENGTH: usize = 12;

/// MELLANOX_SF_VF_MAC_ADDRESS_IN exists to really make it obvious
/// that the MAC address reported to topology data for SFs and VFs
/// comes in as ch:64.
pub const MELLANOX_SF_VF_MAC_ADDRESS_IN: &str = "ch:64";

/// MELLANOX_SF_VF_MAC_ADDRESS_OUT exists to really make it obvious
/// that we take MELLANOX_SF_VF_MAC_ADDRESS_IN and rewrite it out
/// as this.
pub const MELLANOX_SF_VF_MAC_ADDRESS_OUT: &str = "00:00:00:00:00:64";

/// sanitized_mac takes a potentially nasty input MAC address
/// string (e.g. `"a088c2    460c68"`, cleans up anything that
/// isn't base-16, adds colons, and returns you a nice MAC address
/// in the format of a mac_address::MacAddress.
///
///
/// For example:
///   `"a088c2    460c68"` -> `a088c2460c68` -> `A0:88:C2:46:0C:68`
///   `aa:bb:cc:DD:ee:ff`  -> `aabbccDDeeff` -> `AA:BB:CC:DD:EE:FF`
pub fn sanitized_mac(input_mac: &str) -> eyre::Result<MacAddress> {
    // First, strip out anything that isn't hex ([0-9A-Fa-f]),
    // which can be done with is_ascii_hexdigit().
    //
    // This will also strip out [g-zG-Z], so if we wanted to
    // error on that, and not silently drop them, this would
    // need to be changed. However, cases like that should
    // result in a bad STRIPPED_MAC_LENGTH anyway.
    let stripped_mac: String = input_mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();

    if stripped_mac.len() != STRIPPED_MAC_LENGTH {
        return Err(eyre::eyre!(
            "invalid stripped MAC length: {} (input: {}, output: {})",
            stripped_mac.len(),
            input_mac,
            stripped_mac,
        ));
    }

    // And then shove some colons back in, and we're done!
    let sanitized_mac =
        stripped_mac
            .chars()
            .enumerate()
            .fold(String::new(), |mut sanitized, (index, char)| {
                if index > 0 && index % 2 == 0 {
                    sanitized.push(':');
                }
                sanitized.push(char);
                sanitized
            });

    MacAddress::from_str(&sanitized_mac).map_err(|e| eyre::eyre!("failed to initialize MacAddress from sanitized MAC: {} (input: {}, stripped: {}, sanitized: {}", e, input_mac, stripped_mac, sanitized_mac))
}

/// deserialize_mlx_mac exists due to an interesting behavior
/// of Mellanox cards -- SFs and VFs (i.e. interfaces that aren't
/// the physical interface) report a MAC address of "ch:64",
/// which isn't a real MAC address. Unfortnuately, this breaks
/// MAC address validation + serialization for everyone else for
/// this field.
///
/// In other cases, it will report an empty string, so that's
/// another case we need to deal with.
///
/// So, instead of doing away with validation entirely, this
/// custom deserialization function exists to rewrite ch:64 as
/// 00:::::64 -- this is used for both ingestion (as in, when
/// topology data is sent to us as JSON), and for reading legacy
/// data from the database; at this point, serialization out to
/// the database will ALWAYS be a valid MAC, since the field is
/// a MacAddress now, so we just care about deserialization.
///
/// Fwiw, we obviously don't use ch:64 as an actual MAC
/// address, but still want us some insight in topology
/// data that its a special case, while still meeting the
/// requirements of being a valid MAC address.
pub fn deserialize_mlx_mac<'a, D>(deserializer: D) -> Result<MacAddress, D::Error>
where
    D: Deserializer<'a>,
{
    let input_value = String::deserialize(deserializer)?;
    let mac_address = deserialize_input_mac_to_address(&input_value).map_err(|e| {
        serde::de::Error::custom(format!(
            "failed to parse input mac_address({input_value}): {e}"
        ))
    })?;

    Ok(mac_address)
}

pub fn deserialize_optional_mlx_mac<'a, D>(deserializer: D) -> Result<Option<MacAddress>, D::Error>
where
    D: Deserializer<'a>,
{
    let optional_value: Option<String> = Option::deserialize(deserializer)?;

    let mac_address: Option<MacAddress> = match optional_value {
        Some(input_value) => {
            let mac_address = deserialize_input_mac_to_address(&input_value).map_err(|e| {
                serde::de::Error::custom(format!(
                    "failed to parse input mac_address({input_value}): {e}"
                ))
            })?;
            Some(mac_address)
        }
        None => None,
    };

    Ok(mac_address)
}

/// deserialize_input_mac_to_address is a common input to MAC conversion
/// function used by deserialize_mlx_mac and deserialize_optional_mlx_mac.
pub fn deserialize_input_mac_to_address(input_value: &str) -> Result<MacAddress, MacParseError> {
    let mac_string = if input_value == MELLANOX_SF_VF_MAC_ADDRESS_IN {
        MELLANOX_SF_VF_MAC_ADDRESS_OUT
    } else if input_value.is_empty() {
        "00:00:00:00:00:00"
    } else {
        input_value
    };

    let mac_address: MacAddress = mac_string.parse()?;
    Ok(mac_address)
}

/// Whether `mac` has the locally-administered bit set (bit 0x02 of the first
/// octet). NVIDIA BMC/NIC MACs are globally unique, so a locally-administered
/// value indicates transient pre-sync data rather than the burned-in address.
pub fn is_locally_administered_mac(mac: MacAddress) -> bool {
    mac.bytes()[0] & 0x02 != 0
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    // `sanitized_mac` returns `eyre::Result`, whose error does not implement
    // `PartialEq`, so error rows use `Fails` and the run closure maps the
    // success arm to the canonical `to_string()` form (and drops the error).
    #[test]
    fn sanitized_mac_normalizes_or_rejects() {
        check_cases(
            [
                // The motivating example: a quoted Redfish MAC riddled with
                // interior whitespace, normalized to a colon-grouped MAC.
                Case {
                    scenario: "gross redfish mac with quotes and spaces",
                    input: "\"a088c2    460c68\"",
                    expect: Yields("A0:88:C2:46:0C:68".to_string()),
                },
                Case {
                    scenario: "smashed hex, no separators",
                    input: "000000ABC789",
                    expect: Yields("00:00:00:AB:C7:89".to_string()),
                },
                Case {
                    scenario: "already clean, colon-separated",
                    input: "DE:ED:0F:BE:EF:99",
                    expect: Yields("DE:ED:0F:BE:EF:99".to_string()),
                },
                Case {
                    scenario: "mixed case, no separators (uppercased out)",
                    input: "AabBCcdDEefF",
                    expect: Yields("AA:BB:CC:DD:EE:FF".to_string()),
                },
                Case {
                    scenario: "all zeros",
                    input: "000000000000",
                    expect: Yields("00:00:00:00:00:00".to_string()),
                },
                Case {
                    scenario: "all f's, lowercase in",
                    input: "ffffffffffff",
                    expect: Yields("FF:FF:FF:FF:FF:FF".to_string()),
                },
                Case {
                    scenario: "dash-separated separators stripped",
                    input: "a0-88-c2-46-0c-68",
                    expect: Yields("A0:88:C2:46:0C:68".to_string()),
                },
                Case {
                    scenario: "dot-separated (cisco style) separators stripped",
                    input: "a088.c246.0c68",
                    expect: Yields("A0:88:C2:46:0C:68".to_string()),
                },
                Case {
                    scenario: "leading and trailing whitespace",
                    input: "  a088c2460c68  ",
                    expect: Yields("A0:88:C2:46:0C:68".to_string()),
                },
                Case {
                    scenario: "embedded non-hex letters dropped but length stays valid",
                    input: "a0g88hc2460c68",
                    expect: Yields("A0:88:C2:46:0C:68".to_string()),
                },
                // Error arms: any stripped length other than 12 hex digits.
                Case {
                    scenario: "empty input — zero hex digits",
                    input: "",
                    expect: Fails,
                },
                Case {
                    scenario: "no hex digits at all",
                    input: "ghijklmnopqr",
                    expect: Fails,
                },
                Case {
                    scenario: "too short — eleven hex digits",
                    input: "a088c2460c6",
                    expect: Fails,
                },
                Case {
                    scenario: "too short by one — ten hex digits",
                    input: "a088c2460c",
                    expect: Fails,
                },
                Case {
                    scenario: "too long — thirteen hex digits",
                    input: "a088c2460c688",
                    expect: Fails,
                },
                Case {
                    scenario: "way too long with junk",
                    input: "aabbccddeeffgg00112233445566778899",
                    expect: Fails,
                },
                Case {
                    scenario: "only whitespace",
                    input: "        ",
                    expect: Fails,
                },
            ],
            |input| {
                sanitized_mac(input)
                    .map(|mac| mac.to_string())
                    .map_err(drop)
            },
        );
    }

    // `deserialize_input_mac_to_address` returns `Result<_, MacParseError>`;
    // mapping the success arm to `to_string()` and dropping the error keeps the
    // error type as `()`, so reject rows use `Fails`.
    #[test]
    fn deserialize_input_mac_rewrites_or_rejects() {
        check_cases(
            [
                Case {
                    scenario: "ordinary colon-separated mac passes through",
                    input: "00:11:22:33:44:55",
                    expect: Yields("00:11:22:33:44:55".to_string()),
                },
                Case {
                    scenario: "mellanox ch:64 sentinel rewritten to the OUT form",
                    input: MELLANOX_SF_VF_MAC_ADDRESS_IN,
                    expect: Yields(MELLANOX_SF_VF_MAC_ADDRESS_OUT.to_string()),
                },
                Case {
                    scenario: "empty string rewritten to the all-zero mac",
                    input: "",
                    expect: Yields("00:00:00:00:00:00".to_string()),
                },
                Case {
                    scenario: "uppercase hex normalized on parse",
                    input: "AA:BB:CC:DD:EE:FF",
                    expect: Yields("AA:BB:CC:DD:EE:FF".to_string()),
                },
                Case {
                    scenario: "lowercase hex uppercased on display",
                    input: "aa:bb:cc:dd:ee:ff",
                    expect: Yields("AA:BB:CC:DD:EE:FF".to_string()),
                },
                Case {
                    scenario: "dash-separated mac parses",
                    input: "00-11-22-33-44-55",
                    expect: Yields("00:11:22:33:44:55".to_string()),
                },
                // Reject arms: real garbage that isn't the ch:64/empty sentinels.
                Case {
                    scenario: "non-sentinel garbage rejected",
                    input: "not-a-mac",
                    expect: Fails,
                },
                Case {
                    scenario: "too few octets",
                    input: "00:11:22:33:44",
                    expect: Fails,
                },
                Case {
                    scenario: "too many octets",
                    input: "00:11:22:33:44:55:66",
                    expect: Fails,
                },
                Case {
                    scenario: "octet out of hex range",
                    input: "zz:11:22:33:44:55",
                    expect: Fails,
                },
                Case {
                    scenario: "bare 12-hex parses and renders with colons",
                    input: "001122334455",
                    expect: Yields("00:11:22:33:44:55".to_string()),
                },
            ],
            |input| {
                deserialize_input_mac_to_address(input)
                    .map(|mac| mac.to_string())
                    .map_err(drop)
            },
        );
    }

    // The locally-administered bit is 0x02 of the first octet. Drives the table
    // off the parsed MAC's first octet so the bit math is exercised directly.
    #[test]
    fn is_locally_administered_mac_reads_the_u_l_bit() {
        check_cases(
            [
                Case {
                    scenario: "globally-unique NVIDIA-style mac (U/L bit clear)",
                    input: "A0:88:C2:46:0C:68",
                    expect: Yields(false),
                },
                Case {
                    scenario: "all zeros (U/L bit clear)",
                    input: "00:00:00:00:00:00",
                    expect: Yields(false),
                },
                Case {
                    scenario: "locally-administered bit set (0x02)",
                    input: "02:00:00:00:00:00",
                    expect: Yields(true),
                },
                Case {
                    scenario: "locally-administered among other first-octet bits (0x06)",
                    input: "06:11:22:33:44:55",
                    expect: Yields(true),
                },
                Case {
                    scenario: "multicast bit set but U/L bit clear (0x01)",
                    input: "01:00:00:00:00:00",
                    expect: Yields(false),
                },
                Case {
                    scenario: "all f's — every bit set, U/L included",
                    input: "FF:FF:FF:FF:FF:FF",
                    expect: Yields(true),
                },
            ],
            |input| {
                let mac = deserialize_input_mac_to_address(input).expect("valid test mac");
                Ok::<_, ()>(is_locally_administered_mac(mac))
            },
        );
    }
}
