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

use std::path::PathBuf;
use std::time::Duration;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;

use crate::config::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, KmipClientConfig, MAX_CONNECT_TIMEOUT,
    MAX_REQUEST_TIMEOUT, parse_endpoint, validate_timeouts,
};

// The endpoint grammar: an optional port after a DNS name, an IPv4 address
// or a bracketed IPv6 literal, the registered port by default, and rejection
// of every form that could connect somewhere other than intended.
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
        }
        "malformed endpoints are rejected naming the field" {
            "" => FailsWith("kmip endpoint: host is empty".to_string()),
            ":5696" => FailsWith("kmip endpoint: host is empty".to_string()),
            "not a host:5696" => FailsWith(
                "kmip endpoint: \"not a host\" is not a valid DNS name or IP address".to_string()
            ),
            "kms.example.com:0" => FailsWith("kmip endpoint: invalid port \"0\"".to_string()),
            "kms.example.com:kmip" => FailsWith("kmip endpoint: invalid port \"kmip\"".to_string()),
            "[2001:db8::1" => FailsWith("kmip endpoint: unterminated IPv6 literal".to_string()),
            "[2001:db8::1]x" => FailsWith(
                "kmip endpoint: unexpected \"x\" after the IPv6 literal".to_string()
            ),
            "[kms.example.com]:5696" => FailsWith(
                "kmip endpoint: \"kms.example.com\" in brackets is not an IPv6 literal".to_string()
            ),
        }
        "an unbracketed IPv6 literal is ambiguous and rejected" {
            // Read as host and port, this would be a different address.
            "2001:db8::1:5696" => FailsWith(
                "kmip endpoint: host contains more than one colon; an IPv6 literal must be \
                 bracketed, as in [2001:db8::1]:5696".to_string()
            ),
            "2001:db8::1" => FailsWith(
                "kmip endpoint: host contains more than one colon; an IPv6 literal must be \
                 bracketed, as in [2001:db8::1]:5696".to_string()
            ),
        }
    );
}

// A timeout must be positive and within its own hard maximum, and the
// failure names the offending field; the connect and request maxima differ.
#[test]
fn timeouts_must_be_positive_and_within_the_hard_maxima() {
    let config = |connect: Duration, request: Duration| KmipClientConfig {
        endpoint: "kms.example.com".to_string(),
        server_name: None,
        ca_bundle: PathBuf::from("ca.pem"),
        client_cert: PathBuf::from("client.pem"),
        client_key: PathBuf::from("client.key"),
        connect_timeout: connect,
        request_timeout: request,
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
            (MAX_CONNECT_TIMEOUT + Duration::from_millis(1), DEFAULT_REQUEST_TIMEOUT) => FailsWith(
                "kmip connect_timeout: must be greater than zero and at most 60s, got 60.001s"
                    .to_string()
            ),
            (DEFAULT_CONNECT_TIMEOUT, MAX_REQUEST_TIMEOUT + Duration::from_millis(1)) => FailsWith(
                "kmip request_timeout: must be greater than zero and at most 120s, got 120.001s"
                    .to_string()
            ),
        }
    );
}
