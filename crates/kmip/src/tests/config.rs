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
//! Configuration validation: endpoint parsing and the timeout bounds.

use std::time::Duration;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;

use crate::config::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, KmipClientConfig, MAX_CONNECT_TIMEOUT,
    MAX_REQUEST_TIMEOUT, parse_endpoint, validate_timeouts,
};

// Verifies the endpoint grammar: an optional port after a host or bracketed
// IPv6 literal, the registered port by default, and rejection of forms that
// would otherwise connect somewhere unintended.
#[test]
fn endpoints_parse_with_a_default_port_and_ipv6_literals() {
    scenarios!(run = |endpoint: &str| parse_endpoint(endpoint).map_err(|error| error.to_string());
        "host and port" {
            "kms.example.com:5696" => Yields(("kms.example.com".to_string(), 5696)),
            "10.0.0.1:1234" => Yields(("10.0.0.1".to_string(), 1234)),
            "[2001:db8::1]:1234" => Yields(("2001:db8::1".to_string(), 1234)),
        }
        "the registered port is the default" {
            "kms.example.com" => Yields(("kms.example.com".to_string(), 5696)),
            "[2001:db8::1]" => Yields(("2001:db8::1".to_string(), 5696)),
            "2001:db8::1" => Yields(("2001:db8::1".to_string(), 5696)),
        }
        "malformed endpoints are rejected naming the field" {
            "" => FailsWith("kmip endpoint: host is empty".to_string()),
            ":5696" => FailsWith("kmip endpoint: host is empty".to_string()),
            "kms.example.com:0" => FailsWith("kmip endpoint: invalid port \"0\"".to_string()),
            "kms.example.com:kmip" => FailsWith("kmip endpoint: invalid port \"kmip\"".to_string()),
            "[2001:db8::1" => FailsWith("kmip endpoint: unterminated IPv6 literal".to_string()),
            "[2001:db8::1]x" => FailsWith(
                "kmip endpoint: unexpected \"x\" after the IPv6 literal".to_string()
            ),
        }
    );
}

// Verifies that a timeout must be positive and within its hard maximum, and
// that the failure names the offending field.
#[test]
fn timeouts_must_be_positive_and_within_the_hard_maxima() {
    let config = |connect: Duration, request: Duration| {
        let mut config =
            KmipClientConfig::new("kms.example.com", "ca.pem", "client.pem", "client.key");
        config.connect_timeout = connect;
        config.request_timeout = request;
        config
    };
    scenarios!(
        run = |(connect, request)| validate_timeouts(&config(connect, request))
            .map_err(|error| error.to_string());
        "defaults and the maxima are accepted" {
            (DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT) => Yields(()),
            (MAX_CONNECT_TIMEOUT, MAX_REQUEST_TIMEOUT) => Yields(()),
        }
        "zero and above-maximum values are rejected naming the field" {
            (Duration::ZERO, DEFAULT_REQUEST_TIMEOUT) => FailsWith(
                "kmip connect_timeout: must be greater than zero and at most 60s, got 0ns"
                    .to_string()
            ),
            (DEFAULT_CONNECT_TIMEOUT, MAX_REQUEST_TIMEOUT + Duration::from_millis(1)) => FailsWith(
                "kmip request_timeout: must be greater than zero and at most 120s, got 120.001s"
                    .to_string()
            ),
        }
    );
}
