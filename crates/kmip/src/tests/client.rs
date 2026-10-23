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
//! Client behavior against the mock server: connection reuse and recycling,
//! refusal mapping, the checks that tie a response to its request, TLS
//! failures, deadlines, the single retry, cancellation, and configuration
//! validation at construction.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use carbide_test_support::{Check, check_values};
use tempfile::TempDir;

use crate::test_support::{
    EncryptFault, MockBehavior, MockKmipServer, ResponseFault, write_test_pki,
};
use crate::{
    CryptographicAlgorithm, DEFAULT_CONNECT_TIMEOUT, DEFAULT_REQUEST_TIMEOUT, KeyAttributes,
    KmipClient, KmipClientConfig, KmipError, MAX_CONNECT_TIMEOUT, MAX_RESPONSE_BYTES, Operation,
    ProtocolVersion, ResultReason, State, TimeoutPhase, USAGE_MASK_DECRYPT, USAGE_MASK_ENCRYPT,
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

/// A server that holds the responses in `delayed` (counted from zero) for
/// five seconds, far beyond the deadlines the tests configure, and answers
/// every other request promptly.
fn stalling(delayed: Range<usize>) -> MockBehavior {
    MockBehavior {
        response_delay: Some(Duration::from_secs(5)),
        delayed_responses: Some(delayed),
        ..MockBehavior::default()
    }
}

/// Polls `condition` until it holds, failing after `deadline`.
async fn wait_until(deadline: Duration, condition: impl Fn() -> bool) {
    let started = Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < deadline,
            "condition not met within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn protocol_detail(error: &KmipError) -> &str {
    match error {
        KmipError::Protocol { detail, .. } => detail,
        other => panic!("expected a protocol violation, got {other}"),
    }
}

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

// The IV is stored verbatim: a server that ignores the requested IV Length
// and generates 16-byte IVs, as PyKMIP 0.11.0 does for AES, round-trips as
// well as one that honors the requested 12. The mock's Decrypt checks that
// the IV Length sent back describes the 16-byte IV.
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

// The server's facts are reported as sent, so the lists are set to values
// that differ from the mock's defaults and compared exactly.
#[tokio::test]
async fn server_facts_are_reported() {
    let advertised_operations = vec![
        Operation::Encrypt,
        Operation::Decrypt,
        Operation::Query,
        Operation::Locate,
    ];
    let (_server, _dir, client) = server_and_client(MockBehavior {
        advertised_versions: vec![(2, 0), (1, 4)],
        advertised_operations: advertised_operations.clone(),
        ..MockBehavior::default()
    })
    .await;

    let versions = client.discover_versions().await.expect("versions");
    assert_eq!(
        versions,
        [(2, 0), (1, 4)]
            .map(|(major, minor)| ProtocolVersion { major, minor })
            .to_vec()
    );

    let query = client.query_operations().await.expect("query");
    assert_eq!(query.operations, advertised_operations);
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

// A refusal surfaces the server's Result Reason for the three refusals a
// caller tells apart (missing object, failed authentication, unusable key),
// and refusals do not poison the connection.
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

// An Encrypt response is only usable with an IV, a 16-byte tag and
// ciphertext as long as the plaintext; each row corrupts one of those and
// expects the check that names it, and the connection that carried the
// unusable response is not reused.
#[tokio::test]
async fn an_incomplete_or_inconsistent_encrypt_response_is_a_protocol_violation() {
    let cases = [
        (EncryptFault::OmitIv, "encrypt response carries no IV"),
        (EncryptFault::EmptyIv, "encrypt response carries no IV"),
        (
            EncryptFault::OmitTag,
            "encrypt response carries no authentication tag",
        ),
        (
            EncryptFault::EmptyTag,
            "encrypt response carries a 0-byte authentication tag, expected 16",
        ),
        (
            EncryptFault::ShortTag,
            "encrypt response carries a 8-byte authentication tag, expected 16",
        ),
        (
            EncryptFault::LongCiphertext,
            "encrypt response carries 33 ciphertext bytes for 32 plaintext bytes",
        ),
    ];
    for (fault, expected) in cases {
        let (server, _dir, client) = server_and_client(MockBehavior {
            encrypt_fault: Some(fault),
            ..MockBehavior::default()
        })
        .await;
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
            "{fault:?}: {error}"
        );
        assert_eq!(protocol_detail(&error), expected, "{fault:?}");
        client.discover_versions().await.expect("the next call");
        assert_eq!(
            server.connections(),
            2,
            "{fault:?}: the next call opened a fresh connection"
        );
    }
}

// A response is checked against the request it answers; after a mismatch
// the connection can no longer be trusted, so the next call opens a new one.
#[tokio::test]
async fn a_response_that_does_not_answer_the_request_is_rejected_and_its_connection_dropped() {
    let cases = [
        (
            ResponseFault::WrongUniqueIdentifier,
            "response names object \"kek-1-other\", expected \"kek-1\"",
        ),
        (
            ResponseFault::OmitBatchItemId,
            "response carries no Unique Batch Item ID",
        ),
        (
            ResponseFault::WrongProtocolVersion,
            "response declares protocol version 1.2, expected 1.4",
        ),
    ];
    for (fault, expected) in cases {
        let (server, _dir, client) = server_and_client(MockBehavior {
            response_fault: Some(fault),
            ..MockBehavior::default()
        })
        .await;
        let error = client
            .encrypt(KEK, &[1u8; 32])
            .await
            .expect_err("mismatched response");
        assert_eq!(protocol_detail(&error), expected, "{fault:?}");
        client.discover_versions().await.ok();
        assert_eq!(
            server.connections(),
            2,
            "{fault:?}: the next call opened a fresh connection"
        );
    }
}

// Mutual authentication: a client certificate from another CA never gets a
// request processed, and the failure of that fresh connection is not retried.
#[tokio::test]
async fn an_untrusted_client_certificate_is_rejected_before_any_request() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let foreign_dir = tempfile::tempdir().expect("temporary directory");
    let foreign = write_test_pki(foreign_dir.path());
    let mut config = server.client_config_in(dir.path());
    config.client_cert = foreign.client_cert;
    config.client_key = foreign.client_key;
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
    assert_eq!(server.connections(), 1, "a fresh connection is not retried");
}

// The server certificate must chain to the configured bundle and carry the
// configured name; an explicit name the certificate does carry is accepted.
#[tokio::test]
async fn the_server_certificate_is_checked_against_the_bundle_and_the_server_name() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let foreign_dir = tempfile::tempdir().expect("temporary directory");
    let foreign = write_test_pki(foreign_dir.path());
    type Update = fn(&mut KmipClientConfig, &Path);
    let cases: [(&str, Update, &str); 3] = [
        (
            "a server name the certificate carries",
            |config, _| config.server_name = Some("localhost".to_string()),
            "ok",
        ),
        (
            "a server name the certificate lacks",
            |config, _| config.server_name = Some("kms.example.invalid".to_string()),
            "connect failed",
        ),
        (
            "a CA bundle that did not sign the server certificate",
            |config, foreign_ca| config.ca_bundle = foreign_ca.to_path_buf(),
            "connect failed",
        ),
    ];
    for (scenario, update, expected) in cases {
        let mut config = server.client_config_in(dir.path());
        update(&mut config, &foreign.ca);
        let client = KmipClient::new(config).expect("client");
        let outcome = match client.discover_versions().await {
            Ok(_) => "ok".to_string(),
            Err(KmipError::Connect { .. }) => "connect failed".to_string(),
            Err(other) => format!("unexpected {other}"),
        };
        assert_eq!(outcome, expected, "{scenario}");
    }
}

// The connect deadline bounds a peer that accepts TCP but never completes
// TLS, while a refused connection fails at once.
#[tokio::test]
async fn connecting_is_bounded_by_the_connect_deadline() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let pki = write_test_pki(dir.path());
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("silent listener");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let closed_addr = closed.local_addr().expect("address");
    drop(closed);
    let cases = [
        (
            "a listener that never completes TLS",
            silent.local_addr().expect("address"),
            "connect timed out",
        ),
        ("a closed port", closed_addr, "connect failed"),
    ];
    for (scenario, addr, expected) in cases {
        let client = KmipClient::new(KmipClientConfig {
            endpoint: addr.to_string(),
            server_name: None,
            ca_bundle: pki.ca.clone(),
            client_cert: pki.client_cert.clone(),
            client_key: pki.client_key.clone(),
            connect_timeout: Duration::from_millis(200),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
        .expect("client");
        let outcome = match client.discover_versions().await {
            Err(KmipError::ConnectTimeout { .. }) => "connect timed out".to_string(),
            Err(KmipError::Connect { .. }) => "connect failed".to_string(),
            Ok(_) => "ok".to_string(),
            Err(other) => format!("unexpected {other}"),
        };
        assert_eq!(outcome, expected, "{scenario}");
    }
}

// The request deadline on a reused connection: the stalled call reports that
// its request was in flight and is not retried, its connection is discarded,
// and the next call succeeds on a new one.
#[tokio::test]
async fn a_timed_out_call_is_not_retried_and_its_connection_is_replaced() {
    let (server, dir, _client) = server_and_client(stalling(1..2)).await;
    let mut config = server.client_config_in(dir.path());
    config.request_timeout = Duration::from_millis(300);
    let client = KmipClient::new(config).expect("client");

    client.discover_versions().await.expect("first call");
    let error = client.discover_versions().await.expect_err("timeout");
    assert!(
        matches!(
            error,
            KmipError::RequestTimeout {
                operation: Operation::DiscoverVersions,
                phase: TimeoutPhase::InFlight,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(server.requests(), 2, "the stalled request was not retried");
    client.query_operations().await.expect("third call");
    assert_eq!(
        (server.connections(), server.requests()),
        (2, 3),
        "the stalled connection was replaced"
    );
}

// Waiting for the shared connection counts against the request deadline: a
// caller queued behind a stalled call expires when its own budget does, not
// after the first caller's budget plus its own, and reports that it sent
// nothing. With the bound both callers finish after about one request
// timeout; without it the queued caller would take about two, which the
// elapsed-time assertion rejects.
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

    let started = Instant::now();
    let (first, second) = tokio::join!(client.discover_versions(), client.discover_versions());
    let elapsed = started.elapsed();
    let phase = |outcome: Result<Vec<ProtocolVersion>, KmipError>| match outcome {
        Err(KmipError::RequestTimeout { phase, .. }) => phase,
        other => panic!("expected a request timeout, got {other:?}"),
    };
    assert_eq!(phase(first), TimeoutPhase::InFlight);
    assert_eq!(phase(second), TimeoutPhase::Queued);
    assert!(
        elapsed < REQUEST_TIMEOUT.mul_f32(1.6),
        "the queued caller took {elapsed:?}; the bound is one request timeout, not two"
    );
    assert_eq!(
        (server.connections(), server.requests()),
        (1, 1),
        "the queued caller neither connected nor sent"
    );
}

#[tokio::test]
async fn concurrent_callers_share_one_connection() {
    let (server, _dir, client) = server_and_client(MockBehavior::default()).await;

    let (versions, query) = tokio::join!(client.discover_versions(), client.query_operations());
    versions.expect("first caller");
    query.expect("queued caller");
    assert_eq!((server.connections(), server.requests()), (1, 2));
}

// A cached connection the server has since closed is replaced transparently,
// costing one extra connection and no failure.
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

// The retry's limits: a server that stops answering gets exactly one
// reconnect before the failure is reported, and a response cut short after
// its first bytes is not retried at all.
#[tokio::test]
async fn the_retry_happens_at_most_once_and_only_before_a_response_byte() {
    let cases = [
        (
            "a server that stops answering",
            MockBehavior {
                answer_at_most: Some(1),
                ..MockBehavior::default()
            },
            (2, 3),
        ),
        (
            "a response cut short",
            MockBehavior {
                partial_responses_after: Some(1),
                ..MockBehavior::default()
            },
            (1, 2),
        ),
    ];
    for (scenario, behavior, expected_counts) in cases {
        let (server, _dir, client) = server_and_client(behavior).await;
        client.discover_versions().await.expect("first call");
        let error = client
            .discover_versions()
            .await
            .expect_err("the second call fails");
        assert!(
            matches!(error, KmipError::Transport { .. }),
            "{scenario}: {error}"
        );
        assert_eq!(
            (server.connections(), server.requests()),
            expected_counts,
            "{scenario}"
        );
    }
}

// Cancellation safety: a call dropped while its request is in flight takes
// its connection with it, and the next call reconnects and succeeds.
#[tokio::test]
async fn a_dropped_call_drops_its_connection() {
    let (server, _dir, client) = server_and_client(stalling(0..1)).await;
    let client = Arc::new(client);

    let in_flight = tokio::spawn({
        let client = client.clone();
        async move { client.discover_versions().await }
    });
    wait_until(Duration::from_secs(5), || server.requests() == 1).await;
    in_flight.abort();
    assert!(
        in_flight.await.expect_err("aborted").is_cancelled(),
        "the call was cancelled mid-flight"
    );

    client.query_operations().await.expect("the next call");
    assert_eq!(
        (server.connections(), server.requests()),
        (2, 2),
        "the next call opened a fresh connection"
    );
}

// A connection idle longer than the bound is closed before reuse, so a
// silently dropped connection is not trusted and rotated TLS material is
// picked up once traffic pauses.
#[tokio::test]
async fn an_idle_connection_is_replaced_before_reuse() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let client = KmipClient::new(server.client_config_in(dir.path()))
        .expect("client")
        .with_max_idle(Duration::from_millis(50));

    client.discover_versions().await.expect("first call");
    tokio::time::sleep(Duration::from_millis(120)).await;
    client.discover_versions().await.expect("second call");
    assert_eq!((server.connections(), server.requests()), (2, 2));
}

// The TLS files are read again at each connection: material the server
// rejects is replaced on disk, and the same client is accepted next time.
#[tokio::test]
async fn rotated_client_material_is_used_at_the_next_connection() {
    let (server, dir, _client) = server_and_client(MockBehavior::default()).await;
    let config = server.client_config_in(dir.path());
    let foreign_dir = tempfile::tempdir().expect("temporary directory");
    let foreign = write_test_pki(foreign_dir.path());
    std::fs::copy(&foreign.client_cert, &config.client_cert).expect("install foreign cert");
    std::fs::copy(&foreign.client_key, &config.client_key).expect("install foreign key");
    let client = KmipClient::new(config.clone()).expect("foreign material is valid PEM");

    let error = client
        .discover_versions()
        .await
        .expect_err("the server rejects the foreign identity");
    assert!(
        matches!(
            error,
            KmipError::Connect { .. } | KmipError::Transport { .. }
        ),
        "{error}"
    );

    let trusted_dir = tempfile::tempdir().expect("temporary directory");
    let trusted = server.client_config_in(trusted_dir.path());
    std::fs::copy(&trusted.client_cert, &config.client_cert).expect("restore trusted cert");
    std::fs::copy(&trusted.client_key, &config.client_key).expect("restore trusted key");
    client
        .discover_versions()
        .await
        .expect("the rotated material is read at the reconnect");
    assert_eq!(server.connections(), 2);
}

#[tokio::test]
async fn an_oversized_response_is_rejected_from_its_header() {
    let (_server, _dir, client) = server_and_client(MockBehavior {
        oversized_response: true,
        ..MockBehavior::default()
    })
    .await;

    let error = client.discover_versions().await.expect_err("too large");
    assert!(
        matches!(
            error,
            KmipError::ResponseTooLarge { length, max }
                if length == MAX_RESPONSE_BYTES + 8 && max == MAX_RESPONSE_BYTES
        ),
        "{error}"
    );
}

// Construction-time validation: every failure names the field or file at
// fault, before any connection is attempted.
#[test]
fn new_rejects_bad_configuration_and_material_naming_the_field() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let pki = write_test_pki(dir.path());
    let other_dir = tempfile::tempdir().expect("temporary directory");
    let other = write_test_pki(other_dir.path());
    let garbage = dir.path().join("garbage.pem");
    std::fs::write(&garbage, "not pem at all").expect("write garbage");
    let malformed = dir.path().join("malformed.pem");
    std::fs::write(
        &malformed,
        "-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----\n",
    )
    .expect("write malformed PEM");
    let missing = dir.path().join("missing.pem");
    let valid = || KmipClientConfig {
        endpoint: "kms.example.com:5696".to_string(),
        server_name: None,
        ca_bundle: pki.ca.clone(),
        client_cert: pki.client_cert.clone(),
        client_key: pki.client_key.clone(),
        connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        request_timeout: DEFAULT_REQUEST_TIMEOUT,
    };
    let with = |update: fn(&mut KmipClientConfig, &Path)| {
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
                scenario: "a connect timeout above its maximum",
                input: with(|config, _| {
                    config.connect_timeout = MAX_CONNECT_TIMEOUT + Duration::from_millis(1);
                }),
                expect: "kmip connect_timeout: must be greater than zero and at most 60s, got \
                         60.001s"
                    .to_string(),
            },
            Check {
                scenario: "an endpoint without a host",
                input: with(|config, _| config.endpoint = ":5696".to_string()),
                expect: "kmip endpoint: host is empty".to_string(),
            },
            Check {
                scenario: "an endpoint host that is not a name, despite a valid server name",
                input: with(|config, _| {
                    config.endpoint = "not a host:5696".to_string();
                    config.server_name = Some("kms.example.com".to_string());
                }),
                expect: "kmip endpoint: \"not a host\" is not a valid DNS name or IP address"
                    .to_string(),
            },
            Check {
                scenario: "a server name that is neither a DNS name nor an IP address",
                input: with(|config, _| config.server_name = Some("not a name!".to_string())),
                expect: "kmip server_name: \"not a name!\" is not a valid DNS name or IP address"
                    .to_string(),
            },
            Check {
                scenario: "a missing CA bundle",
                input: with(|config, dir| config.ca_bundle = dir.join("missing.pem")),
                expect: format!("failed to read ca_bundle file {}", missing.display()),
            },
            Check {
                scenario: "a CA bundle that is not valid PEM",
                input: with(|config, dir| config.ca_bundle = dir.join("malformed.pem")),
                expect: format!("ca_bundle file {} is not valid PEM", malformed.display()),
            },
            Check {
                scenario: "a key file holding no key",
                input: with(|config, dir| config.client_key = dir.join("garbage.pem")),
                expect: format!(
                    "client_key file {} contains no private key",
                    garbage.display()
                ),
            },
            Check {
                scenario: "a certificate that does not match the key",
                input: {
                    let mut config = valid();
                    config.client_cert = other.client_cert.clone();
                    config
                },
                expect: format!(
                    "client_cert file {} was rejected",
                    other.client_cert.display()
                ),
            },
        ],
        |config| match KmipClient::new(config) {
            Ok(_) => "ok".to_string(),
            Err(error) => error.to_string(),
        },
    );
}
