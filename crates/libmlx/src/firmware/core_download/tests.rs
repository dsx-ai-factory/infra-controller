// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Exercise the authenticated consumer's response boundaries with real HTTP/2.

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, Response, Version};
use axum::routing::get;
use carbide_test_support::{Check, check_values};
use rcgen::{CertificateParams, KeyPair, SanType};
use tokio::net::TcpListener;
use tokio::sync::{Notify, oneshot};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Identity, Server, ServerTlsConfig};

use super::*;

/// Select a real response or stream failure at the Core transport boundary.
#[derive(Clone, Copy)]
enum Reply {
    PendingThenReady,
    StalledBody,
    OversizeBody,
    Redirect,
    PartialThenReady,
    BrokenBody,
}

/// Capture exact paths and node authentication while serving controlled bodies.
#[derive(Clone)]
struct Origin {
    reply: Reply,
    requests: Arc<Mutex<Vec<(String, bool)>>>,
    count: Arc<AtomicUsize>,
    request_times: Arc<Mutex<Vec<Instant>>>,
    fail_body: Arc<Notify>,
}

/// Produce actual HTTP/2 frames for the selected response boundary.
async fn reply(State(origin): State<Origin>, request: Request<Body>) -> Response<Body> {
    assert_eq!(request.version(), Version::HTTP_2);
    origin.request_times.lock().unwrap().push(Instant::now());
    origin.requests.lock().unwrap().push((
        request.uri().path_and_query().unwrap().to_string(),
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer ")),
    ));
    let call = origin.count.fetch_add(1, Ordering::SeqCst);
    let mut response = Response::builder();
    let body = match origin.reply {
        Reply::PendingThenReady if call < 2 => {
            // Neither a cache-pending body nor a temporary unavailable body is
            // an artifact. Retry-After zero must retain the one-second floor.
            response = response
                .status(if call == 0 { 202 } else { 503 })
                .header(header::RETRY_AFTER, "0");
            Body::from("not firmware")
        }
        Reply::PendingThenReady => Body::from(vec![b'x'; 64 * 1024 + 17]),
        Reply::PartialThenReady if call > 0 => Body::from("complete"),
        Reply::PartialThenReady | Reply::BrokenBody => {
            let frames = tokio_stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
                b"stale partial bytes",
            ))]);
            let wait_for_prefix = matches!(origin.reply, Reply::PartialThenReady);
            let failure = tokio_stream::once(origin.fail_body).then(move |fail_body| async move {
                if wait_for_prefix {
                    // Wait until the test independently sees the staged prefix.
                    fail_body.notified().await;
                }
                Err(std::io::Error::other("controlled response stream failure"))
            });
            Body::from_stream(tokio_stream::StreamExt::chain(frames, failure))
        }
        Reply::StalledBody => {
            let frames = tokio_stream::iter([Ok::<_, Infallible>(Bytes::from_static(b"prefix"))]);
            Body::from_stream(tokio_stream::StreamExt::chain(
                frames,
                tokio_stream::pending(),
            ))
        }
        Reply::OversizeBody => Body::from_stream(tokio_stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"first")),
            Ok(Bytes::from_static(b"too large")),
        ])),
        Reply::Redirect => {
            // The original URL is intentionally unreachable. A redirect or a
            // cache failure must never select it as another fetch destination.
            response = response
                .status(302)
                .header(header::LOCATION, "http://127.0.0.1:9/elsewhere");
            Body::empty()
        }
    };
    response.body(body).unwrap()
}

/// Keep listener, identity files and bounded shutdown under each test's owner.
struct Fixture {
    directory: tempfile::TempDir,
    config: CoreFirmwareDownloadConfig,
    requests: Arc<Mutex<Vec<(String, bool)>>>,
    request_times: Arc<Mutex<Vec<Instant>>>,
    fail_body: Arc<Notify>,
    stop: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl Fixture {
    /// Start an isolated TLS listener and certificate-based client configuration.
    async fn start(response: Reply) -> Self {
        // Tonic's test listener uses the process provider; the production
        // Forge client already selects AWS-LC explicitly. Another test may
        // have installed the same provider first.
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .ok();
        let directory = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.subject_alt_names.push(SanType::URI(
            "spiffe://test/machine/firmware".try_into().unwrap(),
        ));
        let certificate = params.self_signed(&key).unwrap();
        let write = |name, content| {
            let path = directory.path().join(name);
            std::fs::write(&path, content).unwrap();
            path.into_os_string().into_string().unwrap()
        };
        let root_ca_path = write("ca.pem", certificate.pem());
        let client_cert_path = write("client.pem", certificate.pem());
        let client_key_path = write("key.pem", key.serialize_pem());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_times = Arc::new(Mutex::new(Vec::new()));
        let fail_body = Arc::new(Notify::new());
        let origin = Origin {
            reply: response,
            requests: requests.clone(),
            count: Arc::new(AtomicUsize::new(0)),
            request_times: request_times.clone(),
            fail_body: fail_body.clone(),
        };
        let routes = Router::new().fallback(get(reply)).with_state(origin);
        let (stop, stopped) = oneshot::channel();
        let server = Server::builder()
            .tls_config(
                ServerTlsConfig::new()
                    .identity(Identity::from_pem(certificate.pem(), key.serialize_pem())),
            )
            .unwrap()
            .add_routes(routes.into())
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                stopped.await.ok();
            });
        let server = tokio::spawn(server);
        Self {
            directory,
            config: CoreFirmwareDownloadConfig {
                core_url: format!("https://localhost:{port}"),
                root_ca_path,
                client_cert_path,
                client_key_path,
                enforce_tls: true,
                require_client_cert: true,
                use_mgmt_vrf: false,
                request_timeout: Some(Duration::from_secs(2)),
                socks_proxy: None,
                connect_retries_max: Some(0),
                connect_retries_interval: None,
            },
            requests,
            request_times,
            fail_body,
            stop,
            server,
        }
    }

    /// Pin one operation ID and digest independently of the fetch address.
    fn delivery(&self) -> CoreFirmwareDelivery {
        CoreFirmwareDelivery::new(
            self.config.clone(),
            "operation/1?x=y".into(),
            FirmwareArtifactDigests {
                image_sha256: [1; 32],
                configuration_sha256: None,
            },
        )
        .unwrap()
    }

    /// Close and join the listener before disposing of credential files.
    async fn finish(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.server)
            .await
            .expect("controlled HTTP/2 server stops")
            .unwrap()
            .unwrap();
    }
}

/// Retry cache readiness without writing its diagnostic body, and retain both
/// node authentication and the exact encoded operation identity on every GET.
#[tokio::test]
async fn cache_readiness_stages_only_the_final_artifact() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let fixture = Fixture::start(Reply::PendingThenReady).await;
        let path = fixture
            .delivery()
            .resolve(
                "http://127.0.0.1:9/image.bin",
                fixture.directory.path(),
                CoreArtifactRole::Image,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; 64 * 1024 + 17]);
        let expected = format!(
            "/firmware/nic/{}/image?operation_id=operation%2F1%3Fx%3Dy",
            "01".repeat(32)
        );
        assert_eq!(*fixture.requests.lock().unwrap(), vec![(expected, true); 3]);
        assert!(
            fixture
                .request_times
                .lock()
                .unwrap()
                .windows(2)
                .all(|pair| { pair[1].duration_since(pair[0]) >= Duration::from_secs(1) }),
            "Retry-After zero must not cause immediate polling"
        );
        fixture.finish().await;
    })
    .await
    .expect("cache readiness is independently bounded");
}

/// A failed response must settle the first prefix before a retry truncates it.
/// The final bytes and repeated authenticated route distinguish recovery from
/// appending, skipping the retry, or selecting the original source instead.
#[tokio::test]
async fn partial_transfer_retries_with_only_the_complete_artifact() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let fixture = Fixture::start(Reply::PartialThenReady).await;
        let mut delivery = fixture.delivery();
        delivery.limits.retry_delay = Duration::from_millis(10);
        let path = fixture.directory.path().join("image.bin");
        let (result, ()) = tokio::join!(
            delivery.resolve(
                "http://127.0.0.1:9/image.bin",
                fixture.directory.path(),
                CoreArtifactRole::Image,
            ),
            async {
                loop {
                    if tokio::fs::read(&path)
                        .await
                        .is_ok_and(|bytes| bytes == b"stale partial bytes")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                fixture.fail_body.notify_one();
            }
        );
        assert_eq!(result.unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), b"complete");
        let expected = format!(
            "/firmware/nic/{}/image?operation_id=operation%2F1%3Fx%3Dy",
            "01".repeat(32)
        );
        assert_eq!(*fixture.requests.lock().unwrap(), vec![(expected, true); 2]);
        fixture.finish().await;
    })
    .await
    .expect("partial transfer recovery is independently bounded");
}

/// Persistent transport failure stops after the initial request and three
/// retries, rather than using the whole artifact deadline as an attempt count.
#[tokio::test]
async fn transport_retries_stop_at_the_artifact_attempt_limit() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let fixture = Fixture::start(Reply::BrokenBody).await;
        let mut delivery = fixture.delivery();
        delivery.limits.retry_delay = Duration::from_millis(10);
        let error = delivery
            .resolve(
                "http://127.0.0.1:9/image.bin",
                fixture.directory.path(),
                CoreArtifactRole::Image,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, FirmwareError::CoreTransfer { .. }));
        assert_eq!(fixture.requests.lock().unwrap().len(), 4);
        fixture.finish().await;
    })
    .await
    .expect("transport retry exhaustion is independently bounded");
}

/// A missing identity file fails before requests and retains a useful category
/// without copying private file names into the durable diagnostic.
#[tokio::test]
async fn setup_failures_preserve_safe_causes_without_retrying_requests() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let fixture = Fixture::start(Reply::PendingThenReady).await;
        let mut delivery = fixture.delivery();
        delivery.config.root_ca_path = fixture
            .directory
            .path()
            .join("private-ca-location-secret.pem")
            .to_str()
            .unwrap()
            .into();
        let error = delivery
            .resolve(
                "http://127.0.0.1:9/image.bin",
                fixture.directory.path(),
                CoreArtifactRole::Image,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            FirmwareError::CoreTransfer {
                failure: CoreTransferFailure::RootCertificate(std::io::ErrorKind::NotFound),
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "core firmware transfer failed during root certificate read (NotFound)"
        );
        assert!(fixture.requests.lock().unwrap().is_empty());
        assert!(!fixture.directory.path().join("image.bin").exists());
        fixture.finish().await;
    })
    .await
    .expect("local setup failure does not consume network retry delays");
}

/// Stalled bodies, chunked overflows and redirects are distinct boundaries:
/// none may succeed, retry through Direct, or leave buffered writes unsettled.
#[tokio::test]
async fn response_failures_stay_bounded_and_never_fall_back() {
    struct Case {
        scenario: &'static str,
        response: Reply,
        check: fn(FirmwareError, &Path),
    }
    for case in [
        Case {
            scenario: "body deadline",
            response: Reply::StalledBody,
            check: |error, path| {
                assert!(matches!(error, FirmwareError::SourceTransferTimeout { .. }));
                assert_eq!(std::fs::read(path).unwrap(), b"prefix");
            },
        },
        Case {
            scenario: "chunked byte limit",
            response: Reply::OversizeBody,
            check: |error, path| {
                assert!(error.to_string().contains("byte limit"));
                assert!(std::fs::metadata(path).unwrap().len() <= 8);
            },
        },
        Case {
            scenario: "redirect rejection",
            response: Reply::Redirect,
            check: |error, path| {
                assert!(error.to_string().contains("302"));
                assert!(!path.exists());
            },
        },
    ] {
        tokio::time::timeout(Duration::from_secs(8), async {
            let fixture = Fixture::start(case.response).await;
            let mut delivery = fixture.delivery();
            delivery.limits.timeout = Duration::from_secs(2);
            delivery.limits.max_bytes = 8;
            let error = delivery
                .resolve(
                    "http://127.0.0.1:9/image.bin",
                    fixture.directory.path(),
                    CoreArtifactRole::Image,
                )
                .await
                .unwrap_err();
            (case.check)(error, &fixture.directory.path().join("image.bin"));
            assert_eq!(
                fixture.requests.lock().unwrap().len(),
                1,
                "{}",
                case.scenario
            );
            fixture.finish().await;
        })
        .await
        .expect(case.scenario);
    }
}

/// Validate wire/runtime settings before admission without opening credential files.
/// Nondefault values must survive both the local protocol and reconstruction of
/// the shared transport, including subsecond durations and connector settings.
#[test]
fn transport_snapshot_preserves_resolved_client_settings() {
    let wire = WireConfig {
        core_url: "https://core.test:8443".into(),
        root_ca_path: "/identity/ca.pem".into(),
        client_cert_path: "/identity/cert.pem".into(),
        client_key_path: "/identity/key.pem".into(),
        enforce_tls: true,
        require_client_cert: false,
        use_mgmt_vrf: true,
        request_timeout: Some(Duration::new(17, 123).try_into().unwrap()),
        socks_proxy: Some("127.0.0.1:1080".into()),
        connect_retries_max: Some(7),
        connect_retries_interval: Some(Duration::new(3, 456).try_into().unwrap()),
    };
    let snapshot = CoreFirmwareDownloadConfig::try_from(wire.clone()).unwrap();
    let client = snapshot.client_config();
    assert_eq!(snapshot.core_url, wire.core_url);
    assert_eq!(client.root_ca_path, wire.root_ca_path);
    let certificate = client.client_cert.unwrap();
    assert_eq!(certificate.cert_path, wire.client_cert_path);
    assert_eq!(certificate.key_path, wire.client_key_path);
    assert_eq!(client.enforce_tls, wire.enforce_tls);
    assert_eq!(client.require_client_cert, wire.require_client_cert);
    assert_eq!(client.use_mgmt_vrf, wire.use_mgmt_vrf);
    assert_eq!(client.request_timeout, Some(Duration::new(17, 123)));
    assert_eq!(client.socks_proxy, wire.socks_proxy);
    assert_eq!(client.connect_retries_max, wire.connect_retries_max);
    assert_eq!(client.connect_retries_interval, Some(Duration::new(3, 456)));
    assert_eq!(WireConfig::try_from(snapshot).unwrap(), wire);
}

/// Invalid origins or paths reject before opening files or admitting work.
#[test]
fn transport_config_rejects_untrusted_origins_and_relative_identity_paths() {
    type Edit = fn(&mut CoreFirmwareDownloadConfig);
    check_values(
        [
            Check {
                scenario: "verified origin",
                input: (|_| {}) as Edit,
                expect: true,
            },
            Check {
                scenario: "plaintext origin",
                input: |config| config.core_url = "http://core.test".into(),
                expect: false,
            },
            Check {
                scenario: "origin credentials",
                input: |config| config.core_url = "https://user:secret@core.test".into(),
                expect: false,
            },
            Check {
                scenario: "origin path",
                input: |config| config.core_url = "https://core.test/other".into(),
                expect: false,
            },
            Check {
                scenario: "relative credential path",
                input: |config| config.client_key_path = "key.pem".into(),
                expect: false,
            },
            Check {
                scenario: "insecure TLS override",
                input: |config| config.enforce_tls = false,
                expect: false,
            },
        ],
        |edit| {
            let mut config = CoreFirmwareDownloadConfig {
                core_url: "https://core.test".into(),
                root_ca_path: "/ca.pem".into(),
                client_cert_path: "/cert.pem".into(),
                client_key_path: "/key.pem".into(),
                enforce_tls: true,
                require_client_cert: true,
                use_mgmt_vrf: false,
                request_timeout: None,
                socks_proxy: None,
                connect_retries_max: None,
                connect_retries_interval: None,
            };
            edit(&mut config);
            config.validate().is_ok()
        },
    );
}
