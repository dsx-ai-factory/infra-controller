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

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use carbide_test_support::{Case, Outcome, check_cases_async, scenarios};
use common::fixtures::*;
use nras::{EarStatus, NrasError, VerifierClient, VerifierResponseJwt};
use serde_json::json;

#[tokio::test]
async fn verify_preserves_device_records_and_returns_a_signed_negative_ear() {
    let mut server = mockito::Server::new_async().await;
    let fixture = SignedEarFixture::new();
    let mut claims = ear_claims();
    claims["ear_status"] = json!("contraindicated");
    claims["submods"]["gpu_1"] = claims["submods"]["gpu_0"].clone();
    claims["submods"]["gpu_1"]["ear_status"] = json!("contraindicated");
    claims["submods"]["gpu_1"]["eat_nonce"] = json!(URL_SAFE_NO_PAD.encode([0xfd; 96]));
    claims["submods"]["gpu_1"]["ear_nvidia_error_details"] = json!({"reason": "test appraisal"});
    let token = fixture.sign(&claims, Some(KEY_ID));

    // Match the complete wire request, including both labels, encoding alphabets,
    // chain ordering and the smallest/largest permitted nonces.
    let record = |nonce: &[u8]| {
        json!({
            "__cmwc_t": "tag:nvidia.com,2026:evidence-item/v1",
            "evidence": ["application/vnd.nvidia.spdm-measurement-transcript", "-_8"],
            "certificate": ["application/vnd.nvidia.pem-cert-chain", URL_SAFE_NO_PAD.encode(CERTIFICATE_CHAIN)],
            "nonce": ["application/octet-stream", URL_SAFE_NO_PAD.encode(nonce)],
        })
    };
    let verify = server
        .mock("POST", "/v1/verify")
        .match_header("content-type", "application/cmw+json")
        .match_header("accept", "application/eat+jwt")
        .match_body(mockito::Matcher::Json(json!({
            "__cmwc_t": "tag:nvidia.com,2026:verifier-input/profiles/base/pre-1.0.0",
            "gpu_0": record(&[0xfb; 32]),
            "gpu_1": record(&[0xfd; 96]),
        })))
        .with_status(200)
        .with_header("content-type", "application/eat+jwt")
        .with_body(&token)
        .create_async()
        .await;
    let jwks = server
        .mock("GET", "/.well-known/jwks.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&fixture.jwks)
        .create_async()
        .await;
    let config = nras::Config {
        // A trailing slash must not introduce a second slash before v1/verify.
        nras_url: format!("{}/", server.url()),
        nras_jwks_url: format!("{}/.well-known/jwks.json", server.url()),
        ..Default::default()
    };
    let instrumentation = carbide_instrument::Instrumentation::noop();
    let client = nras::NrasVerifierClient::new_with_config(&config, instrumentation.clone());
    let mut second_device = device("gpu_1");
    second_device.nonce = "fd".repeat(96);
    let response = client
        .attest(vec![device("gpu_0"), second_device])
        .await
        .unwrap();
    assert_eq!(response.as_ref(), token);
    let keystore = nras::NrasKeyStore::new_with_config(&config, &instrumentation)
        .await
        .unwrap();
    let parsed = response.validate_and_parse(&keystore, true).unwrap();

    // A negative appraisal is still a successfully validated EAR. Preserve the
    // structured claims instead of flattening nested values into strings.
    assert_eq!(parsed.ear_status, EarStatus::Contraindicated);
    assert_eq!(parsed.submods.len(), 2);
    assert_eq!(parsed.submods["gpu_0"].ear_status, EarStatus::Affirming);
    assert_eq!(
        parsed.submods["gpu_1"].ear_status,
        EarStatus::Contraindicated
    );
    assert!(parsed.eat_nonce.is_none());
    assert_eq!(parsed.additional_claims["ear-extension"], "preserved");
    assert_eq!(
        parsed.ear_nvidia_inputs.additional_claims["input-extension"],
        true
    );
    assert_eq!(
        parsed.submods["gpu_0"].additional_claims["submodule-extension"],
        json!([1, 2])
    );
    assert_eq!(
        parsed.submods["gpu_1"].ear_nvidia_error_details,
        Some(json!({"reason": "test appraisal"}))
    );
    verify.assert_async().await;
    jwks.assert_async().await;
}

#[tokio::test]
async fn nras_comm_failure_returns_comm_err() {
    let client = nras::NrasVerifierClient::new_with_config(
        &nras::Config {
            nras_url: "invalidurl".to_string(),
            ..Default::default()
        },
        carbide_instrument::Instrumentation::noop(),
    );
    let error = client.attest(vec![device("gpu_0")]).await.unwrap_err();
    assert!(matches!(error, NrasError::Communication(_)));
}

#[tokio::test]
async fn nras_non_200_responses_return_status_without_the_body() {
    let mut server = mockito::Server::new_async().await;
    for status in [400, 503] {
        // Error messages from NRAS are untrusted and may contain attestation material.
        let response = server.mock("POST", "/v1/verify")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errorCode":4,"message":"sensitive-evidence","timestamp":"2026-07-29T04:43:14Z"}"#)
            .create_async().await;
        let client = nras::NrasVerifierClient::new_with_config(
            &nras::Config {
                nras_url: server.url(),
                ..Default::default()
            },
            carbide_instrument::Instrumentation::noop(),
        );
        let error = client.attest(vec![device("gpu_0")]).await.unwrap_err();
        assert!(matches!(error, NrasError::Communication(_)));
        assert!(error.to_string().contains(&status.to_string()));
        assert!(!error.to_string().contains("sensitive-evidence"));
        response.assert_async().await;
        response.remove_async().await;
    }
}

#[tokio::test]
async fn malformed_collections_fail_before_http() {
    let client = nras::NrasVerifierClient::new_with_config(
        &nras::Config {
            nras_url: "invalidurl".to_string(),
            ..Default::default()
        },
        carbide_instrument::Instrumentation::noop(),
    );
    let changed = |field: &str, value: &str| {
        let mut input = device("gpu_0");
        match field {
            "evidence" => input.evidence = value.to_string(),
            "certificate" => input.certificate = value.to_string(),
            "nonce" => input.nonce = value.to_string(),
            _ => unreachable!(),
        }
        vec![input]
    };
    // Distinguish label collisions from encoding errors. The invalid URL exposes
    // any case that unexpectedly reaches the HTTP request.
    let cases = [
        ("empty collection", vec![], "Serde"),
        (
            "duplicate labels",
            vec![device("gpu_0"), device("gpu_0")],
            "Bmc",
        ),
        ("reserved profile label", vec![device("__cmwc_t")], "Bmc"),
        ("malformed evidence", changed("evidence", "!"), "Bmc"),
        ("malformed certificate", changed("certificate", "!"), "Bmc"),
        ("odd hex length", changed("nonce", "a"), "Serde"),
        ("non-ASCII hex", changed("nonce", "é"), "Serde"),
        (
            "nonce below minimum",
            changed("nonce", &"fb".repeat(31)),
            "Serde",
        ),
        (
            "nonce above maximum",
            changed("nonce", &"fb".repeat(97)),
            "Serde",
        ),
    ];
    check_cases_async(
        cases.into_iter().map(|(scenario, input, error_kind)| Case {
            scenario,
            input,
            expect: Outcome::FailsWith(error_kind),
        }),
        |input| async {
            client
                .attest(input)
                .await
                .map(drop)
                .map_err(|error| match error {
                    NrasError::Bmc(_) => "Bmc",
                    NrasError::Serde(_) => "Serde",
                    _ => "unexpected error kind",
                })
        },
    )
    .await;
}

#[test]
fn signed_ear_validation_checks_structure_keys_signature_and_times() {
    use carbide_test_support::Outcome::*;

    let fixture = SignedEarFixture::new();
    let claims = ear_claims();
    let token = fixture.sign(&claims, Some(KEY_ID));
    let mut expired = claims.clone();
    expired["exp"] = json!(jsonwebtoken::get_current_timestamp() - 3600);
    let mut future = claims.clone();
    future["nbf"] = json!(jsonwebtoken::get_current_timestamp() + 3600);
    let mut missing_claim = claims.clone();
    missing_claim.as_object_mut().unwrap().remove("iss");
    let mut malformed_submod = claims.clone();
    malformed_submod["submods"]["gpu_0"]["ear_status"] = json!("unrecognised");
    let mut warning = claims.clone();
    warning["ear_status"] = json!("warning");
    warning["submods"]["gpu_0"]["ear_status"] = json!("warning");
    let other_key = SignedEarFixture::new();

    // Every case exercises the public validation boundary; failures must remain
    // failures even when HTTP has already returned a successful response.
    scenarios!(run = |(token, validate_expiry): (String, bool)| {
        VerifierResponseJwt::from(token).validate_and_parse(&fixture, validate_expiry)
            .map(|parsed| parsed.ear_status).map_err(drop)
    };
        "valid signed results" {
            (token, true) => Yields(EarStatus::Affirming),
            (fixture.sign(&warning, Some(KEY_ID)), true) => Yields(EarStatus::Warning),
            (fixture.sign(&expired, Some(KEY_ID)), false) => Yields(EarStatus::Affirming),
        }
        "invalid JWTs and claims" {
            ("malformed-token".to_string(), true) => Fails,
            (fixture.sign(&claims, None), true) => Fails,
            (fixture.sign(&claims, Some("unknown-key")), true) => Fails,
            (other_key.sign(&claims, Some(KEY_ID)), true) => Fails,
            (fixture.sign(&missing_claim, Some(KEY_ID)), true) => Fails,
            (fixture.sign(&malformed_submod, Some(KEY_ID)), true) => Fails,
            (fixture.sign(&expired, Some(KEY_ID)), true) => Fails,
            (fixture.sign(&future, Some(KEY_ID)), false) => Fails,
        }
    );
}

#[tokio::test]
async fn keystore_reports_transport_http_and_decoding_failures() {
    let mut server = mockito::Server::new_async().await;
    for (scenario, status, body, expected_kind) in [
        ("HTTP error", 503, "", "communication"),
        ("malformed JSON", 200, "not-json", "serde"),
        (
            "malformed EC key",
            200,
            r#"{"keys":[{"kty":"EC","kid":"broken","x5c":["invalid"]}]}"#,
            "jwk",
        ),
    ] {
        let response = server
            .mock("GET", "/keystore")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let url = format!("{}/keystore", server.url());
        let result = nras::NrasKeyStore::new_with_config(
            &nras::Config {
                nras_jwks_url: url,
                ..Default::default()
            },
            &carbide_instrument::Instrumentation::noop(),
        )
        .await;
        let kind = match result.unwrap_err() {
            NrasError::Communication(_) => "communication",
            NrasError::Serde(_) => "serde",
            NrasError::Jwk(_) => "jwk",
            other => panic!("{scenario}: unexpected error {other:?}"),
        };
        assert_eq!(kind, expected_kind, "{scenario}");
        response.assert_async().await;
        response.remove_async().await;
    }
}
