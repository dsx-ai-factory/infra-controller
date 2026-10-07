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

use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::sanitized_mac;

/// This type represent base mac that is reported by DPU. It is
/// serialized as MAC-address without ':' separator and can be parsed
/// from any MAC-address representation acceptable by sanitized_mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BaseMac(MacAddress);

impl BaseMac {
    pub fn to_mac(self) -> MacAddress {
        self.0
    }
}

impl From<MacAddress> for BaseMac {
    fn from(v: MacAddress) -> Self {
        Self(v)
    }
}

impl fmt::Display for BaseMac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0.bytes();
        let _ = write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
        );
        Ok(())
    }
}

impl FromStr for BaseMac {
    type Err = eyre::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        sanitized_mac(s).map(BaseMac)
    }
}

impl Serialize for BaseMac {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BaseMac {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        Self::from_str(&str_value).map_err(|err| Error::custom(err.to_string()))
    }
}

#[cfg(test)]
mod test {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    fn mac(bytes: [u8; 6]) -> BaseMac {
        BaseMac(MacAddress::new(bytes))
    }

    // --- Display: total, uppercase hex, no colons ---

    #[test]
    fn display_formats_uppercase_hex_without_colons() {
        value_scenarios!(
            run = |m| m.to_string();
            "ascending low bytes" {
                mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]) => "010203040506".to_string(),
            }

            "high bytes uppercase" {
                mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]) => "AABBCCDDEEFF".to_string(),
            }

            "all zeros" {
                mac([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]) => "000000000000".to_string(),
            }

            "all ones" {
                mac([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]) => "FFFFFFFFFFFF".to_string(),
            }

            "single-digit bytes zero-padded" {
                mac([0x00, 0x0A, 0x00, 0x0B, 0x00, 0x0C]) => "000A000B000C".to_string(),
            }

            "lowercase-hex digits rendered uppercase" {
                mac([0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f]) => "0A0B0C0D0E0F".to_string(),
            }

            "mixed bytes" {
                mac([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54]) => "FEDCBA987654".to_string(),
            }
        );
    }

    // --- to_mac: total getter, returns the wrapped MacAddress ---

    #[test]
    fn to_mac_returns_wrapped_address() {
        value_scenarios!(
            run = |m| m.to_mac();
            "ascending bytes" {
                mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]) => MacAddress::new([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            }

            "zeros" {
                mac([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]) => MacAddress::new([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            }

            "high bytes" {
                mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]) => MacAddress::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]),
            }
        );
    }

    // --- From<MacAddress>: total conversion ---

    #[test]
    fn from_macaddress_wraps_unchanged() {
        value_scenarios!(
            run = BaseMac::from;
            "ascending bytes" {
                MacAddress::new([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]) => mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            }

            "high bytes" {
                MacAddress::new([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54]) => mac([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54]),
            }
        );
    }

    // --- FromStr: fallible. eyre::Error is not PartialEq -> Fails + map_err(drop). ---
    // Ok rows compare on the rendered Display form; Err rows assert failure.

    #[test]
    fn from_str_parses_accepted_forms() {
        check_cases(
            [
                Case {
                    scenario: "raw hex, no separators",
                    input: "010203040506",
                    expect: Yields("010203040506".to_string()),
                },
                Case {
                    scenario: "colon separated",
                    input: "01:02:03:04:05:06",
                    expect: Yields("010203040506".to_string()),
                },
                Case {
                    scenario: "space separated",
                    input: "01 02 03 04 05 06",
                    expect: Yields("010203040506".to_string()),
                },
                Case {
                    scenario: "hyphen separated",
                    input: "01-02-03-04-05-06",
                    expect: Yields("010203040506".to_string()),
                },
                Case {
                    scenario: "mixed case normalized to uppercase",
                    input: "AaBbCcDdEeFf",
                    expect: Yields("AABBCCDDEEFF".to_string()),
                },
                Case {
                    scenario: "lowercase normalized to uppercase",
                    input: "aabbccddeeff",
                    expect: Yields("AABBCCDDEEFF".to_string()),
                },
                Case {
                    scenario: "all zeros",
                    input: "000000000000",
                    expect: Yields("000000000000".to_string()),
                },
                Case {
                    scenario: "all ff",
                    input: "ffffffffffff",
                    expect: Yields("FFFFFFFFFFFF".to_string()),
                },
                Case {
                    scenario: "runs of whitespace between bytes",
                    input: "a088c2    460c68",
                    expect: Yields("A088C2460C68".to_string()),
                },
                Case {
                    // Dots are non-hex, so they're stripped and the 12-hex-digit
                    // residue is accepted -- a side effect of the stripping, not a
                    // deliberately supported separator.
                    scenario: "non-hex separators stripped",
                    input: "0a.0b.0c.0d.0e.0f",
                    expect: Yields("0A0B0C0D0E0F".to_string()),
                },
                Case {
                    scenario: "empty string has zero hex digits",
                    input: "",
                    expect: Fails,
                },
                Case {
                    scenario: "too short by one byte",
                    input: "0102030405",
                    expect: Fails,
                },
                Case {
                    scenario: "too long by one byte",
                    input: "0102030405060708",
                    expect: Fails,
                },
                Case {
                    scenario: "one extra hex digit",
                    input: "0102030405067",
                    expect: Fails,
                },
                Case {
                    scenario: "only non-hex characters",
                    input: "invalid-mac",
                    expect: Fails,
                },
                Case {
                    scenario: "non-hex letters dropped leaving too few digits",
                    input: "gg:hh:ii:jj:kk:ll",
                    expect: Fails,
                },
                Case {
                    scenario: "whitespace only",
                    input: "   ",
                    expect: Fails,
                },
            ],
            |s| BaseMac::from_str(s).map(|m| m.to_string()).map_err(drop),
        );
    }

    #[test]
    fn from_str_rejects_with_invalid_length_message() {
        // Overlaps with `from_str_parses_accepted_forms` on the same inputs by
        // design: that test pins *which* inputs fail, this one pins *what the
        // error message says*.
        scenarios!(
            run = |(value, tokens)| {
                let produced = BaseMac::from_str(value).unwrap_err().to_string();
                Ok::<_, ()>(tokens.iter().all(|t| produced.contains(t)))
            };
            "too short reports invalid length" {
                ("0102030405", &["invalid stripped MAC length"][..]) => Yields(true),
            }

            "too long reports invalid length" {
                ("0102030405060708", &["invalid stripped MAC length"][..]) => Yields(true),
            }

            "empty reports invalid length" {
                ("", &["invalid stripped MAC length"][..]) => Yields(true),
            }
        );
    }

    // --- Serialize: produces the colon-free uppercase Display string, JSON-quoted. ---

    #[test]
    fn serialize_emits_quoted_display_string() {
        scenarios!(
            run = |m| serde_json::to_string(&m).map_err(drop);
            "ascending bytes" {
                mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]) => Yields("\"010203040506\"".to_string()),
            }

            "high bytes uppercase" {
                mac([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54]) => Yields("\"FEDCBA987654\"".to_string()),
            }

            "zeros" {
                mac([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]) => Yields("\"000000000000\"".to_string()),
            }
        );
    }

    // --- Deserialize: fallible. serde_json::Error is not relied on for equality. ---

    #[test]
    fn deserialize_parses_accepted_json_strings() {
        scenarios!(
            run = |json| {
                serde_json::from_str::<BaseMac>(json)
                    .map(|m| m.to_string())
                    .map_err(drop)
            };
            "raw hex string" {
                "\"0a0b0c0d0e0f\"" => Yields("0A0B0C0D0E0F".to_string()),
            }

            "colon separated string" {
                "\"11:22:33:44:55:66\"" => Yields("112233445566".to_string()),
            }

            "uppercase round-trips" {
                "\"FEDCBA987654\"" => Yields("FEDCBA987654".to_string()),
            }

            "non-string json number" {
                "1234" => Fails,
            }

            "null" {
                "null" => Fails,
            }

            "invalid mac text" {
                "\"invalid-mac\"" => Fails,
            }

            "too-short string" {
                "\"0102030405\"" => Fails,
            }

            "empty string" {
                "\"\"" => Fails,
            }
        );
    }

    #[test]
    fn deserialize_failure_carries_invalid_length_message() {
        scenarios!(
            run = |(json, tokens)| {
                let produced = serde_json::from_str::<BaseMac>(json)
                    .unwrap_err()
                    .to_string();
                Ok::<_, ()>(tokens.iter().all(|t| produced.contains(t)))
            };
            "invalid text reports length" {
                ("\"invalid-mac\"", &["invalid stripped MAC length"][..]) => Yields(true),
            }

            "short string reports length" {
                ("\"0102030405\"", &["invalid stripped MAC length"][..]) => Yields(true),
            }
        );
    }

    // --- Round trip: serialize then deserialize yields the original value. ---

    #[test]
    fn round_trips_through_json() {
        scenarios!(
            run = |original| {
                let serialized = serde_json::to_string(&original).map_err(drop)?;
                serde_json::from_str::<BaseMac>(&serialized).map_err(drop)
            };
            "high bytes" {
                mac([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54]) => Yields(mac([0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54])),
            }

            "ascending bytes" {
                mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]) => Yields(mac([0x01, 0x02, 0x03, 0x04, 0x05, 0x06])),
            }

            "zeros" {
                mac([0x00, 0x00, 0x00, 0x00, 0x00, 0x00]) => Yields(mac([0x00, 0x00, 0x00, 0x00, 0x00, 0x00])),
            }
        );
    }
}
