// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_test_support::Outcome::*;
use carbide_test_support::{check_cases, scenarios};
use model::resource_pool::Range;
use v1::address_prefix::Prefix;
use v1::eligibility::Entries;

use super::*;

#[test]
fn definitions_normalize_prefixes_without_expanding_inventory() {
    struct Case {
        name: &'static str,
        pool_type: ResourcePoolType,
        prefix: &'static str,
        delegate_prefix_len: Option<u8>,
        domain: ResourcePoolValueDomain,
        expected: Entries,
    }

    let cases = [
        Case {
            name: "IPv4 dotted mask normalizes host bits; delegation length is ignored",
            pool_type: ResourcePoolType::Ipv4,
            prefix: "192.0.2.5/255.255.255.0",
            delegate_prefix_len: Some(1),
            domain: ResourcePoolValueDomain::IpAddress,
            expected: Entries::AddressPrefix(v1::AddressPrefix {
                prefix: Some(Prefix::Ipv4(v1::Ipv4Prefix {
                    network: vec![192, 0, 2, 0],
                    prefix_len: 24,
                })),
            }),
        },
        Case {
            name: "IPv6 address prefix host bits",
            pool_type: ResourcePoolType::Ipv6,
            prefix: "2001:db8::5/64",
            delegate_prefix_len: None,
            domain: ResourcePoolValueDomain::IpAddress,
            expected: Entries::AddressPrefix(v1::AddressPrefix {
                prefix: Some(Prefix::Ipv6(v1::Ipv6Prefix {
                    network: "2001:db8::".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
                    prefix_len: 64,
                })),
            }),
        },
        Case {
            name: "large delegation with parent host bits",
            pool_type: ResourcePoolType::Ipv6Prefix,
            prefix: "2001:db8::5/0",
            delegate_prefix_len: Some(64),
            domain: ResourcePoolValueDomain::Ipv6Prefix,
            expected: Entries::DelegatedPrefix(v1::DelegatedPrefix {
                prefix: Some(v1::Ipv6Prefix {
                    network: vec![0; 16],
                    prefix_len: 0,
                }),
                delegate_prefix_len: 64,
            }),
        },
    ];

    check_cases(
        cases.map(|case| carbide_test_support::Case {
            scenario: case.name,
            input: ResourcePoolDef {
                pool_type: case.pool_type,
                prefix: Some(case.prefix.to_string()),
                ranges: vec![],
                delegate_prefix_len: case.delegate_prefix_len,
            },
            expect: Yields(ResourcePoolEligibility {
                domain: case.domain,
                entries: vec![v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Automatic as i32,
                    entries: Some(case.expected),
                }],
            }),
        }),
        |definition| ResourcePoolEligibility::try_from(definition).map_err(drop),
    );
}

#[test]
fn definitions_preserve_source_selection_and_prefix_range_validation() {
    let prefix = ResourcePoolDef {
        pool_type: ResourcePoolType::Ipv6Prefix,
        prefix: Some("2001:db8::/48".to_string()),
        ranges: vec![],
        delegate_prefix_len: None,
    };
    scenarios!(
        run = |definition| ResourcePoolEligibility::try_from(definition).map(drop).map_err(drop);
        "one source required" {
            ResourcePoolDef {
                prefix: None,
                ..prefix.clone()
            } => Fails,
            ResourcePoolDef {
                ranges: vec![Range {
                    start: "2001:db8::/64".to_string(),
                    end: "2001:db8:0:1::/64".to_string(),
                    auto_assign: true,
                }],
                delegate_prefix_len: Some(64),
                ..prefix.clone()
            } => Fails,
        }
        "integer pools cannot use a prefix" {
            ResourcePoolDef {
                pool_type: ResourcePoolType::Integer,
                prefix: Some("192.0.2.0/24".to_string()),
                ranges: vec![],
                delegate_prefix_len: None,
            } => Fails,
        }
        "delegation length required" {
            prefix => Fails,
        }
        "prefix ranges must retain their host bits for rejection" {
            range_definition(ResourcePoolType::Ipv6Prefix, "2001:db8::1/64", "2001:db8:0:1::/64") => Fails,
        }
    );
}

#[test]
fn address_definitions_reject_the_other_family() {
    scenarios!(
        run = |definition| ResourcePoolEligibility::try_from(definition).map(drop).map_err(drop);
        "prefix family must match the definition" {
            ResourcePoolDef {
                pool_type: ResourcePoolType::Ipv4,
                prefix: Some("2001:db8::/64".to_string()),
                ranges: vec![],
                delegate_prefix_len: None,
            } => Fails,
            ResourcePoolDef {
                pool_type: ResourcePoolType::Ipv6,
                prefix: Some("192.0.2.0/24".to_string()),
                ranges: vec![],
                delegate_prefix_len: None,
            } => Fails,
        }
        "range family must match the definition" {
            range_definition(ResourcePoolType::Ipv4, "2001:db8::1", "2001:db8::4") => Fails,
            range_definition(ResourcePoolType::Ipv6, "192.0.2.1", "192.0.2.4") => Fails,
        }
    );
}

#[test]
fn address_and_prefix_ranges_preserve_exact_endpoints() {
    scenarios!(
        run = |(pool_type, start, end)| ResourcePoolEligibility::try_from(
            range_definition(pool_type, start, end),
        ).map_err(drop);
        "IPv4 address range" {
            (ResourcePoolType::Ipv4, "192.0.2.1", "192.0.2.4") => Yields(ResourcePoolEligibility {
                domain: ResourcePoolValueDomain::IpAddress,
                entries: vec![v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Automatic as i32,
                    entries: Some(Entries::Range(v1::ValueRange {
                        start: Some(Ipv4Addr::new(192, 0, 2, 1).into()),
                        end: Some(Ipv4Addr::new(192, 0, 2, 4).into()),
                    })),
                }],
            }),
        }
        "IPv6 address range" {
            (ResourcePoolType::Ipv6, "2001:db8::1", "2001:db8::4") => Yields(ResourcePoolEligibility {
                domain: ResourcePoolValueDomain::IpAddress,
                entries: vec![v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Automatic as i32,
                    entries: Some(Entries::Range(v1::ValueRange {
                        start: Some("2001:db8::1".parse::<Ipv6Addr>().unwrap().into()),
                        end: Some("2001:db8::4".parse::<Ipv6Addr>().unwrap().into()),
                    })),
                }],
            }),
        }
        "aligned IPv6 prefix range" {
            (ResourcePoolType::Ipv6Prefix, "2001:db8::/64", "2001:db8:0:4::/64") => Yields(ResourcePoolEligibility {
                domain: ResourcePoolValueDomain::Ipv6Prefix,
                entries: vec![v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Automatic as i32,
                    entries: Some(Entries::Range(v1::ValueRange {
                        start: Some(v1::Value {
                            kind: Some(v1::value::Kind::Ipv6Prefix(v1::Ipv6Prefix {
                                network: "2001:db8::".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
                                prefix_len: 64,
                            })),
                        }),
                        end: Some(v1::Value {
                            kind: Some(v1::value::Kind::Ipv6Prefix(v1::Ipv6Prefix {
                                network: "2001:db8:0:4::".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
                                prefix_len: 64,
                            })),
                        }),
                    })),
                }],
            }),
        }
    );
}

#[test]
fn integer_ranges_keep_integrated_parsing_and_empty_range_semantics() {
    scenarios!(
        run = |(start, end)| {
            let converted = ResourcePoolEligibility::try_from(range_definition(
                ResourcePoolType::Integer, start, end,
            )).map_err(drop)?;
            Ok::<_, ()>(converted.entries)
        };
        "shared parser accepts hexadecimal bounds" {
            ("0x10", "0x20") => Yields(vec![wire_range(16, 32, v1::AssignmentMode::Automatic)]),
        }
        "reversed ranges remain empty declarations" {
            ("2", "1") => Yields(vec![wire_range(2, 1, v1::AssignmentMode::Automatic)]),
        }
    );

    assert!(matches!(
        ResourcePoolEligibility::try_from(
            range_definition(ResourcePoolType::Integer, "-0x1", "1",)
        ),
        Err(ValueConversionError::Integer(_))
    ));
}

#[test]
fn overlapping_declarations_keep_the_first_assignment_mode() {
    let converted = ResourcePoolEligibility::try_from(ResourcePoolDef {
        pool_type: ResourcePoolType::Integer,
        prefix: None,
        ranges: vec![
            Range {
                start: "1".to_string(),
                end: "4".to_string(),
                auto_assign: false,
            },
            Range {
                start: "3".to_string(),
                end: "6".to_string(),
                auto_assign: true,
            },
        ],
        // Range declarations ignore the delegation length.
        delegate_prefix_len: Some(99),
    })
    .unwrap();
    assert_eq!(
        converted.entries,
        vec![
            wire_range(1, 4, v1::AssignmentMode::Manual),
            wire_range(3, 6, v1::AssignmentMode::Automatic),
        ]
    );

    scenarios!(
        run = |(value, mode)| converted.validate_allocation(&v1::Allocation {
            value: Some(v1::Value::from(value)),
            assignment_mode: mode as i32,
            ..Default::default()
        }).map_err(drop);
        "first declaration owns the overlap" {
            (3_i64, v1::AssignmentMode::Manual) => Yields(()),
            (3, v1::AssignmentMode::Automatic) => Fails,
        }
        "later declaration and exclusive upper endpoint" {
            (5, v1::AssignmentMode::Automatic) => Yields(()),
            (6, v1::AssignmentMode::Automatic) => Fails,
        }
    );
}

#[test]
fn allocation_validation_checks_the_saved_domain_and_every_declaration() {
    let eligibility =
        ResourcePoolEligibility::try_from(range_definition(ResourcePoolType::Integer, "1", "4"))
            .unwrap();
    let conflicting_domain = ResourcePoolEligibility {
        domain: ResourcePoolValueDomain::IpAddress,
        entries: eligibility.entries.clone(),
    };
    let mut malformed_tail = eligibility.clone();
    malformed_tail.entries.push(v1::Eligibility {
        assignment_mode: v1::AssignmentMode::Unspecified as i32,
        ..wire_range(5, 8, v1::AssignmentMode::Automatic)
    });
    scenarios!(
        run = |(eligibility, value): (ResourcePoolEligibility, Option<v1::Value>)| eligibility.validate_allocation(&v1::Allocation {
            value,
            assignment_mode: v1::AssignmentMode::Automatic as i32,
            ..Default::default()
        }).map_err(|error| error.to_string());
        "required allocation value" {
            (eligibility, None) => FailsWith("invalid resource pool allocation: value is required".to_string()),
        }
        "saved domain rejects a value even when an entry matches" {
            (conflicting_domain, Some(2_i64.into())) => FailsWith("invalid resource pool wire value or eligibility".to_string()),
        }
        "a matching entry cannot hide a malformed later declaration" {
            (malformed_tail, Some(2_i64.into())) => FailsWith("invalid resource pool wire value or eligibility".to_string()),
        }
    );
}

#[test]
fn integer_values_round_trip_without_narrowing_the_wire() {
    scenarios!(
        run = |value| {
            let wire = value_to_wire(ResourcePoolValueDomain::Integer, &value).map_err(drop)?;
            assert_eq!(wire, v1::Value::from(value));
            value_from_wire::<i64>(ResourcePoolValueDomain::Integer, &wire).map_err(drop)
        };
        "signed 64-bit endpoints" {
            i64::MIN => Yields(i64::MIN),
            i64::MAX => Yields(i64::MAX),
        }
    );

    scenarios!(
        run = |value| value_from_wire::<i32>(ResourcePoolValueDomain::Integer, &v1::Value::from(value)).map_err(drop);
        "Rust consumer width" {
            i64::from(i32::MAX) => Yields(i32::MAX),
            i64::from(i32::MAX) + 1 => Fails,
        }
    );

    // f64 accepts the integer text but cannot preserve every integer above 2^53.
    assert!(matches!(
        value_from_wire::<f64>(
            ResourcePoolValueDomain::Integer,
            &v1::Value::from(9_007_199_254_740_993_i64),
        ),
        Err(ValueConversionError::ConsumerValueChanged { .. })
    ));
}

#[test]
fn address_consumers_use_the_authoritative_domain_and_rust_type() {
    scenarios!(
        run = |address| {
            let wire = value_to_wire(ResourcePoolValueDomain::IpAddress, &address).map_err(drop)?;
            value_from_wire::<IpAddr>(ResourcePoolValueDomain::IpAddress, &wire).map_err(drop)
        };
        "IpAddr accepts both address families" {
            "192.0.2.1".parse::<IpAddr>().unwrap() => Yields("192.0.2.1".parse::<IpAddr>().unwrap()),
            "2001:0db8:0:0::5".parse::<IpAddr>().unwrap() => Yields("2001:db8::5".parse::<IpAddr>().unwrap()),
        }
    );

    let ipv6: Ipv6Addr = "2001:db8::5".parse().unwrap();
    assert_eq!(
        value_from_wire::<Ipv6Addr>(ResourcePoolValueDomain::IpAddress, &ipv6.into()).unwrap(),
        ipv6
    );
    let ipv4 = v1::Value::from(Ipv4Addr::new(192, 0, 2, 1));
    assert!(matches!(
        value_from_wire::<Ipv6Addr>(ResourcePoolValueDomain::IpAddress, &ipv4),
        Err(ValueConversionError::Consumer { .. })
    ));
    assert!(matches!(
        value_from_wire::<IpAddr>(ResourcePoolValueDomain::Integer, &ipv4),
        Err(ValueConversionError::Wire(_))
    ));
}

#[test]
fn ipv6_prefixes_remain_distinct_from_addresses() {
    let prefix: Ipv6Network = "2001:db8::/64".parse().unwrap();
    let wire = value_to_wire(ResourcePoolValueDomain::Ipv6Prefix, &prefix).unwrap();
    assert_eq!(
        wire.kind,
        Some(v1::value::Kind::Ipv6Prefix(v1::Ipv6Prefix {
            network: prefix.network().octets().to_vec(),
            prefix_len: 64,
        }))
    );
    assert_eq!(
        value_from_wire::<Ipv6Network>(ResourcePoolValueDomain::Ipv6Prefix, &wire).unwrap(),
        prefix
    );
    assert!(matches!(
        value_from_wire::<Ipv6Addr>(ResourcePoolValueDomain::IpAddress, &wire),
        Err(ValueConversionError::Wire(_))
    ));
    assert!(matches!(
        value_from_wire::<Ipv6Addr>(ResourcePoolValueDomain::Ipv6Prefix, &wire),
        Err(ValueConversionError::Consumer { .. })
    ));
    assert!(matches!(
        value_from_wire::<Ipv6Network>(
            ResourcePoolValueDomain::IpAddress,
            &v1::Value::from(prefix.network()),
        ),
        Err(ValueConversionError::Consumer { .. })
    ));
    assert!(matches!(
        value_to_wire(ResourcePoolValueDomain::Ipv6Prefix, &prefix.network()),
        Err(ValueConversionError::Wire(_))
    ));
    assert!(matches!(
        value_to_wire(
            ResourcePoolValueDomain::Ipv6Prefix,
            &"2001:db8::1/64".parse::<Ipv6Network>().unwrap(),
        ),
        Err(ValueConversionError::Wire(_))
    ));
}

#[test]
fn parse_errors_preserve_sources_without_echoing_input() {
    let input = format!("private-input/{}/extra", "x".repeat(4096));
    let error = value_to_wire(ResourcePoolValueDomain::Ipv6Prefix, &input).unwrap_err();
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<IpNetworkError>()
            .is_some()
    );
    assert_eq!(error.to_string(), "invalid resource pool network");
    assert_eq!(format!("{error:?}"), "invalid resource pool network");

    let error =
        value_from_wire::<i32>(ResourcePoolValueDomain::Integer, &v1::Value::from(i64::MAX))
            .unwrap_err();
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<ParseIntError>()
            .is_some()
    );
}

fn range_definition(pool_type: ResourcePoolType, start: &str, end: &str) -> ResourcePoolDef {
    ResourcePoolDef {
        pool_type,
        prefix: None,
        ranges: vec![Range {
            start: start.to_string(),
            end: end.to_string(),
            auto_assign: true,
        }],
        delegate_prefix_len: None,
    }
}

fn wire_range(start: i64, end: i64, assignment_mode: v1::AssignmentMode) -> v1::Eligibility {
    v1::Eligibility {
        assignment_mode: assignment_mode as i32,
        entries: Some(Entries::Range(v1::ValueRange {
            start: Some(start.into()),
            end: Some(end.into()),
        })),
    }
}
