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
use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use clap::{CommandFactory, Parser};
use ipnet::IpNet;
use rpc::forge::DeletedFilter;

use super::common::VpcPrefixSelector;
use super::*;
use crate::test_support::{parse_leaf, raw_value};

const TEST_VPC_ID: &str = "00000000-0000-0000-0000-000000000001";
const TEST_VPC_PREFIX_ID: &str = "00000000-0000-0000-0000-000000000002";
const TEST_SITE_PREFIX_ID: &str = "00000000-0000-0000-0000-000000000003";

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

// parse_show routes every valid `show` invocation to the Show variant and
// reports which selectors/filters landed: a tuple of (prefix_selector?,
// vpc_id?, contains?, contained_by?, deleted-is-only) so each original
// presence/enum assertion survives as a row.
#[test]
fn parse_show_routes_to_show_variant() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["show"])
                .map(|matches| {
                    (
                        matches
                            .get_one::<VpcPrefixSelector>("VpcPrefixSelector")
                            .is_some(),
                        matches.get_one::<VpcId>("vpc-id").is_some(),
                        matches.get_one::<IpNet>("contains").is_some(),
                        matches.get_one::<IpNet>("contained-by").is_some(),
                        matches!(
                            matches.get_one::<DeletedFilter>("deleted"),
                            Some(DeletedFilter::Only)
                        ),
                    )
                })
                .map_err(drop)
        };
        "show with no arguments" {
            &["vpc-prefix", "show"][..] => Yields((false, false, false, false, false)),
        }

        "show with --deleted only" {
            &["vpc-prefix", "show", "--deleted", "only"][..] => Yields((false, false, false, false, true)),
        }

        "show with a prefix-selector id" {
            &["vpc-prefix", "show", TEST_VPC_PREFIX_ID][..] => Yields((true, false, false, false, false)),
        }

        "show with a prefix-selector cidr" {
            &["vpc-prefix", "show", "10.0.0.0/8"][..] => Yields((true, false, false, false, false)),
        }

        "show with --vpc-id" {
            &["vpc-prefix", "show", "--vpc-id", TEST_VPC_ID][..] => Yields((false, true, false, false, false)),
        }

        "show with --contains" {
            &["vpc-prefix", "show", "--contains", "10.0.0.0/24"][..] => Yields((false, false, true, false, false)),
        }

        "show with --contained-by" {
            &["vpc-prefix", "show", "--contained-by", "10.0.0.0/8"][..] => Yields((false, false, false, true, false)),
        }
    );
}

#[test]
fn contains_search_accepts_addresses_and_prefixes() {
    scenarios!(
        run = |value| {
            parse_leaf::<Cmd>(&["vpc-prefix", "show", "--contains", value], &["show"])
                .map(|matches| matches.get_one::<IpNet>("contains").map(ToString::to_string))
                .map_err(|error| error.kind())
        };
        "IPv4 address" {
            "192.0.2.5" => Yields(Some("192.0.2.5/32".to_string())),
        }
        "IPv6 address" {
            "2001:db8::1234" => Yields(Some("2001:db8::1234/128".to_string())),
        }
        "CIDR prefix passes through unchanged" {
            "2001:db8::/64" => Yields(Some("2001:db8::/64".to_string())),
        }
        "invalid prefix length" {
            "2001:db8::/129" => FailsWith(clap::error::ErrorKind::ValueValidation),
        }
    );
}

// parse_create routes every valid `create` invocation to the Create variant,
// reporting (vpc_id, prefix, name, vpc_prefix_id?, site_prefix_id?) so the
// required-field and optional-id assertions each become a row.
#[test]
fn parse_create_routes_to_create_variant() {
    scenarios!(
        run = |argv| {
            parse_leaf::<Cmd>(argv, &["create"])
                .map(|matches| {
                    (
                        matches
                            .get_one::<VpcId>("vpc-id")
                            .copied()
                            .expect("VPC ID is required"),
                        *matches
                            .get_one::<IpNet>("prefix")
                            .expect("prefix is required"),
                        raw_value(&matches, "name").expect("name is required"),
                        matches
                            .get_one::<VpcPrefixId>("vpc-prefix-id")
                            .is_some(),
                        matches
                            .get_one::<SitePrefixId>("site-prefix-id")
                            .copied(),
                    )
                })
                .map_err(drop)
        };
        "create with required args" {
            &[
                "vpc-prefix",
                "create",
                "--vpc-id",
                TEST_VPC_ID,
                "--prefix",
                "10.0.0.0/8",
                "--name",
                "test-prefix",
            ][..] => Yields((
                TEST_VPC_ID.parse::<VpcId>().unwrap(),
                "10.0.0.0/8".parse::<IpNet>().unwrap(),
                "test-prefix".to_string(),
                false,
                None,
            )),
        }

        "create with optional --vpc-prefix-id" {
            &[
                "vpc-prefix",
                "create",
                "--vpc-id",
                TEST_VPC_ID,
                "--prefix",
                "10.0.0.0/8",
                "--name",
                "test-prefix",
                "--vpc-prefix-id",
                TEST_VPC_PREFIX_ID,
            ][..] => Yields((
                TEST_VPC_ID.parse::<VpcId>().unwrap(),
                "10.0.0.0/8".parse::<IpNet>().unwrap(),
                "test-prefix".to_string(),
                true,
                None,
            )),
        }

        "create with optional --site-prefix-id" {
            &[
                "vpc-prefix",
                "create",
                "--vpc-id",
                TEST_VPC_ID,
                "--site-prefix-id",
                TEST_SITE_PREFIX_ID,
                "--prefix",
                "10.0.0.0/8",
                "--name",
                "test-prefix",
            ][..] => Yields((
                TEST_VPC_ID.parse::<VpcId>().unwrap(),
                "10.0.0.0/8".parse::<IpNet>().unwrap(),
                "test-prefix".to_string(),
                false,
                Some(TEST_SITE_PREFIX_ID.parse::<SitePrefixId>().unwrap()),
            )),
        }
    );
}

#[test]
fn create_request_preserves_site_prefix_id() {
    let Cmd::Create(args) = Cmd::try_parse_from([
        "vpc-prefix",
        "create",
        "--vpc-id",
        TEST_VPC_ID,
        "--site-prefix-id",
        TEST_SITE_PREFIX_ID,
        "--prefix",
        "10.0.0.0/8",
        "--name",
        "test-prefix",
    ])
    .expect("create arguments should parse") else {
        panic!("create arguments should route to the create command");
    };

    let request: rpc::forge::VpcPrefixCreationRequest = args.into();

    assert_eq!(
        request.site_prefix_id,
        Some(TEST_SITE_PREFIX_ID.parse().unwrap())
    );
}

// parse_delete routes a valid `delete` invocation to the Delete variant,
// reporting the parsed vpc_prefix_id.
#[test]
fn parse_delete_routes_to_delete_variant() {
    scenarios!(
        run = |argv| parse_leaf::<Cmd>(argv, &["delete"])
            .map(|matches| {
                matches
                    .get_one::<VpcPrefixId>("vpc_prefix_id")
                    .copied()
                    .expect("VPC prefix ID is required")
            })
            .map_err(drop);
        "delete with a vpc-prefix-id" {
            &["vpc-prefix", "delete", TEST_VPC_PREFIX_ID][..] => Yields(TEST_VPC_PREFIX_ID.parse::<VpcPrefixId>().unwrap()),
        }
    );
}

// Every malformed invocation is rejected at parse time -- mutually exclusive
// filters, a missing required argument, or a subcommand left without its
// positional id.
#[test]
fn invalid_invocations_are_rejected() {
    scenarios!(
        run = |argv| {
            Cmd::try_parse_from(argv.iter().copied())
                .map(|_| ())
                .map_err(drop)
        };
        "show with both --contains and --contained-by" {
            &[
                "vpc-prefix",
                "show",
                "--contains",
                "10.0.0.0/24",
                "--contained-by",
                "10.0.0.0/8",
            ][..] => Fails,
        }

        "create without --vpc-id" {
            &[
                "vpc-prefix",
                "create",
                "--prefix",
                "10.0.0.0/8",
                "--name",
                "test",
            ][..] => Fails,
        }

        "delete without a vpc-prefix-id" {
            &["vpc-prefix", "delete"][..] => Fails,
        }
    );
}
