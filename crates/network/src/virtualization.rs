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

#[cfg(feature = "ipnetwork")]
use ipnetwork::IpNetwork;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// DEFAULT_NETWORK_VIRTUALIZATION_TYPE is what to default to if the Cloud API
/// doesn't send it to NICo (which it never does), or if the NICo API
/// doesn't send it to the DPU agent.
pub const DEFAULT_NETWORK_VIRTUALIZATION_TYPE: VpcVirtualizationType =
    VpcVirtualizationType::EthernetVirtualizer;

/// VpcVirtualizationType is the type of network virtualization
/// being used for the environment. This is currently stored in the
/// database at the VPC level, but not actually plumbed down to the
/// DPU agent. Instead, the DPU agent just gets fed a
/// NetworkVirtualizationType based on the value of `nvue_enabled`.
///
/// The idea is with FNN, we'll actually mark a VPC as ETV or FNN,
/// and plumb the value down to the DPU agent, which gets piped into
/// the `update_nvue` function, which is then used to drive
/// population of the appropriate template.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpcVirtualizationType {
    #[default]
    EthernetVirtualizer,
    /// Deprecated: equivalent to `EthernetVirtualizer` for all live behavior;
    /// retained only so older database rows decode correctly. Treat the two
    /// variants as the same thing in match arms.
    EthernetVirtualizerWithNvue,
    Fnn,
    /// `Flat` is for VPCs whose tenant instances live directly on the
    /// underlay (zero-DPU hosts, or hosts with their DPU in NIC mode) and
    /// whose interfaces are bound to `HostInband` network segments rather
    /// than a NICo-managed overlay. Flat VPCs are still real tenant
    /// VPCs with a VNI and NSGs, but NICo doesn't drive their data
    /// plane -- routing and ACL enforcement between Flat VPCs and other
    /// VPCs is the network operator's responsibility.
    Flat,
}

impl VpcVirtualizationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EthernetVirtualizer | Self::EthernetVirtualizerWithNvue => "etv",
            Self::Fnn => "fnn",
            Self::Flat => "flat",
        }
    }
}

/// Custom Serialize implementation to use our custom string representation
impl Serialize for VpcVirtualizationType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Custom Deserialize implementation to use our custom string representation
impl<'de> Deserialize<'de> for VpcVirtualizationType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        <String>::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

// Per-variant policy ("how does this type behave with respect to segments,
// peering, routing profiles, IPv6, host fabric interfaces") is declared
// as data in `carbide_api_model::vpc::capability` and consulted via the
// `VpcVirtualizationTypeCapabilities` extension trait. There are no
// inherent methods here; adding a new variant means filling in one
// `VpcCapabilities` literal in that module, not editing handler logic.

// Manual sqlx impls so that legacy DB value 'etv' decodes as EthernetVirtualizerWithNvue.
#[cfg(feature = "sqlx")]
const PG_TYPE_NAME: &str = "network_virtualization_type_t";

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for VpcVirtualizationType {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name(PG_TYPE_NAME)
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Postgres> for VpcVirtualizationType {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode(self.as_str(), buf)
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::postgres::PgHasArrayType for VpcVirtualizationType {
    fn array_type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("_network_virtualization_type_t")
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Decode<'_, sqlx::Postgres> for VpcVirtualizationType {
    fn decode(value: sqlx::postgres::PgValueRef<'_>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        s.parse().map_err(sqlx::error::BoxDynError::from)
    }
}

#[cfg(all(test, feature = "sqlx"))]
mod sqlx_tests {
    use carbide_test_support::value_scenarios;
    use sqlx::Encode;
    use sqlx::postgres::PgArgumentBuffer;

    use super::VpcVirtualizationType;

    fn encode_to_string(v: VpcVirtualizationType) -> String {
        let mut buf = PgArgumentBuffer::default();
        let _ = v.encode_by_ref(&mut buf).unwrap();
        String::from_utf8(buf.to_vec()).unwrap()
    }

    #[test]
    fn encode_writes_the_wire_string_for_each_variant() {
        value_scenarios!(
            run = encode_to_string;
            "EthernetVirtualizer encodes as etv" {
                VpcVirtualizationType::EthernetVirtualizer => "etv".to_string(),
            }

            "EthernetVirtualizerWithNvue encodes as legacy etv" {
                VpcVirtualizationType::EthernetVirtualizerWithNvue => "etv".to_string(),
            }

            "Fnn encodes as fnn" {
                VpcVirtualizationType::Fnn => "fnn".to_string(),
            }

            "Flat encodes as flat" {
                VpcVirtualizationType::Flat => "flat".to_string(),
            }
        );
    }
}

// Real Postgres round-trips for the sqlx `Encode`/`Decode` impls. Each case binds
// a value (`Encode`) and reads it back (`Decode`) through a live connection, which
// is the half the buffer-only `encode` test above can't reach. The per-test pool
// comes from the shared harness (`DATABASE_URL` via `.envrc`); the closure does
// the database work, so `carbide-test-support` itself stays db-agnostic.
#[cfg(all(test, feature = "sqlx"))]
mod sqlx_db_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};

    use super::VpcVirtualizationType;

    #[crate::sqlx_test]
    async fn vpc_virtualization_type_round_trips_through_postgres(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        // The custom `network_virtualization_type_t` enum already exists in the
        // harness's schema, so we go straight to the round-trip.
        check_cases_async(
            [
                Case {
                    scenario: "etv round-trips",
                    input: VpcVirtualizationType::EthernetVirtualizer,
                    expect: Yields(VpcVirtualizationType::EthernetVirtualizer),
                },
                Case {
                    // Encodes to the shared `etv` label, so it decodes back to the
                    // canonical EthernetVirtualizer rather than the nvue alias.
                    scenario: "etv-with-nvue collapses onto etv on the way back",
                    input: VpcVirtualizationType::EthernetVirtualizerWithNvue,
                    expect: Yields(VpcVirtualizationType::EthernetVirtualizer),
                },
                Case {
                    scenario: "fnn round-trips",
                    input: VpcVirtualizationType::Fnn,
                    expect: Yields(VpcVirtualizationType::Fnn),
                },
                Case {
                    scenario: "flat round-trips",
                    input: VpcVirtualizationType::Flat,
                    expect: Yields(VpcVirtualizationType::Flat),
                },
            ],
            |v: VpcVirtualizationType| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, VpcVirtualizationType>(
                        "SELECT $1::network_virtualization_type_t",
                    )
                    .bind(v)
                    .fetch_one(&pool)
                    .await
                    .map_err(drop)
                }
            },
        )
        .await;
        Ok(())
    }
}

impl fmt::Display for VpcVirtualizationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Concatenate IPv4 and IPv6 values in family order. Empty strings and `None`
/// represent absent families and are omitted.
pub fn build_dual_stack_list(v4: Option<String>, v6: Option<String>) -> Vec<String> {
    v4.into_iter()
        .chain(v6)
        .filter(|value| !value.is_empty())
        .collect()
}

impl FromStr for VpcVirtualizationType {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "etv" | "etv_nvue" => Ok(Self::EthernetVirtualizer),
            "fnn" => Ok(Self::Fnn),
            "flat" => Ok(Self::Flat),
            x => Err(eyre::eyre!(format!("unknown virt type {}", x))),
        }
    }
}

#[cfg(feature = "ipnetwork")]
/// get_host_ip returns the host IP for a tenant instance
/// for a given IpNetwork. This is being initially introduced
/// for the purpose of FNN /30 allocations (where the host IP
/// ends up being the 4th IP -- aka the second IP of the second
/// /31 allocation in the /30), and will probably change with
/// a wider refactor + intro of NICo IP Prefix Management.
pub fn get_host_ip(network: &IpNetwork) -> eyre::Result<std::net::IpAddr> {
    match network.prefix() {
        // Single-host allocation: IPv4 /32 or IPv6 /128
        32 | 128 => Ok(network.ip()),
        // Point-to-point linknet: IPv4 /31 (RFC 3021) or IPv6 /127 (RFC 6164)
        // The second address is the host IP.
        31 | 127 => match network.iter().nth(1) {
            Some(ip_addr) => Ok(ip_addr),
            None => Err(eyre::eyre!(
                "no viable host IP found in point-to-point network: {}",
                network
            )),
        },
        // Legacy /30 allocation: host IP is the 4th address
        30 => match network.iter().nth(3) {
            Some(ip_addr) => Ok(ip_addr),
            None => Err(eyre::eyre!(
                "no viable host IP found in network: {}",
                network
            )),
        },
        _ => Err(eyre::eyre!(
            "tenant instance network size unsupported: {}",
            network.prefix()
        )),
    }
}

#[cfg(feature = "ipnetwork")]
/// get_svi_ip returns the SVI IP (also known as the gateway IP)
/// for a tenant instance for a given IpNetwork. This is valid only for l2 segments under FNN.
pub fn get_svi_ip(
    svi_ip: &Option<std::net::IpAddr>,
    virtualization_type: VpcVirtualizationType,
    is_l2_segment: bool,
    prefix: u8,
) -> eyre::Result<Option<IpNetwork>> {
    if virtualization_type == VpcVirtualizationType::Fnn && is_l2_segment {
        let Some(svi_ip) = svi_ip else {
            return Err(eyre::eyre!(format!("SVI IP is not allocated",)));
        };

        return Ok(Some(IpNetwork::new(*svi_ip, prefix)?));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn from_str_maps_known_strings_and_rejects_the_rest() {
        scenarios!(
            run = |s| s.parse::<VpcVirtualizationType>().map_err(drop);
            "etv -> EthernetVirtualizer" {
                "etv" => Yields(VpcVirtualizationType::EthernetVirtualizer),
            }

            "etv_nvue is an alias for EthernetVirtualizer" {
                "etv_nvue" => Yields(VpcVirtualizationType::EthernetVirtualizer),
            }

            "fnn -> Fnn" {
                "fnn" => Yields(VpcVirtualizationType::Fnn),
            }

            "flat -> Flat" {
                "flat" => Yields(VpcVirtualizationType::Flat),
            }

            "unknown token is rejected" {
                "bogus" => Fails,
            }

            "empty string is rejected" {
                "" => Fails,
            }

            "wrong case is rejected (match is exact)" {
                "ETV" => Fails,
            }

            "leading whitespace is not trimmed" {
                " etv" => Fails,
            }

            "trailing whitespace is not trimmed" {
                "flat " => Fails,
            }

            "etv-nvue with a hyphen is not the alias" {
                "etv-nvue" => Fails,
            }
        );
    }

    #[test]
    fn from_str_unknown_token_reports_the_input() {
        scenarios!(
            run = |(value, tokens)| {
                let produced = value
                    .parse::<VpcVirtualizationType>()
                    .map_err(|e| e.to_string())
                    .expect_err("unknown token must fail");
                Ok::<_, ()>(tokens.iter().all(|t| produced.contains(t)))
            };
            "error names the unknown token" {
                ("bogus", &["unknown virt type", "bogus"][..]) => Yields(true),
            }

            "error echoes a numeric token" {
                ("42", &["unknown virt type", "42"][..]) => Yields(true),
            }
        );
    }

    #[test]
    fn as_str_renders_each_variant() {
        value_scenarios!(
            run = |v| v.as_str();
            "EthernetVirtualizer -> etv" {
                VpcVirtualizationType::EthernetVirtualizer => "etv",
            }

            "EthernetVirtualizerWithNvue -> etv" {
                VpcVirtualizationType::EthernetVirtualizerWithNvue => "etv",
            }

            "Fnn -> fnn" {
                VpcVirtualizationType::Fnn => "fnn",
            }

            "Flat -> flat" {
                VpcVirtualizationType::Flat => "flat",
            }
        );
    }

    #[test]
    fn display_renders_each_variant() {
        value_scenarios!(
            run = |v| v.to_string();
            "EthernetVirtualizer displays etv" {
                VpcVirtualizationType::EthernetVirtualizer => "etv".to_string(),
            }

            "EthernetVirtualizerWithNvue displays etv" {
                VpcVirtualizationType::EthernetVirtualizerWithNvue => "etv".to_string(),
            }

            "Fnn displays fnn" {
                VpcVirtualizationType::Fnn => "fnn".to_string(),
            }

            "Flat displays flat" {
                VpcVirtualizationType::Flat => "flat".to_string(),
            }
        );
    }

    #[test]
    fn display_agrees_with_as_str_for_every_variant() {
        value_scenarios!(
            run = |v| v.to_string() == v.as_str();
            "EthernetVirtualizer" {
                VpcVirtualizationType::EthernetVirtualizer => true,
            }

            "EthernetVirtualizerWithNvue" {
                VpcVirtualizationType::EthernetVirtualizerWithNvue => true,
            }

            "Fnn" {
                VpcVirtualizationType::Fnn => true,
            }

            "Flat" {
                VpcVirtualizationType::Flat => true,
            }
        );
    }

    #[test]
    fn default_is_ethernet_virtualizer() {
        value_scenarios!(
            run = |()| VpcVirtualizationType::default();
            "Default trait yields EthernetVirtualizer" {
                () => VpcVirtualizationType::EthernetVirtualizer,
            }

            "the DEFAULT_* constant agrees with Default" {
                () => DEFAULT_NETWORK_VIRTUALIZATION_TYPE,
            }
        );
    }

    #[test]
    fn from_str_round_trips_through_as_str() {
        scenarios!(
            run = |v| v.as_str().parse::<VpcVirtualizationType>().map_err(drop);
            "EthernetVirtualizer" {
                VpcVirtualizationType::EthernetVirtualizer => Yields(VpcVirtualizationType::EthernetVirtualizer),
            }

            "Fnn" {
                VpcVirtualizationType::Fnn => Yields(VpcVirtualizationType::Fnn),
            }

            "Flat" {
                VpcVirtualizationType::Flat => Yields(VpcVirtualizationType::Flat),
            }
        );
    }

    #[test]
    fn serde_round_trips_through_json() {
        scenarios!(
            run = |v| serde_json::to_string(&v).map_err(drop);
            "EthernetVirtualizer serializes to \"etv\"" {
                VpcVirtualizationType::EthernetVirtualizer => Yields("\"etv\"".to_string()),
            }

            "EthernetVirtualizerWithNvue also serializes to \"etv\"" {
                VpcVirtualizationType::EthernetVirtualizerWithNvue => Yields("\"etv\"".to_string()),
            }

            "Fnn serializes to \"fnn\"" {
                VpcVirtualizationType::Fnn => Yields("\"fnn\"".to_string()),
            }

            "Flat serializes to \"flat\"" {
                VpcVirtualizationType::Flat => Yields("\"flat\"".to_string()),
            }
        );
    }

    #[test]
    fn deserialize_accepts_known_strings_and_rejects_the_rest() {
        scenarios!(
            run = |json| serde_json::from_str::<VpcVirtualizationType>(json).map_err(drop);
            "\"etv\" -> EthernetVirtualizer" {
                "\"etv\"" => Yields(VpcVirtualizationType::EthernetVirtualizer),
            }

            "\"etv_nvue\" alias -> EthernetVirtualizer" {
                "\"etv_nvue\"" => Yields(VpcVirtualizationType::EthernetVirtualizer),
            }

            "\"fnn\" -> Fnn" {
                "\"fnn\"" => Yields(VpcVirtualizationType::Fnn),
            }

            "\"flat\" -> Flat" {
                "\"flat\"" => Yields(VpcVirtualizationType::Flat),
            }

            "unknown string is rejected" {
                "\"bogus\"" => Fails,
            }

            "a JSON number is rejected (expects a string)" {
                "7" => Fails,
            }

            "a JSON null is rejected" {
                "null" => Fails,
            }
        );
    }

    #[test]
    fn build_dual_stack_list_assembles_the_address_list() {
        value_scenarios!(
            run = |(v4, v6)| build_dual_stack_list(v4, v6);
            "v4 only when v6 is absent" {
                (Some("10.0.0.1".to_string()), None) => vec!["10.0.0.1".to_string()],
            }

            "v4 then v6 when both present" {
                (Some("10.0.0.1".to_string()), Some("2001:db8::1".to_string())) => vec!["10.0.0.1".to_string(), "2001:db8::1".to_string()],
            }

            "empty v6 string is filtered out" {
                (Some("10.0.0.1".to_string()), Some(String::new())) => vec!["10.0.0.1".to_string()],
            }

            "both families absent" {
                (None, None) => vec![],
            }

            "v6 only when v4 is absent" {
                (None, Some("2001:db8::1".to_string())) => vec!["2001:db8::1".to_string()],
            }

            "empty strings for both families are absent" {
                (Some(String::new()), Some(String::new())) => vec![],
            }
        );
    }

    #[cfg(feature = "ipnetwork")]
    mod ip {
        use std::net::IpAddr;

        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        use super::super::*;

        fn net(ip: &str, prefix: u8) -> IpNetwork {
            IpNetwork::new(ip.parse().unwrap(), prefix).unwrap()
        }

        #[test]
        fn get_host_ip_picks_the_right_address_per_prefix() {
            scenarios!(
                run = |network| get_host_ip(&network).map_err(drop);
                "ipv4 /32 single host is itself" {
                    net("10.0.0.5", 32) => Yields("10.0.0.5".parse::<IpAddr>().unwrap()),
                }

                "ipv6 /128 single host is itself" {
                    net("2001:db8::1", 128) => Yields("2001:db8::1".parse::<IpAddr>().unwrap()),
                }

                "ipv4 /31 point-to-point uses the second address" {
                    net("10.0.0.0", 31) => Yields("10.0.0.1".parse::<IpAddr>().unwrap()),
                }

                "ipv6 /127 point-to-point uses the second address" {
                    net("2001:db8::0", 127) => Yields("2001:db8::1".parse::<IpAddr>().unwrap()),
                }

                "ipv4 /30 legacy uses the fourth address" {
                    net("10.0.0.0", 30) => Yields("10.0.0.3".parse::<IpAddr>().unwrap()),
                }

                "ipv4 /29 is too large to be supported" {
                    net("10.0.0.0", 29) => Fails,
                }

                "ipv4 /24 is unsupported" {
                    net("10.0.0.0", 24) => Fails,
                }

                "ipv4 /0 is unsupported" {
                    net("0.0.0.0", 0) => Fails,
                }

                "ipv6 /64 is unsupported" {
                    net("2001:db8::", 64) => Fails,
                }

                "ipv6 /126 is unsupported" {
                    net("2001:db8::", 126) => Fails,
                }
            );
        }

        #[test]
        fn get_host_ip_unsupported_prefix_names_the_problem() {
            scenarios!(
                run = |(network, tokens)| {
                    let produced = get_host_ip(&network)
                        .map_err(|e| e.to_string())
                        .expect_err("oversized prefix must fail");
                    Ok::<_, ()>(tokens.iter().all(|t| produced.contains(t)))
                };
                "error mentions it is unsupported" {
                    (net("10.0.0.0", 29), &["unsupported"][..]) => Yields(true),
                }
            );
        }

        #[test]
        fn get_svi_ip_only_yields_for_fnn_l2_segments() {
            let svi_ip: IpAddr = "10.0.0.1".parse().unwrap();
            let svi: Option<IpAddr> = Some(svi_ip);
            scenarios!(
                run = |(svi_ip, virt, is_l2, prefix)| {
                    get_svi_ip(&svi_ip, virt, is_l2, prefix).map_err(drop)
                };
                "fnn + l2 + allocated svi yields the network" {
                    (svi, VpcVirtualizationType::Fnn, true, 24u8) => Yields(Some(IpNetwork::new(svi_ip, 24).unwrap())),
                }

                "fnn + l2 but no svi allocated fails" {
                    (None, VpcVirtualizationType::Fnn, true, 24u8) => Fails,
                }

                "fnn but not an l2 segment yields none" {
                    (svi, VpcVirtualizationType::Fnn, false, 24u8) => Yields(None),
                }

                "l2 but not fnn yields none" {
                    (svi, VpcVirtualizationType::EthernetVirtualizer, true, 24u8) => Yields(None),
                }

                "flat l2 segment yields none" {
                    (svi, VpcVirtualizationType::Flat, true, 24u8) => Yields(None),
                }

                "neither fnn nor l2 yields none" {
                    (svi, VpcVirtualizationType::EthernetVirtualizer, false, 24u8) => Yields(None),
                }

                "fnn + l2 + invalid prefix for ipv4 fails" {
                    (svi, VpcVirtualizationType::Fnn, true, 33u8) => Fails,
                }
            );
        }
    }
}
