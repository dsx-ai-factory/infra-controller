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

use serde::{Deserialize, Serialize};

use crate::address_selection_strategy::AddressSelectionStrategy;

/// Distinguishes how an IP address was allocated to a machine interface,
/// and are generally derived from the AddressSelectionStrategy used.
///
/// - `Dhcp`: These addresses allocated and managed by carbide-dhcp,
///   or a DHCP service that integrates directly with carbide-api.
/// - `Static`: These addresses are assigned and managed explicitly by
///   an operator or operator-provided configuration.
/// - `Slaac`: These IPv6 addresses were inferred from a client MAC using
///   modified EUI-64 during stateless DHCPv6 handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AllocationType {
    Dhcp,
    Static,
    Slaac,
}

impl From<AddressSelectionStrategy> for AllocationType {
    fn from(strategy: AddressSelectionStrategy) -> Self {
        match strategy {
            AddressSelectionStrategy::NextAvailableIp => AllocationType::Dhcp,
            AddressSelectionStrategy::Automatic => AllocationType::Dhcp,
            AddressSelectionStrategy::NextAvailablePrefix(_) => AllocationType::Dhcp,
            AddressSelectionStrategy::StaticAddress(_) => AllocationType::Static,
        }
    }
}

/// The result of assigning a static address, indicating what
/// previously existed for that address family on the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignStaticResult {
    /// No prior address existed for this family.
    Assigned,
    /// An existing static address was replaced.
    ReplacedStatic,
    /// An existing DHCP allocation was replaced.
    ///
    /// If you "replace" a DHCP allocation with the same address
    /// (effectively making a static DHCP  reservation), then it's
    /// basically a no-op.
    ///
    /// If you replace a DHCP allocation with a static address that
    /// is within a Carbide-managed network, then the next time the
    /// machine renews its lease, carbide-dhcp -> carbide-api will
    /// flow, and carbide-api will see the new IP and naturally
    /// return it. MOST DHCP clients will accept this new IP and
    /// reconfigure. SOME DHCP clients will see this is NOT their
    /// original offer, and re-DHCPDISCOVER, at which point the
    /// carbide-dhcp -> carbide-api flow will naturally return
    /// the static reservation anyway. It will be a small hiccup
    /// in a sense, but the client will never lose it's address,
    /// and will just re-discover to the same address.
    ///
    /// If you replace a DHCP allocation with a static address that
    /// is OUTSIDE a Carbide-managed network, then we will now assume
    /// that device is where you say it is. But it's important to
    /// understand a bit of a nuance, as soon as that previous DHCP
    /// allocation is deleted, it is eligible for re-assignment,
    /// meaning if your device is still holding onto that IP (before
    /// it's next renewal), there will potentially be a period of time
    /// where there are duplicate IP conflicts. We can definitely
    /// do some work to make sure these things are mitigated, but
    /// I also think replacing DHCP -> static reservations comes
    /// with some "use at your own risk" in general. We can improve
    /// on it if needed.
    ReplacedDhcp,
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;
    use crate::address_selection_strategy::AddressSelectionStrategy;

    // Total `From<AddressSelectionStrategy>` conversions: every strategy maps to an
    // allocation type. The conversion is infallible, so each row is a plain value
    // checked with `check_values`. Strategies that carry data are exercised across
    // boundary payloads (prefix length extremes, IPv4 vs IPv6 static addresses) to
    // confirm the discriminant alone — not the payload — drives the result.
    #[test]
    fn strategy_maps_to_allocation_type() {
        value_scenarios!(
            run = AllocationType::from;
            "next available ip -> dhcp" {
                AddressSelectionStrategy::NextAvailableIp => AllocationType::Dhcp,
            }

            "automatic alias -> dhcp" {
                AddressSelectionStrategy::Automatic => AllocationType::Dhcp,
            }

            "next available prefix /30 -> dhcp" {
                AddressSelectionStrategy::NextAvailablePrefix(30) => AllocationType::Dhcp,
            }

            "next available prefix /0 (boundary low) -> dhcp" {
                AddressSelectionStrategy::NextAvailablePrefix(0) => AllocationType::Dhcp,
            }

            "next available prefix /32 -> dhcp" {
                AddressSelectionStrategy::NextAvailablePrefix(32) => AllocationType::Dhcp,
            }

            "next available prefix /128 -> dhcp" {
                AddressSelectionStrategy::NextAvailablePrefix(128) => AllocationType::Dhcp,
            }

            "next available prefix /255 (boundary high) -> dhcp" {
                AddressSelectionStrategy::NextAvailablePrefix(u8::MAX) => AllocationType::Dhcp,
            }

            "static ipv4 -> static" {
                AddressSelectionStrategy::StaticAddress(
                    Ipv4Addr::new(10, 0, 0, 1).into(),
                ) => AllocationType::Static,
            }

            "static ipv4 unspecified (0.0.0.0) -> static" {
                AddressSelectionStrategy::StaticAddress(Ipv4Addr::UNSPECIFIED.into()) => AllocationType::Static,
            }

            "static ipv4 broadcast (255.255.255.255) -> static" {
                AddressSelectionStrategy::StaticAddress(Ipv4Addr::BROADCAST.into()) => AllocationType::Static,
            }

            "static ipv6 -> static" {
                AddressSelectionStrategy::StaticAddress(
                    Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1).into(),
                ) => AllocationType::Static,
            }

            "static ipv6 unspecified (::) -> static" {
                AddressSelectionStrategy::StaticAddress(Ipv6Addr::UNSPECIFIED.into()) => AllocationType::Static,
            }

            "static ipv6 localhost (::1) -> static" {
                AddressSelectionStrategy::StaticAddress(Ipv6Addr::LOCALHOST.into()) => AllocationType::Static,
            }
        );
    }

    // Serialization: each `AllocationType` variant renders to its snake_case wire
    // form. Total operation, so the produced string is checked directly.
    #[test]
    fn serializes_to_snake_case() {
        value_scenarios!(
            run = |value| serde_json::to_string(&value).expect("serialization is infallible");
            "dhcp" {
                AllocationType::Dhcp => r#""dhcp""#.to_string(),
            }

            "static" {
                AllocationType::Static => r#""static""#.to_string(),
            }

            "slaac" {
                AllocationType::Slaac => r#""slaac""#.to_string(),
            }
        );
    }

    // JSON round-trip: serialize each variant and deserialize it back, asserting
    // both the wire form and that it survives the trip. The closure returns the
    // wire string plus the recovered value so each row pins down both directions.
    #[test]
    fn serde_roundtrip() {
        scenarios!(
            // serialize, then deserialize the wire form back into a value
            run = |value| {
                let wire = serde_json::to_string(&value).map_err(drop)?;
                let recovered: AllocationType = serde_json::from_str(&wire).map_err(drop)?;
                Ok::<_, ()>((wire, recovered))
            };
            "dhcp" {
                AllocationType::Dhcp => Yields((r#""dhcp""#.to_string(), AllocationType::Dhcp)),
            }

            "static" {
                AllocationType::Static => Yields((r#""static""#.to_string(), AllocationType::Static)),
            }

            "slaac" {
                AllocationType::Slaac => Yields((r#""slaac""#.to_string(), AllocationType::Slaac)),
            }
        );
    }

    // Deserialization from the wire: accepted snake_case forms recover their
    // variant; anything else (wrong case, unknown tag, wrong JSON type, malformed)
    // is rejected. `serde_json::Error` is not `PartialEq`, so failures use `Fails`
    // with `.map_err(drop)`.
    #[test]
    fn deserializes_known_forms_and_rejects_the_rest() {
        scenarios!(
            run = |wire| serde_json::from_str::<AllocationType>(wire).map_err(drop);
            "dhcp" {
                r#""dhcp""# => Yields(AllocationType::Dhcp),
            }

            "static" {
                r#""static""# => Yields(AllocationType::Static),
            }

            "slaac" {
                r#""slaac""# => Yields(AllocationType::Slaac),
            }

            "wrong case rejected" {
                r#""Dhcp""# => Fails,
            }

            "uppercase rejected" {
                r#""STATIC""# => Fails,
            }

            "unknown variant rejected" {
                r#""bootp""# => Fails,
            }

            "empty string rejected" {
                r#""""# => Fails,
            }

            "leading whitespace in tag rejected" {
                r#"" dhcp""# => Fails,
            }

            "number rejected" {
                "0" => Fails,
            }

            "null rejected" {
                "null" => Fails,
            }

            "object rejected" {
                r#"{"dhcp":true}"# => Fails,
            }

            "malformed json rejected" {
                r#""dhcp"# => Fails,
            }
        );
    }

    // Derived `PartialEq`/`Eq` over `AllocationType`: a variant equals only itself.
    #[test]
    fn variants_compare_by_identity() {
        value_scenarios!(
            run = |(left, right)| left == right;
            "dhcp == dhcp" {
                (AllocationType::Dhcp, AllocationType::Dhcp) => true,
            }

            "static == static" {
                (AllocationType::Static, AllocationType::Static) => true,
            }

            "slaac == slaac" {
                (AllocationType::Slaac, AllocationType::Slaac) => true,
            }

            "dhcp != static" {
                (AllocationType::Dhcp, AllocationType::Static) => false,
            }

            "dhcp != slaac" {
                (AllocationType::Dhcp, AllocationType::Slaac) => false,
            }

            "static != slaac" {
                (AllocationType::Static, AllocationType::Slaac) => false,
            }
        );
    }
}
