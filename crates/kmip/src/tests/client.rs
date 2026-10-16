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
//! Client behaviour against the mock server: connection reuse, refusal
//! mapping, TLS failures, deadlines, the single retry, cancellation, and
//! configuration validation at construction.

use std::time::Duration;

use carbide_test_support::{Check, check_values};
use tempfile::TempDir;

use crate::test_support::{MockBehavior, MockKmipServer};
use crate::{
    CryptographicAlgorithm, KeyAttributes, KmipClient, KmipClientConfig, KmipError, Operation,
    ProtocolVersion, ResultReason, State, USAGE_MASK_DECRYPT, USAGE_MASK_ENCRYPT,
};

const KEK: &str = "kek-1";

/// A mock server holding one active AES-256 key, and a client configured to
/// reach it with the server's own client identity.
async fn server_and_client(behavior: MockBehavior) -> (MockKmipServer, TempDir, KmipClient) {
    let server = MockKmipServer::start(behavior).await;
    server.add_aes256_key(KEK);
    let dir = tempfile::tempdir().expect("temporary directory");
    let client = KmipClient::new(server.client_config_in(dir.path())).expect("client");
    (server, dir, client)
}

// Verifies the core contract: a 32-byte DEK wraps and unwraps through the
// server, and consecutive calls share one connection.
#[tokio::test]
async fn wrap_and_unwrap_round_trip_on_one_connection() {
    let (server, _dir, client) = server_and_client(MockBehavior::default()).await;
    let dek: [u8; 32] = rand::random();

    let wrapped = client.encrypt(KEK, &dek).await.expect("encrypt");
    assert_eq!(
        (
            wrapped.iv.len(),
            wrapped.ciphertext.len(),
            wrapped.tag.len()
        ),
        (12, 32, 16)
    );
    let unwrapped = client.decrypt(KEK, &wrapped).await.expect("decrypt");
    assert_eq!(unwrapped.as_slice(), &dek);
    assert_eq!(
        (server.connections(), server.requests()),
        (1, 2),
        "both calls share one connection"
    );
}

// Verifies that the IV is stored verbatim: a server that ignores the
// requested IV Length and generates 16-byte IVs (as PyKMIP does) round-trips
// as well as one that honours the requested 12.
#[tokio::test]
async fn a_sixteen_byte_server_iv_round_trips() {
    let (_server, _dir, client) = server_and_client(MockBehavior {
        iv_len: Some(16),
        ..MockBehavior::default()
    })
    .await;
    let dek = [7u8; 32];

    let wrapped = client.encrypt(KEK, &dek).await.expect("encrypt");
    assert_eq!(wrapped.iv.len(), 16);
    let unwrapped = client.decrypt(KEK, &wrapped).await.expect("decrypt");
    assert_eq!(unwrapped.as_slice(), &dek);
}

// Verifies that the read-only operations report what the server says:
// versions, operations with the vendor string, and the key's attributes.
#[tokio::test]
async fn server_facts_are_reported() {
    let (_server, _dir, client) = server_and_client(MockBehavior::default()).await;

    let versions = client.discover_versions().await.expect("versions");
    assert_eq!(
        versions,
        [(1, 4), (1, 3), (1, 2)]
            .map(|(major, minor)| ProtocolVersion { major, minor })
            .to_vec()
    );

    let query = client.query_operations().await.expect("query");
    assert!(
        query.operations.contains(&Operation::Encrypt)
            && query.operations.contains(&Operation::Decrypt),
        "{:?}",
        query.operations
    );
    assert_eq!(
        query.vendor_identification.as_deref(),
        Some("carbide-kmip mock server")
    );

    let attributes = client.get_key_attributes(KEK).await.expect("attributes");
    assert_eq!(
        attributes,
        KeyAttributes {
            state: Some(State::Active),
            algorithm: Some(CryptographicAlgorithm::Aes),
            length_bits: Some(256),
            usage_mask: Some(USAGE_MASK_ENCRYPT | USAGE_MASK_DECRYPT),
        }
    );
}

// Verifies that a refusal surfaces the server's Result Reason for the three
// cases the provider maps, and that refusals do not poison the connection.
#[tokio::test]
async fn refusals_carry_the_servers_reason_and_keep_the_connection() {
    let (server, _dir, client) = server_and_client(MockBehavior::default()).await;

    let missing = client
        .get_key_attributes("missing")
        .await
        .expect_err("unknown object");
    assert!(missing.is_item_not_found(), "{missing}");

    let mut wrapped = client.encrypt(KEK, &[1u8; 32]).await.expect("encrypt");
    wrapped.tag[0] ^= 0xFF;
    let tampered = client
        .decrypt(KEK, &wrapped)
        .await
        .expect_err("tampered tag");
    assert_eq!(
        tampered.result_reason(),
        Some(ResultReason::CryptographicFailure),
        "{tampered}"
    );

    server.update_key(KEK, |key| key.state = State::Deactivated);
    let inactive = client
        .encrypt(KEK, &[1u8; 32])
        .await
        .expect_err("inactive key");
    assert_eq!(
        inactive.result_reason(),
        Some(ResultReason::PermissionDenied),
        "{inactive}"
    );

    assert_eq!(server.connections(), 1, "refusals reuse the connection");
}

// Verifies that an Encrypt response missing the IV or the tag is rejected as
// a protocol violation rather than stored as an incomplete ciphertext.
#[tokio::test]
async fn an_encrypt_response_without_iv_or_tag_is_a_protocol_violation() {
    for behavior in [
        MockBehavior {
            omit_iv: true,
            ..MockBehavior::default()
        },
        MockBehavior {
            omit_tag: true,
            ..MockBehavior::default()
        },
    ] {
        let (_server, _dir, client) = server_and_client(behavior).await;
        let error = client
            .encrypt(KEK, &[1u8; 32])
            .await
            .expect_err("incomplete response");
        assert!(
            matches!(
                error,
                KmipError::Protocol {
                    operation: Operation::Encrypt,
                    ..
                }
            ),
            "{error}"
        );
    }
}

// Verifies mutual authentication: a client certificate from another CA never
// gets a request to the server.
#[tokio::test]
async fn an_untrusted_client_certificate_is_rejected_before_any_request() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let (cert, key) = MockKmipServer::write_untrusted_client_identity(dir.path());
    let mut config = server.client_config_in(dir.path());
    config.client_cert = cert;
    config.client_key = key;
    let client = KmipClient::new(config).expect("the material itself is valid");

    let error = client
        .discover_versions()
        .await
        .expect_err("the handshake must fail");
    assert!(
        matches!(
            error,
            KmipError::Connect { .. } | KmipError::Transport { .. }
        ),
        "{error}"
    );
    assert_eq!(server.requests(), 0, "no request reached the server");
}

// Verifies that the server certificate is checked against the configured
// server name, not just against the CA bundle.
#[tokio::test]
async fn a_server_name_mismatch_fails_the_connection() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let mut config = server.client_config_in(dir.path());
    config.server_name = Some("kms.example.invalid".to_string());
    let client = KmipClient::new(config).expect("client");

    let error = client.discover_versions().await.expect_err("name mismatch");
    assert!(matches!(error, KmipError::Connect { .. }), "{error}");
}

// Verifies the request deadline: a stalled server produces a timeout for the
// operation, and the attempt is not retried.
#[tokio::test]
async fn a_slow_server_times_out_without_a_retry() {
    let (server, dir, _client) = server_and_client(MockBehavior {
        response_delay: Some(Duration::from_secs(5)),
        ..MockBehavior::default()
    })
    .await;
    let mut config = server.client_config_in(dir.path());
    config.request_timeout = Duration::from_millis(200);
    let client = KmipClient::new(config).expect("client");

    let error = client.discover_versions().await.expect_err("timeout");
    assert!(
        matches!(
            error,
            KmipError::RequestTimeout {
                operation: Operation::DiscoverVersions,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(server.requests(), 1, "sent once, not retried");
}

// Verifies that waiting for the shared connection counts against the request
// deadline: a caller queued behind a stalled call expires when its own budget
// does, not after the first caller's budget plus its own. With the bound both
// callers finish after about one request timeout; without it the queued
// caller would take about two, which the elapsed-time assertion rejects with
// a margin of almost a full timeout on either side.
#[tokio::test]
async fn queued_callers_are_bounded_by_the_request_deadline() {
    const REQUEST_TIMEOUT: Duration = Duration::from_millis(1500);
    let server = MockKmipServer::start(MockBehavior {
        response_delay: Some(Duration::from_secs(60)),
        ..MockBehavior::default()
    })
    .await;
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut config = server.client_config_in(dir.path());
    config.request_timeout = REQUEST_TIMEOUT;
    let client = KmipClient::new(config).expect("client");

    let started = std::time::Instant::now();
    let (first, second) = tokio::join!(client.discover_versions(), client.discover_versions());
    let elapsed = started.elapsed();
    for outcome in [first, second] {
        let error = outcome.expect_err("both calls time out");
        assert!(matches!(error, KmipError::RequestTimeout { .. }), "{error}");
    }
    assert!(
        elapsed < REQUEST_TIMEOUT.mul_f32(1.6),
        "the queued caller took {elapsed:?}; the bound is one request timeout, not two"
    );
}

// Verifies the single retry: a cached connection the server has since closed
// is replaced transparently, costing one extra connection and no failure.
#[tokio::test]
async fn a_closed_cached_connection_is_retried_once() {
    let (server, _dir, client) = server_and_client(MockBehavior {
        close_after_responses: Some(1),
        ..MockBehavior::default()
    })
    .await;

    client.discover_versions().await.expect("first call");
    client
        .discover_versions()
        .await
        .expect("second call survives the stale connection");
    assert_eq!((server.connections(), server.requests()), (2, 2));
}

// Verifies cancellation safety: dropping a call mid-flight drops its
// connection instead of leaving a half-used one in the cache.
#[tokio::test]
async fn a_dropped_call_drops_its_connection() {
    let (server, dir, _client) = server_and_client(MockBehavior {
        response_delay: Some(Duration::from_secs(5)),
        ..MockBehavior::default()
    })
    .await;
    let mut config = server.client_config_in(dir.path());
    config.request_timeout = Duration::from_millis(300);
    let client = KmipClient::new(config).expect("client");

    let cancelled =
        tokio::time::timeout(Duration::from_millis(50), client.discover_versions()).await;
    assert!(cancelled.is_err(), "the caller's deadline cancels the call");
    client.discover_versions().await.ok();
    assert_eq!(
        server.connections(),
        2,
        "the next call opened a fresh connection"
    );
}

// Verifies construction-time validation: every failure names the field or
// file at fault, before any connection is attempted.
#[test]
fn new_rejects_bad_configuration_and_material_naming_the_field() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let (cert, key) = MockKmipServer::write_untrusted_client_identity(dir.path());
    let other = dir.path().join("other");
    std::fs::create_dir(&other).expect("second identity directory");
    let (other_cert, _other_key) = MockKmipServer::write_untrusted_client_identity(&other);
    let garbage = dir.path().join("garbage.pem");
    std::fs::write(&garbage, "not pem at all").expect("write garbage");
    let missing = dir.path().join("missing.pem");
    // Any parsable certificate serves as a CA bundle for parsing purposes.
    let valid = || KmipClientConfig::new("kms.example.com:5696", &cert, &cert, &key);
    let with = |update: fn(&mut KmipClientConfig, &std::path::Path)| {
        let mut config = valid();
        update(&mut config, dir.path());
        config
    };

    check_values(
        [
            Check {
                scenario: "valid material is accepted",
                input: valid(),
                expect: "ok".to_string(),
            },
            Check {
                scenario: "a zero timeout",
                input: with(|config, _| config.request_timeout = Duration::ZERO),
                expect: "kmip request_timeout: must be greater than zero and at most 120s, got 0ns"
                    .to_string(),
            },
            Check {
                scenario: "an endpoint without a host",
                input: with(|config, _| config.endpoint = ":5696".to_string()),
                expect: "kmip endpoint: host is empty".to_string(),
            },
            Check {
                scenario: "a server name that is neither a DNS name nor an IP address",
                input: with(|config, _| config.server_name = Some("not a name!".to_string())),
                expect: "kmip tls material: server name \"not a name!\" is not a valid DNS name or IP address"
                    .to_string(),
            },
            Check {
                scenario: "a missing CA bundle",
                input: with(|config, dir| config.ca_bundle = dir.join("missing.pem")),
                expect: format!(
                    "kmip tls material: failed to read ca_bundle file {}",
                    missing.display()
                ),
            },
            Check {
                scenario: "a key file holding no key",
                input: with(|config, dir| config.client_key = dir.join("garbage.pem")),
                expect: format!(
                    "kmip tls material: client_key file {} contains no private key",
                    garbage.display()
                ),
            },
            Check {
                scenario: "a certificate that does not match the key",
                input: with(|config, dir| config.client_cert = dir.join("other/untrusted-client.pem")),
                expect: format!(
                    "kmip tls material: client_cert file {} was rejected",
                    other_cert.display()
                ),
            },
        ],
        |config| match KmipClient::new(config) {
            Ok(_) => "ok".to_string(),
            Err(error) => error.to_string(),
        },
    );
}
