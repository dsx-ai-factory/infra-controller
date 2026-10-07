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

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

use crate::{
    MAX_BOOTSTRAP_CA_BYTES, download_bootstrap_ca, download_bootstrap_ca_with_timeout,
    install_bootstrap_ca, publish_bootstrap_ca, read_bootstrap_ca_file,
    republish_bootstrap_ca_if_changed,
};

const VALID_CA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../dev/forge_prodroot.pem"
));
const PRIVATE_KEY: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../dev/certs/server_identity.key"
));
const UNKNOWN_PRIVATE_KEY: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\n\
                                      c2VjcmV0\n\
                                      -----END OPENSSH PRIVATE KEY-----\n";

struct TestServer {
    url: Url,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        self.task.await.unwrap();
    }
}

async fn serve(status: StatusCode, body: Vec<u8>) -> TestServer {
    serve_after(Duration::ZERO, status, body).await
}

async fn serve_after(delay: Duration, status: StatusCode, body: Vec<u8>) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, shutdown_receiver) = oneshot::channel();
    let app = Router::new().route(
        "/root-ca",
        get(move || {
            let body = body.clone();
            async move {
                tokio::time::sleep(delay).await;
                (status, body)
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_receiver.await.ok();
            })
            .await
            .unwrap();
    });
    TestServer {
        url: Url::parse(&format!("http://{address}/root-ca")).unwrap(),
        shutdown,
        task,
    }
}

#[tokio::test]
async fn bootstrap_ca_download_accepts_successful_bounded_response() {
    let server = serve(StatusCode::OK, VALID_CA.to_vec()).await;
    let result = download_bootstrap_ca(&server.url).await;
    server.shutdown().await;
    assert_eq!(result.unwrap(), VALID_CA);
}

#[tokio::test]
async fn bootstrap_ca_download_rejects_error_status_without_body_installation() {
    let server = serve(StatusCode::BAD_GATEWAY, VALID_CA.to_vec()).await;
    let result = download_bootstrap_ca(&server.url).await;
    server.shutdown().await;
    let error = result.unwrap_err();
    assert!(error.to_string().contains("error status"));
}

#[tokio::test]
async fn bootstrap_ca_download_rejects_oversized_response() {
    let server = serve(StatusCode::OK, vec![b'x'; MAX_BOOTSTRAP_CA_BYTES + 1]).await;
    let result = download_bootstrap_ca(&server.url).await;
    server.shutdown().await;
    let error = result.unwrap_err();
    assert!(error.to_string().contains("size limit"));
}

#[tokio::test]
async fn mounted_bootstrap_ca_read_rejects_oversized_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ca.pem");
    std::fs::write(&path, vec![b'x'; MAX_BOOTSTRAP_CA_BYTES + 1]).unwrap();

    let error = read_bootstrap_ca_file(&path).await.unwrap_err();

    assert!(error.to_string().contains("size limit"));
}

#[tokio::test]
async fn bootstrap_ca_download_times_out() {
    let server = serve_after(
        Duration::from_millis(100),
        StatusCode::OK,
        VALID_CA.to_vec(),
    )
    .await;
    let result = download_bootstrap_ca_with_timeout(&server.url, Duration::from_millis(10)).await;
    server.shutdown().await;

    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failed to download bootstrap CA")
    );
    assert!(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_timeout),
        "expected reqwest timeout, got {error:?}"
    );
}

#[test]
fn bootstrap_ca_install_atomically_replaces_existing_file_with_mode_0644() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("forge_root.pem");
    std::fs::write(&output, b"old trust anchor").unwrap();

    install_bootstrap_ca(VALID_CA, &output).unwrap();

    assert_eq!(std::fs::read(&output).unwrap(), VALID_CA);
    assert_eq!(
        std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn invalid_bootstrap_ca_preserves_existing_file() {
    let mut certificate_and_key = VALID_CA.to_vec();
    certificate_and_key.extend_from_slice(PRIVATE_KEY);
    let mut certificate_and_unknown_key = VALID_CA.to_vec();
    certificate_and_unknown_key.extend_from_slice(UNKNOWN_PRIVATE_KEY);
    let mut certificate_with_suffixed_begin = b"-----BEGIN CERTIFICATE-----suffix\n".to_vec();
    certificate_with_suffixed_begin
        .extend_from_slice(&VALID_CA[b"-----BEGIN CERTIFICATE-----\n".len()..]);
    let cases = [
        ("empty", Vec::new()),
        ("not PEM", b"this is not a certificate".to_vec()),
        (
            "malformed certificate PEM",
            b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n".to_vec(),
        ),
        (
            "non-certificate DER",
            b"-----BEGIN CERTIFICATE-----\nbm90LWFuLXg1MDktY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n".to_vec(),
        ),
        ("certificate plus private key", certificate_and_key),
        (
            "certificate plus unsupported private key",
            certificate_and_unknown_key,
        ),
        (
            "certificate with a suffixed BEGIN marker",
            certificate_with_suffixed_begin,
        ),
    ];

    for (scenario, invalid_ca) in cases {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("forge_root.pem");
        std::fs::write(&output, b"old trust anchor").unwrap();

        assert!(
            install_bootstrap_ca(&invalid_ca, &output).is_err(),
            "{scenario} should fail"
        );
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"old trust anchor",
            "{scenario} should preserve the existing trust anchor"
        );
    }
}

#[test]
fn oversized_bootstrap_ca_preserves_existing_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("forge_root.pem");
    std::fs::write(&output, b"old trust anchor").unwrap();
    let oversized = vec![b'x'; MAX_BOOTSTRAP_CA_BYTES + 1];

    assert!(install_bootstrap_ca(&oversized, &output).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"old trust anchor");
}

/// The published copy has to land beside whichever CA path is configured.
/// Token-mode fmds waits on `<certsDir>/pub/<rootCaFile>`, so publishing to a
/// fixed location would leave its init container blocked forever on any
/// deployment that moved the credentials directory — with nothing logged to
/// explain it.
#[test]
fn published_ca_follows_the_configured_credentials_directory() {
    let directory = tempfile::tempdir().unwrap();
    let creds = directory.path().join("srv").join("creds");
    std::fs::create_dir_all(&creds).unwrap();
    let source = creds.join("site_root.pem");

    publish_bootstrap_ca(VALID_CA, &source).unwrap();

    let published = creds.join("pub").join("site_root.pem");
    assert_eq!(
        std::fs::read(&published).unwrap(),
        VALID_CA,
        "the CA must be published to {}",
        published.display()
    );
}

/// Republishing over an existing copy is the rotation path, and must land
/// atomically rather than leaving a truncated file a consumer could read.
#[test]
fn publishing_replaces_a_previous_published_ca() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("forge_root.pem");
    let published_dir = directory.path().join("pub");
    std::fs::create_dir_all(&published_dir).unwrap();
    std::fs::write(published_dir.join("forge_root.pem"), b"old trust anchor").unwrap();

    publish_bootstrap_ca(VALID_CA, &source).unwrap();

    assert_eq!(
        std::fs::read(published_dir.join("forge_root.pem")).unwrap(),
        VALID_CA
    );
}

/// A CA rotated while the agent is running must reach the published copy.
/// `pub/` is the only trust anchor a token-mode consumer has, so leaving it on
/// the old issuer would break fmds the moment that issuer is retired — the same
/// stale-snapshot failure the API-side validator reload exists to prevent.
#[test]
fn republish_picks_up_a_rotated_ca() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("forge_root.pem");
    let published = directory.path().join("pub").join("forge_root.pem");

    // Both anchors must be real certificates: publishing validates, so a
    // placeholder would be rejected and the test would prove nothing.
    let old_ca = a_self_signed_ca();
    std::fs::write(&source, &old_ca).unwrap();
    republish_bootstrap_ca_if_changed(&source.to_string_lossy());
    assert_eq!(std::fs::read(&published).unwrap(), old_ca);

    // Rotation, out of band, with the agent already running.
    std::fs::write(&source, VALID_CA).unwrap();
    republish_bootstrap_ca_if_changed(&source.to_string_lossy());
    assert_eq!(
        std::fs::read(&published).unwrap(),
        VALID_CA,
        "the published copy must follow the configured one"
    );
}

/// Steady state must not rewrite. The reconcile runs on a timer, and an atomic
/// rename every interval would churn the inode consumers are watching for no
/// reason.
#[test]
fn republish_is_a_no_op_when_the_ca_is_unchanged() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("forge_root.pem");
    let published = directory.path().join("pub").join("forge_root.pem");
    std::fs::write(&source, VALID_CA).unwrap();

    republish_bootstrap_ca_if_changed(&source.to_string_lossy());
    let first = std::fs::metadata(&published).unwrap().ino();

    republish_bootstrap_ca_if_changed(&source.to_string_lossy());
    let second = std::fs::metadata(&published).unwrap().ino();

    assert_eq!(first, second, "an unchanged CA must not be republished");
}

/// Before registration there is no CA at all. That is the normal first-boot
/// state, not an error, and it must not panic the republish loop.
#[test]
fn republish_tolerates_a_missing_ca() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("forge_root.pem");
    republish_bootstrap_ca_if_changed(&missing.to_string_lossy());
    assert!(!directory.path().join("pub").exists());
}

/// A second, distinct trust anchor — publishing validates its input, so
/// rotation tests need real certificates on both sides of the change.
fn a_self_signed_ca() -> Vec<u8> {
    let params = rcgen::CertificateParams::default();
    let key = rcgen::KeyPair::generate().expect("key pair");
    params
        .self_signed(&key)
        .expect("certificate")
        .pem()
        .into_bytes()
}
