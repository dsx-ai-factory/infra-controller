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

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use libmlx::firmware::credentials::Credentials;

// -- validate_http --

#[test]
fn validate_http_accepts_http_creds_and_rejects_ssh_creds() {
    scenarios!(
        run = |cred| cred.validate_http().map_err(drop);
        "bearer token is valid for http" {
            Credentials::bearer_token("my-token") => Yields(()),
        }

        "basic auth is valid for http" {
            Credentials::basic_auth("user", "pass") => Yields(()),
        }

        "header is valid for http" {
            Credentials::header("X-API-Key", "abc123") => Yields(()),
        }

        "ssh key is invalid for http" {
            Credentials::ssh_key("/home/user/.ssh/id_rsa") => Fails,
        }

        "ssh agent is invalid for http" {
            Credentials::ssh_agent() => Fails,
        }
    );
}

// -- validate_ssh --

#[test]
fn validate_ssh_accepts_ssh_creds_and_rejects_http_creds() {
    scenarios!(
        run = |cred| cred.validate_ssh().map_err(drop);
        "ssh key is valid for ssh" {
            Credentials::ssh_key("/home/user/.ssh/id_rsa") => Yields(()),
        }

        "ssh key with passphrase is valid for ssh" {
            Credentials::ssh_key_with_passphrase(
                "/home/user/.ssh/id_rsa",
                "my-passphrase",
            ) => Yields(()),
        }

        "ssh agent is valid for ssh" {
            Credentials::ssh_agent() => Yields(()),
        }

        "bearer token is invalid for ssh" {
            Credentials::bearer_token("my-token") => Fails,
        }

        "basic auth is invalid for ssh" {
            Credentials::basic_auth("user", "pass") => Fails,
        }
    );
}

// -- serde roundtrip --

#[test]
fn test_bearer_token_serde_roundtrip() {
    let cred = Credentials::bearer_token("my-secret-token");
    let toml = toml::to_string(&cred).unwrap();
    let deserialized: Credentials = toml::from_str(&toml).unwrap();

    match deserialized {
        Credentials::BearerToken { token } => assert_eq!(token, "my-secret-token"),
        other => panic!("Expected BearerToken, got {other:?}"),
    }
}

#[test]
fn test_basic_auth_serde_roundtrip() {
    let cred = Credentials::basic_auth("deploy", "s3cret");
    let toml = toml::to_string(&cred).unwrap();
    let deserialized: Credentials = toml::from_str(&toml).unwrap();

    match deserialized {
        Credentials::BasicAuth { username, password } => {
            assert_eq!(username, "deploy");
            assert_eq!(password, "s3cret");
        }
        other => panic!("Expected BasicAuth, got {other:?}"),
    }
}

#[test]
fn test_ssh_agent_serde_roundtrip() {
    let cred = Credentials::ssh_agent();
    let toml = toml::to_string(&cred).unwrap();
    let deserialized: Credentials = toml::from_str(&toml).unwrap();

    assert!(matches!(deserialized, Credentials::SshAgent {}));
}

#[test]
fn test_ssh_key_serde_roundtrip() {
    let cred = Credentials::ssh_key("/home/deploy/.ssh/id_ed25519");
    let toml = toml::to_string(&cred).unwrap();
    let deserialized: Credentials = toml::from_str(&toml).unwrap();

    match deserialized {
        Credentials::SshKey { path, passphrase } => {
            assert_eq!(path, "/home/deploy/.ssh/id_ed25519");
            assert!(passphrase.is_none());
        }
        other => panic!("Expected SshKey, got {other:?}"),
    }
}
