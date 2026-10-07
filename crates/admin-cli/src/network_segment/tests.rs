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

// The intent of the tests.rs file is to test the integrity of the
// command, including things like basic structure parsing, enum
// translations, and any external input validators that are
// configured. Specific "categories" are:
//
// Command Structure - Baseline debug_assert() of the entire command.
// Argument Parsing  - Ensure required/optional arg combinations parse correctly.

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use clap::{CommandFactory, Parser};
use rpc::forge;

use super::*;
use crate::test_support::{parse_leaf, raw_value};

// verify_cmd_structure runs a baseline clap debug_assert()
// to do basic command configuration checking and validation,
// ensuring things like unique argument definitions, group
// configurations, argument references, etc. Things that would
// otherwise be missed until runtime.
#[test]
fn verify_cmd_structure() {
    Cmd::command().debug_assert();
}

/////////////////////////////////////////////////////////////////////////////
// Argument Parsing
//
// This section contains tests specific to argument parsing,
// including testing required arguments, as well as optional
// flag-specific checking.

// show parses with any mix of its optional filters and routes to the Show
// variant; each row yields whether `network` is set plus the parsed
// `tenant_org_id` and `name`.
#[test]
fn parse_show_routes_to_show() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["show"])
                .map(|matches| {
                    (
                        matches
                            .get_one::<NetworkSegmentId>("network")
                            .is_some(),
                        raw_value(&matches, "tenant_org_id"),
                        raw_value(&matches, "name"),
                    )
                })
                .map_err(drop)
        };
        "no arguments (all segments)" {
            &["network-segment", "show"][..] => Yields((false, None, None)),
        }

        "with --tenant-org-id" {
            &["network-segment", "show", "--tenant-org-id", "tenant-123"][..] => Yields((false, Some("tenant-123".to_string()), None)),
        }

        "with --name" {
            &["network-segment", "show", "--name", "my-segment"][..] => Yields((false, None, Some("my-segment".to_string()))),
        }
    );
}

#[test]
fn parse_create_controls_slaac_eui64_inference() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|command| match command {
                    Cmd::Create(args) => {
                        forge::NetworkSegmentCreationRequest::from(args)
                            .infer_slaac_eui64_addresses
                    }
                    _ => unreachable!("scenario must parse as network-segment create"),
                })
                .map_err(drop)
        };
        "inference omitted" {
            &["network-segment", "create", "--name", "segment", "--prefix", "2001:db8::/64"][..] => Yields(false),
        }

        "inference enabled" {
            &[
                "network-segment",
                "create",
                "--name",
                "segment",
                "--prefix",
                "2001:db8::/64",
                "--infer-slaac-eui64-addresses",
            ][..] => Yields(true),
        }
    );
}

#[test]
fn create_request_maps_gateway_by_prefix_family() {
    let command = Cmd::try_parse_from([
        "network-segment",
        "create",
        "--name=segment",
        "--prefix=192.0.2.0/24",
        "--gateway=192.0.2.1",
        "--prefix=2001:db8::/64",
    ])
    .expect("dual-stack create command should parse");

    let Cmd::Create(args) = command else {
        unreachable!("command should parse as network-segment create");
    };
    let request = forge::NetworkSegmentCreationRequest::from(args);

    assert_eq!(
        request
            .prefixes
            .into_iter()
            .map(|prefix| (prefix.prefix, prefix.gateway))
            .collect::<Vec<_>>(),
        vec![
            ("192.0.2.0/24".to_string(), Some("192.0.2.1".to_string())),
            ("2001:db8::/64".to_string(), None),
        ]
    );
}

// Every malformed invocation is rejected at parse time.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "delete without --id" {
            &["network-segment", "delete"][..] => Fails,
        }

        "create with an IPv6 gateway" {
            &[
                "network-segment",
                "create",
                "--name",
                "segment",
                "--prefix",
                "2001:db8::/64",
                "--gateway",
                "2001:db8::1",
            ][..] => Fails,
        }

        // The creation RPC has no DHCPv6 link-address field, so keep the
        // former no-op rejected.
        "create with unsupported --dhcpv6-link-address" {
            &[
                "network-segment",
                "create",
                "--name",
                "segment",
                "--prefix",
                "2001:db8::/64",
                "--dhcpv6-link-address",
                "fe80::1",
            ][..] => Fails,
        }
    );
}

#[test]
fn parse_attach_vpc() {
    let matches = parse_leaf::<Cmd>(
        &[
            "network-segment",
            "attach-vpc",
            "--id",
            "12345678-1234-5678-90ab-cdef01234567",
            "--vpc-id",
            "abcdef01-2345-6789-abcd-ef0123456789",
        ],
        &["attach-vpc"],
    )
    .expect("should parse attach-vpc");

    assert_eq!(
        matches
            .get_one::<NetworkSegmentId>("id")
            .map(ToString::to_string)
            .as_deref(),
        Some("12345678-1234-5678-90ab-cdef01234567")
    );
    assert_eq!(
        matches
            .get_one::<VpcId>("vpc_id")
            .map(ToString::to_string)
            .as_deref(),
        Some("abcdef01-2345-6789-abcd-ef0123456789")
    );
    assert!(!matches.get_flag("force"));
}

#[test]
fn parse_attach_vpc_force() {
    let matches = parse_leaf::<Cmd>(
        &[
            "network-segment",
            "attach-vpc",
            "--id",
            "12345678-1234-5678-90ab-cdef01234567",
            "--vpc-id",
            "abcdef01-2345-6789-abcd-ef0123456789",
            "--force",
        ],
        &["attach-vpc"],
    )
    .expect("should parse attach-vpc with force");

    assert!(matches.get_flag("force"));
}

#[test]
fn parse_attach_vpc_missing_id_fails() {
    let result = Cmd::try_parse_from([
        "network-segment",
        "attach-vpc",
        "--vpc-id",
        "abcdef01-2345-6789-abcd-ef0123456789",
    ]);
    assert!(result.is_err(), "should fail without --id");
}

#[test]
fn parse_attach_vpc_missing_vpc_id_fails() {
    let result = Cmd::try_parse_from([
        "network-segment",
        "attach-vpc",
        "--id",
        "12345678-1234-5678-90ab-cdef01234567",
    ]);
    assert!(result.is_err(), "should fail without --vpc-id");
}
