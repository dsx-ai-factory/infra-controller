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

use std::collections as stdcol;

use common::fixtures::*;
use common::{mock_keystore as mks, mock_server as ms};
use nras::{DeviceAttestationInfo, NrasError, VerifierClient};

// --> NrasVerifierClient <--
#[tokio::test]
async fn nras_comm_failure_returns_comm_err() {
    // set up - do nothing
    let config = nras::Config {
        nras_url: "invalidurl".to_string(),
        nras_gpu_url_suffix: "invalid_urls_suffix".to_string(),
        nras_jwks_url: "invalid_jwks_url".to_string(),
        validate_jwt_expiry: false,
    };
    // execute
    let client = nras::NrasVerifierClient::new_with_config(&config);
    let device_att_info: DeviceAttestationInfo = Default::default();

    let actual_err = client
        .attest_gpu(&device_att_info)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Communication(_)));
}

#[tokio::test]
async fn nras_response_parsing_failure_returns_parsing_err() {
    // set up - create mockito http server
    let mut server = ms::create_mock_http_server().await;
    let url = ms::add_mock(
        &mut server,
        "/client",
        MALFORMED_VERIFIER_RESPONSE,
        &ms::Method::Post,
        200,
    );

    let config = nras::Config {
        nras_url: url,
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: String::new(),
        validate_jwt_expiry: false,
    };

    // execute
    let client = nras::NrasVerifierClient::new_with_config(&config);
    let device_att_info: DeviceAttestationInfo = Default::default();

    let actual_err = client
        .attest_gpu(&device_att_info)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Serde(_)));
}

#[tokio::test]
async fn nras_response_not_200_returns_comm_error() {
    let mut server = ms::create_mock_http_server().await;
    let url = ms::add_mock(&mut server, "/client", "", &ms::Method::Post, 404);

    let config = nras::Config {
        nras_url: url,
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: String::new(),
        validate_jwt_expiry: false,
    };

    // execute
    let client = nras::NrasVerifierClient::new_with_config(&config);
    let device_att_info: DeviceAttestationInfo = Default::default();

    let actual_err = client
        .attest_gpu(&device_att_info)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Communication(_)));
}

// --> NrasKeyStore <--
#[tokio::test]
async fn keystore_comm_failure_returns_comm_error() {
    // set up
    let mut server = ms::create_mock_http_server().await;
    let url = ms::add_mock(&mut server, "/keystore", "", &ms::Method::Post, 404);

    let config = nras::Config {
        nras_url: String::new(),
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: url,
        validate_jwt_expiry: false,
    };

    // execute
    let actual_err = nras::NrasKeyStore::new_with_config(&config)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Communication(_)));
}

#[tokio::test]
async fn keystore_status_code_not_200_returns_comm_error() {
    // set up
    let mut server = ms::create_mock_http_server().await;
    let url = ms::add_mock(
        &mut server,
        "/keystore",
        GOOD_JWKS_RESPONSE,
        &ms::Method::Get,
        500,
    );

    let config = nras::Config {
        nras_url: String::new(),
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: url,
        validate_jwt_expiry: false,
    };

    // execute
    let actual_err = nras::NrasKeyStore::new_with_config(&config)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Communication(_)));
}

#[tokio::test]
async fn keystore_parsing_error_returns_jwk_err() {
    // set up
    let mut server = ms::create_mock_http_server().await;
    let url = ms::add_mock(
        &mut server,
        "/keystore",
        MALFORMED_JWK_RESPONSE,
        &ms::Method::Get,
        200,
    );

    let config = nras::Config {
        nras_url: String::new(),
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: url,
        validate_jwt_expiry: false,
    };

    // execute
    let actual_err = nras::NrasKeyStore::new_with_config(&config)
        .await
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Jwk(_)));
}

// --> parser <--
#[test]
fn parser_malformed_jwt_returns_jwt_error() {
    // set up
    let mock_keystore = mks::MockKeyStore::new_with_key(JWK_X, JWK_Y);

    let raw_outcome = nras::RawAttestationOutcome {
        overall_outcome: ("JWT".to_string(), MALFORMED_JWT.to_string()),
        devices_outcome: stdcol::HashMap::from([(
            "GPU-0".to_string(),
            GOOD_DEVICES_JWT.to_string(),
        )]),
    };

    let config = nras::Config {
        validate_jwt_expiry: false,
        ..Default::default()
    };

    let parser = nras::Parser::new_with_config(&config);

    // execute

    let actual_err = parser
        .parse_attestation_outcome(&raw_outcome, &mock_keystore)
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Jwt(_)));
}

#[test]
fn parser_malformed_devices_jwt_returns_jwt_error() {
    // set up
    let mock_keystore = mks::MockKeyStore::new_with_key(JWK_X, JWK_Y);

    let raw_outcome = nras::RawAttestationOutcome {
        overall_outcome: ("JWT".to_string(), GOOD_JWT.to_string()),
        devices_outcome: stdcol::HashMap::from([(
            "GPU-0".to_string(),
            MALFORMED_DEVICES_JWT.to_string(),
        )]),
    };

    let config = nras::Config {
        validate_jwt_expiry: false,
        ..Default::default()
    };

    let parser = nras::Parser::new_with_config(&config);

    // execute

    let actual_err = parser
        .parse_attestation_outcome(&raw_outcome, &mock_keystore)
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::Jwt(_)));
}

#[test]
fn parser_decoding_key_not_found_returns_decoding_key_error() {
    // set up
    let mock_keystore = mks::MockKeyStore::new_with_no_key();

    let raw_outcome = nras::RawAttestationOutcome {
        overall_outcome: ("JWT".to_string(), GOOD_JWT.to_string()),
        devices_outcome: stdcol::HashMap::from([(
            "GPU-0".to_string(),
            GOOD_DEVICES_JWT.to_string(),
        )]),
    };

    let config = nras::Config {
        validate_jwt_expiry: false,
        ..Default::default()
    };

    let parser = nras::Parser::new_with_config(&config);

    // execute
    let actual_err = parser
        .parse_attestation_outcome(&raw_outcome, &mock_keystore)
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::DecodingKeyNotFound(_)));
}

#[test]
fn parser_submod_has_missing_device_returns_verifier_error() {
    // set up
    let mock_keystore = mks::MockKeyStore::new_with_key(JWK_X, JWK_Y);

    let raw_outcome = nras::RawAttestationOutcome {
        overall_outcome: ("JWT".to_string(), GOOD_JWT.to_string()),
        devices_outcome: stdcol::HashMap::from([(
            "GPU-1".to_string(),
            GOOD_DEVICES_JWT.to_string(),
        )]),
    };

    let config = nras::Config {
        validate_jwt_expiry: false,
        ..Default::default()
    };

    let parser = nras::Parser::new_with_config(&config);

    // execute

    let actual_err = parser
        .parse_attestation_outcome(&raw_outcome, &mock_keystore)
        .expect_err("Expected NrasError to be returned");

    // verify
    assert!(matches!(actual_err, NrasError::ParsingVerifierResponse(_)));
}

#[tokio::test]
async fn happy_path_one_gpu_returns_processed_attestation_outcome() {
    // set up - create mockito http server
    let mut server = ms::create_mock_http_server().await;
    let url_client = ms::add_mock(
        &mut server,
        "/client",
        GOOD_VERIFIER_RESPONSE,
        &ms::Method::Post,
        200,
    );
    let url_keystore = ms::add_mock(
        &mut server,
        "/keystore",
        GOOD_JWKS_RESPONSE,
        &ms::Method::Get,
        200,
    );

    let config = nras::Config {
        nras_url: url_client,
        nras_gpu_url_suffix: String::new(),
        nras_jwks_url: url_keystore,
        validate_jwt_expiry: false,
    };

    let client = nras::NrasVerifierClient::new_with_config(&config);

    let keystore = nras::NrasKeyStore::new_with_config(&config)
        .await
        .expect("Unexpected error creating KeyStore");

    let device_att_info: DeviceAttestationInfo = Default::default();

    let parser = nras::Parser::new_with_config(&config);

    // execute
    let raw_outcome = client
        .attest_gpu(&device_att_info)
        .await
        .expect("Unexpected error attesting GPU");

    let parsed_outcome = parser
        .parse_attestation_outcome(&raw_outcome, &keystore)
        .expect("Unexpected error parsing raw attestation outcome");

    // verify
    assert!(parsed_outcome.attestation_passed);
}
