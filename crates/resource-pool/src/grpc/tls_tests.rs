// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;
use std::fs::{self, File, FileTimes};
use std::net::{Ipv6Addr, SocketAddr};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use carbide_resource_pool_api::v1::DescribeRequest;
use carbide_resource_pool_api::v1::resource_pool_server::ResourcePoolServer;
use carbide_test_support::Outcome::Fails;
#[cfg(unix)]
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use rcgen::{CertificateParams, Issuer, KeyPair};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
use tonic::transport::{Certificate, Identity, ServerTlsConfig};
use tonic::{Request, Status};
use url::Url;

use super::tests::grpc_timeout;
use super::tests::server::{
    Fault, FaultProvider, Gate, TEST_TIMEOUT, TestServer, bounded, pool, provider,
};
use super::{
    CredentialFile, GrpcResourcePoolBackend, GrpcResourcePoolError, ResourcePoolTlsConfig,
};

const PRIVATE_MARKER: &str = "resource-pool-private-key-must-not-appear";
// Allow scheduling delay while still rejecting a restarted 200 ms phase.
const TIMER_TOLERANCE: Duration = Duration::from_millis(100);

struct Authority {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

impl Authority {
    fn new() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = KeyPair::generate().expect("generate authority key");
        let certificate = params
            .self_signed(&key)
            .expect("sign authority certificate");
        Self {
            pem: certificate.pem(),
            issuer: Issuer::new(params, key),
        }
    }

    fn issue(&self, name: &str) -> TestIdentity {
        let params = CertificateParams::new(vec![name.to_owned()]).expect("certificate names");
        let key = KeyPair::generate().expect("generate identity key");
        let certificate = params
            .signed_by(&key, &self.issuer)
            .expect("sign identity certificate");
        TestIdentity {
            certificate: certificate.pem(),
            key: key.serialize_pem(),
            der: certificate.der().to_vec(),
        }
    }
}

struct TestIdentity {
    certificate: String,
    key: String,
    der: Vec<u8>,
}

fn server_tls(identity: &TestIdentity, client_ca: Option<&Authority>) -> ServerTlsConfig {
    // Match tonic's client fallback when no process default is installed.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let tls =
        ServerTlsConfig::new().identity(Identity::from_pem(&identity.certificate, &identity.key));
    match client_ca {
        Some(authority) => tls.client_ca_root(Certificate::from_pem(&authority.pem)),
        None => tls,
    }
}

fn credential_files(
    directory: &TempDir,
    authority: &Authority,
    identity: Option<&TestIdentity>,
) -> ResourcePoolTlsConfig {
    let ca = directory.path().join("ca.pem");
    fs::write(&ca, &authority.pem).expect("write trust bundle");
    let mut tls = ResourcePoolTlsConfig {
        ca_cert_file: Some(ca),
        ..Default::default()
    };
    if let Some(identity) = identity {
        let certificate = directory.path().join("client.pem");
        let key = directory.path().join("client.key");
        fs::write(&certificate, &identity.certificate).expect("write client certificate");
        fs::write(&key, &identity.key).expect("write client key");
        tls.client_cert_file = Some(certificate);
        tls.client_key_file = Some(key);
    }
    tls
}

fn replace_file(path: &Path, contents: &str, modified: SystemTime) {
    fs::write(path, contents).expect("replace credential bytes");
    File::options()
        .write(true)
        .open(path)
        .expect("open replaced credential")
        .set_times(FileTimes::new().set_modified(modified))
        .expect("restore credential modification time");
    assert_eq!(
        fs::metadata(path)
            .expect("credential metadata")
            .modified()
            .expect("credential modification time"),
        modified
    );
}

fn description() -> DescribeRequest {
    let pool = pool();
    DescribeRequest {
        pool_alias: pool.logical_pool,
    }
}

async fn assert_description(client: &GrpcResourcePoolBackend) {
    let response = bounded(client.describe(description()))
        .await
        .expect("describe over verified TLS");
    assert_eq!(response.authority_id, pool().authority_id);
    assert_eq!(response.pool_id, pool().pool_id);
}

fn assert_redacted(error: &GrpcResourcePoolError, secret: &str) {
    let mut source: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(error) = source {
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
        source = error.source();
    }
}

// Forward TLS unchanged, or read ClientHello without replying. Closing a
// forwarded socket lets the same cached channel exercise its reconnect path.
struct HandshakeProxy {
    endpoint: Url,
    forwarding: Arc<AtomicBool>,
    disconnect: Arc<Notify>,
    blackhole: Arc<Notify>,
    blackholed: Arc<Notify>,
    accepted: watch::Receiver<usize>,
    closed: watch::Receiver<usize>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl HandshakeProxy {
    async fn start(target: Option<SocketAddr>, initial_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TLS proxy");
        let endpoint = Url::parse(&format!(
            "https://{}",
            listener.local_addr().expect("proxy address")
        ))
        .expect("proxy endpoint");
        let forwarding = Arc::new(AtomicBool::new(target.is_some()));
        let forward = forwarding.clone();
        let disconnect = Arc::new(Notify::new());
        let disconnect_connection = disconnect.clone();
        let blackhole = Arc::new(Notify::new());
        let blackhole_connection = blackhole.clone();
        let blackholed = Arc::new(Notify::new());
        let connection_blackholed = blackholed.clone();
        let shutdown = CancellationToken::new();
        let cancelled = shutdown.clone();
        let (connections, accepted) = watch::channel(0);
        let (completed, closed) = watch::channel(0);
        let task = tokio::spawn(async move {
            let serve = async {
                for count in 1.. {
                    let (mut downstream, _) = listener
                        .accept()
                        .await
                        .expect("accept TLS proxy connection");
                    connections.send_replace(count);
                    let result = match target.filter(|_| forward.load(Ordering::SeqCst)) {
                        Some(target) => {
                            let mut upstream = TcpStream::connect(target)
                                .await
                                .expect("connect TLS provider");
                            if count == 1 {
                                tokio::time::sleep(initial_delay).await;
                            }
                            tokio::select! {
                                result = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => result.map(drop),
                                () = disconnect_connection.notified() => Ok(()),
                                () = blackhole_connection.notified() => {
                                    connection_blackholed.notify_one();
                                    tokio::io::copy(&mut downstream, &mut tokio::io::sink()).await.map(drop)
                                },
                            }
                        }
                        None => tokio::io::copy(&mut downstream, &mut tokio::io::sink())
                            .await
                            .map(drop),
                    };
                    if let Err(error) = result {
                        assert!(
                            matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::BrokenPipe
                            ),
                            "proxy I/O failed: {error}"
                        );
                    }
                    drop(downstream);
                    completed.send_replace(count);
                }
            };
            tokio::select! {
                () = cancelled.cancelled() => {},
                () = serve => {},
            }
        });
        Self {
            endpoint,
            forwarding,
            disconnect,
            blackhole,
            blackholed,
            accepted,
            closed,
            shutdown,
            task,
        }
    }

    async fn wait_for_connection(&mut self, count: usize) {
        bounded(self.accepted.wait_for(|accepted| *accepted >= count))
            .await
            .expect("proxy still running");
    }

    async fn wait_for_close(&mut self, count: usize) {
        bounded(self.closed.wait_for(|closed| *closed >= count))
            .await
            .expect("proxy still running");
    }

    async fn stop(mut self) {
        self.shutdown.cancel();
        match tokio::time::timeout(TEST_TIMEOUT, &mut self.task).await {
            Ok(result) => result.expect("join TLS proxy"),
            Err(_) => {
                self.task.abort();
                assert!(self.task.await.expect_err("abort TLS proxy").is_cancelled());
                panic!("TLS proxy did not stop within {TEST_TIMEOUT:?}");
            }
        }
    }
}

#[test]
fn direct_backend_construction_rejects_invalid_settings_without_disclosure() {
    let result = GrpcResourcePoolBackend::new(
        Url::parse("https://user:endpoint-secret@provider.example").expect("provider endpoint"),
        None,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("constructor accepted endpoint credentials"),
    };
    assert!(matches!(
        error,
        GrpcResourcePoolError::InvalidConfiguration(_)
    ));
    assert_redacted(&error, "endpoint-secret");
}

#[tokio::test]
async fn constructor_defers_credential_reads_until_the_call() {
    let directory = TempDir::new().expect("credential directory");
    let client = GrpcResourcePoolBackend::new(
        Url::parse("https://127.0.0.1:1").expect("provider endpoint"),
        Some(ResourcePoolTlsConfig {
            ca_cert_file: Some(directory.path().join("missing-ca.pem")),
            ..Default::default()
        }),
    )
    .expect("construction does not read credential files");
    assert!(matches!(
        bounded(client.describe(description())).await,
        Err(GrpcResourcePoolError::Credentials {
            file: CredentialFile::CaCertificate,
            ..
        })
    ));
}

#[tokio::test]
async fn credential_sources_fail_locally_and_allow_symlinked_regular_files() {
    #[derive(Clone, Copy, Debug)]
    enum Source {
        Directory,
        Empty,
        Oversized,
        KeyOnly,
        MalformedAfterValid,
        #[cfg(unix)]
        Fifo,
        #[cfg(unix)]
        Symlink,
    }

    check_cases_async(
        [
            Case {
                scenario: "directory",
                input: Source::Directory,
                expect: Fails,
            },
            Case {
                scenario: "empty file",
                input: Source::Empty,
                expect: Fails,
            },
            Case {
                scenario: "file exceeds 256 KiB",
                input: Source::Oversized,
                expect: Fails,
            },
            Case {
                scenario: "CA file contains only a private key",
                input: Source::KeyOnly,
                expect: Fails,
            },
            Case {
                scenario: "valid CA followed by a malformed certificate",
                input: Source::MalformedAfterValid,
                expect: Fails,
            },
            #[cfg(unix)]
            Case {
                scenario: "FIFO without a writer",
                input: Source::Fifo,
                expect: Fails,
            },
            #[cfg(unix)]
            Case {
                scenario: "symlink to regular CA file",
                input: Source::Symlink,
                expect: Yields(()),
            },
        ],
        |source| async move {
            let authority = Authority::new();
            let identity = authority.issue("127.0.0.1");
            let server = TestServer::start_tls(provider(), server_tls(&identity, None)).await;
            let directory = TempDir::new().expect("credential directory");
            let path = directory.path().join("ca.pem");
            match source {
                Source::Directory => fs::create_dir(&path).expect("create credential directory"),
                Source::Empty => fs::write(&path, []).expect("write empty credential"),
                Source::Oversized => fs::write(&path, vec![b' '; 256 * 1024 + 1])
                    .expect("write oversized credential"),
                Source::KeyOnly => fs::write(&path, &identity.key).expect("write key as CA"),
                Source::MalformedAfterValid => fs::write(
                    &path,
                    format!(
                        "{}-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----\n",
                        authority.pem
                    ),
                )
                .expect("write partially malformed CA bundle"),
                #[cfg(unix)]
                Source::Fifo => nix::unistd::mkfifo(
                    &path,
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .expect("create credential FIFO"),
                #[cfg(unix)]
                Source::Symlink => {
                    let target = directory.path().join("ca-target.pem");
                    fs::write(&target, &authority.pem).expect("write CA target");
                    std::os::unix::fs::symlink(target, &path).expect("link CA credential");
                }
            }
            let client = GrpcResourcePoolBackend::new(
                server.endpoint.clone(),
                Some(ResourcePoolTlsConfig {
                    ca_cert_file: Some(path.clone()),
                    ..Default::default()
                }),
            )
            .expect("construct client before credential validation");
            let mut call = Box::pin(client.describe(description()));
            let result = match tokio::time::timeout(TEST_TIMEOUT, &mut call).await {
                Ok(result) => result,
                Err(_) => {
                    #[cfg(unix)]
                    if matches!(source, Source::Fifo) {
                        // A blocking FIFO open survives future cancellation.
                        // Release it before failing so runtime shutdown finishes.
                        let writer = File::options()
                            .read(true)
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&path)
                            .expect("release blocked FIFO reader");
                        let _result = bounded(&mut call).await;
                        drop(writer);
                    }
                    panic!("credential validation exceeded {TEST_TIMEOUT:?}");
                }
            };
            drop(call);
            match &result {
                Ok(response) => {
                    assert_eq!(response.pool_id, pool().pool_id);
                    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);
                }
                Err(error) => {
                    assert!(
                        matches!(
                            error,
                            GrpcResourcePoolError::Credentials {
                                file: CredentialFile::CaCertificate,
                                ..
                            }
                        ),
                        "unexpected credential failure: {error:?}"
                    );
                    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 0);
                    assert_redacted(error, &identity.key);
                    assert_redacted(error, path.to_str().expect("credential path"));
                }
            }
            drop(client);
            server.stop().await;
            result.map(drop).map_err(drop)
        },
    )
    .await;
}

#[tokio::test]
async fn tls_rejects_untrusted_peers_and_wrong_server_names() {
    #[derive(Clone, Copy, Debug)]
    enum Rejection {
        ServerAuthority,
        ServerName,
        MissingClient,
        ClientAuthority,
    }

    check_cases_async(
        [
            Case {
                scenario: "server CA is not trusted",
                input: Rejection::ServerAuthority,
                expect: Fails,
            },
            Case {
                scenario: "server certificate names another host",
                input: Rejection::ServerName,
                expect: Fails,
            },
            Case {
                scenario: "server requires a client certificate",
                input: Rejection::MissingClient,
                expect: Fails,
            },
            Case {
                scenario: "client CA is not trusted",
                input: Rejection::ClientAuthority,
                expect: Fails,
            },
        ],
        |rejection| async move {
            let authority = Authority::new();
            let unrelated = Authority::new();
            let name = match rejection {
                Rejection::ServerName => "another-provider.example",
                _ => "127.0.0.1",
            };
            let server_identity = authority.issue(name);
            let client_identity = match rejection {
                Rejection::ClientAuthority => unrelated.issue("client"),
                _ => authority.issue("client"),
            };
            let received = Arc::new(AtomicUsize::new(0));
            let requests = received.clone();
            let service =
                ResourcePoolServer::with_interceptor(provider(), move |request: Request<()>| {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Status>(request)
                });
            let client_ca = Some(&authority);
            let server =
                TestServer::start_service(service, Some(server_tls(&server_identity, client_ca)))
                    .await;
            let directory = TempDir::new().expect("credential directory");
            let trust = match rejection {
                Rejection::ServerAuthority => &unrelated,
                _ => &authority,
            };
            let identity = match rejection {
                Rejection::MissingClient => None,
                _ => Some(&client_identity),
            };
            let tls = Some(credential_files(&directory, trust, identity));
            let client = GrpcResourcePoolBackend::new(server.endpoint.clone(), tls)
                .expect("valid TLS source configuration");
            let result = bounded(client.describe(description())).await;
            assert!(
                matches!(
                    &result,
                    Err(GrpcResourcePoolError::Connect(_) | GrpcResourcePoolError::Rpc(_))
                ),
                "TLS peer rejection must fail at connection or RPC setup: {result:?}"
            );
            assert_eq!(received.load(Ordering::SeqCst), 0);
            assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);
            drop(client);
            server.stop().await;
            result.map(drop).map_err(drop)
        },
    )
    .await;
}

#[tokio::test]
async fn tls_verifies_an_ipv6_literal_authority_against_its_ip_certificate() {
    let authority = Authority::new();
    let identity = authority.issue("::1");
    let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
        .await
        .expect("bind IPv6 loopback provider");
    let server = TestServer::start_service_with_listener(
        listener,
        ResourcePoolServer::new(provider()),
        Some(server_tls(&identity, None)),
    );
    let directory = TempDir::new().expect("credential directory");
    let client = GrpcResourcePoolBackend::new(
        server.endpoint.clone(),
        Some(credential_files(&directory, &authority, None)),
    )
    .expect("IPv6 TLS backend");
    assert_description(&client).await;
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn mtls_reuses_connections_and_reloads_identity_bytes_without_newer_timestamps() {
    let authority = Authority::new();
    let server_identity = authority.issue("127.0.0.1");
    let first_identity = authority.issue("first-client");
    let second_identity = authority.issue("second-client");
    let recovered_identity = authority.issue("recovered-client");
    let gate = Arc::new(Gate::default());
    let stalled = FaultProvider::new(provider(), Fault::StallDescription(gate.clone()));
    let received = Arc::new(Mutex::new(Vec::new()));
    let certificates = received.clone();
    let service = ResourcePoolServer::with_interceptor(stalled, move |request: Request<()>| {
        let peer = request
            .extensions()
            .get::<TlsConnectInfo<TcpConnectInfo>>()
            .expect("TLS connection information")
            .peer_certs()
            .expect("authenticated client certificate");
        certificates
            .lock()
            .expect("received certificates lock")
            .push(peer[0].to_vec());
        Ok::<_, Status>(request)
    });
    let server = TestServer::start_service(
        service,
        Some(server_tls(&server_identity, Some(&authority))),
    )
    .await;
    let directory = TempDir::new().expect("credential directory");
    let tls = credential_files(&directory, &authority, Some(&first_identity));
    let certificate_path = tls.client_cert_file.as_ref().expect("certificate path");
    let key_path = tls.client_key_file.as_ref().expect("key path");
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    replace_file(certificate_path, &first_identity.certificate, modified);
    replace_file(key_path, &first_identity.key, modified);
    let client = GrpcResourcePoolBackend::new(server.endpoint.clone(), Some(tls.clone()))
        .expect("create mTLS backend");

    gate.release.notify_one();
    assert_description(&client).await;
    bounded(gate.entered.notified()).await;
    let old_client = client.clone();
    let mut old_call = Box::pin(old_client.describe(description()));
    tokio::select! {
        result = &mut old_call => panic!("old request passed its gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {},
    }
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);

    // Both paths and mtimes stay the same. The certificate received by the
    // provider proves which identity the replacement connection presented.
    replace_file(certificate_path, &second_identity.certificate, modified);
    replace_file(key_path, &second_identity.key, modified);
    let mut new_call = Box::pin(client.describe(description()));
    tokio::select! {
        result = &mut new_call => panic!("new request passed its gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {},
    }
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 2);
    assert_eq!(
        *received.lock().expect("received certificates lock"),
        vec![
            first_identity.der.clone(),
            first_identity.der.clone(),
            second_identity.der,
        ]
    );
    // The old request stays in flight while the new connection authenticates.
    // Replacing the cached channel must preserve both callers' connections.
    gate.release.notify_waiters();
    let (old_response, new_response) = bounded(async { tokio::join!(old_call, new_call) }).await;
    for response in [old_response, new_response] {
        assert_eq!(
            response
                .expect("request survives identity replacement")
                .pool_id,
            pool().pool_id
        );
    }
    drop(old_client);

    #[derive(Clone, Copy, Debug)]
    enum InvalidIdentity {
        Malformed,
        Mismatched,
        MissingKey,
        MissingCertificate,
    }

    check_cases_async(
        [
            Case {
                scenario: "malformed replacement key",
                input: InvalidIdentity::Malformed,
                expect: Fails,
            },
            Case {
                scenario: "replacement key does not match certificate",
                input: InvalidIdentity::Mismatched,
                expect: Fails,
            },
            Case {
                scenario: "replacement key is unavailable",
                input: InvalidIdentity::MissingKey,
                expect: Fails,
            },
            Case {
                scenario: "replacement certificate is unavailable",
                input: InvalidIdentity::MissingCertificate,
                expect: Fails,
            },
        ],
        |replacement| {
            let client = &client;
            let original_key = &first_identity.key;
            let current_key = &second_identity.key;
            let received = &received;
            async move {
                match replacement {
                    InvalidIdentity::Malformed => replace_file(
                        key_path,
                        &format!(
                            "-----BEGIN PRIVATE KEY-----\n{PRIVATE_MARKER}\n-----END PRIVATE KEY-----"
                        ),
                        modified,
                    ),
                    InvalidIdentity::Mismatched => replace_file(key_path, original_key, modified),
                    InvalidIdentity::MissingKey => fs::remove_file(key_path).expect("remove client key"),
                    InvalidIdentity::MissingCertificate => {
                        replace_file(key_path, current_key, modified);
                        fs::remove_file(certificate_path).expect("remove client certificate");
                    }
                }
                let result = bounded(client.describe(description())).await;
                if let Err(error) = &result {
                    match replacement {
                        InvalidIdentity::Malformed | InvalidIdentity::Mismatched => {
                            assert!(matches!(error, GrpcResourcePoolError::Tls(_)));
                        }
                        InvalidIdentity::MissingKey | InvalidIdentity::MissingCertificate => {
                            let expected_file = match replacement {
                                InvalidIdentity::MissingKey => CredentialFile::ClientKey,
                                _ => CredentialFile::ClientCertificate,
                            };
                            assert!(matches!(error, GrpcResourcePoolError::Credentials { file, source }
                                if *file == expected_file && source.kind() == std::io::ErrorKind::NotFound));
                        }
                    }
                    assert_redacted(error, PRIVATE_MARKER);
                    assert_redacted(error, original_key);
                }
                assert_eq!(
                    received.lock().expect("received certificates lock").len(),
                    3
                );
                result.map(drop).map_err(drop)
            }
        },
    )
    .await;

    let older = modified - Duration::from_secs(1);
    replace_file(certificate_path, &recovered_identity.certificate, older);
    replace_file(key_path, &recovered_identity.key, older);
    gate.release.notify_one();
    assert_description(&client).await;
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 3);
    assert_eq!(
        received.lock().expect("received certificates lock").last(),
        Some(&recovered_identity.der)
    );
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn replacing_ca_bytes_changes_trust_and_recovers_with_an_older_timestamp() {
    let authority = Authority::new();
    let unrelated = Authority::new();
    let identity = authority.issue("127.0.0.1");
    let server = TestServer::start_tls(provider(), server_tls(&identity, None)).await;
    let directory = TempDir::new().expect("credential directory");
    let tls = credential_files(&directory, &authority, None);
    let ca_path = tls.ca_cert_file.as_ref().expect("CA path");
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    replace_file(ca_path, &authority.pem, modified);
    #[cfg(unix)]
    let tls = {
        let path = directory.path().join("current-ca.pem");
        std::os::unix::fs::symlink(ca_path, &path).expect("link initial CA target");
        ResourcePoolTlsConfig {
            ca_cert_file: Some(path),
            ..tls
        }
    };
    let configured_ca = tls.ca_cert_file.as_ref().expect("configured CA path");
    let replace_ca = |name: &str, contents: &str, modified| {
        let target = directory.path().join(name);
        replace_file(&target, contents, modified);
        #[cfg(unix)]
        {
            // Replace the symlink atomically while preserving each target's
            // timestamp. Every call must follow the configured path again.
            let next = directory.path().join("next-ca.pem");
            std::os::unix::fs::symlink(target, &next).expect("link replacement CA target");
            fs::rename(next, configured_ca).expect("replace CA symlink atomically");
        }
        #[cfg(not(unix))]
        fs::rename(target, configured_ca).expect("replace CA file atomically");
    };
    let client = GrpcResourcePoolBackend::new(server.endpoint.clone(), Some(tls.clone()))
        .expect("create TLS backend without client identity");
    assert_description(&client).await;
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);

    replace_ca("untrusted-ca.pem", &unrelated.pem, modified);
    assert!(matches!(
        bounded(client.describe(description())).await,
        Err(GrpcResourcePoolError::Connect(_))
    ));
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 2);

    // Correct the trust bundle with different bytes, ensuring recovery must
    // establish a new connection instead of matching the original snapshot.
    let bundle = format!("{}{}", authority.pem, unrelated.pem);
    replace_ca(
        "corrected-ca.pem",
        &bundle,
        modified - Duration::from_secs(1),
    );
    assert_description(&client).await;
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 3);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn a_stalled_tls_handshake_obeys_the_connect_deadline_without_retrying() {
    let authority = Authority::new();
    let directory = TempDir::new().expect("credential directory");
    let proxy = HandshakeProxy::start(None, Duration::ZERO).await;
    let connect_budget = Duration::from_millis(200);
    let client = GrpcResourcePoolBackend::new(
        proxy.endpoint.clone(),
        Some(credential_files(&directory, &authority, None)),
    )
    .expect("stalled TLS backend")
    .with_timeouts(connect_budget, TEST_TIMEOUT);

    let started = Instant::now();
    let result = tokio::time::timeout(
        connect_budget + TIMER_TOLERANCE,
        client.describe(description()),
    )
    .await
    .expect("TLS handshake ended within the connection budget and scheduling tolerance");
    assert!(matches!(
        result,
        Err(GrpcResourcePoolError::DeadlineExceeded)
    ));
    assert!(started.elapsed() >= connect_budget);
    assert_eq!(*proxy.accepted.borrow(), 1);
    drop(client);
    proxy.stop().await;
}

#[tokio::test]
async fn the_call_budget_includes_refresh_lock_wait_and_the_following_handshake() {
    let authority = Authority::new();
    let directory = TempDir::new().expect("credential directory");
    let mut proxy = HandshakeProxy::start(None, Duration::ZERO).await;
    let connect_budget = Duration::from_secs(2);
    let call_budget = Duration::from_millis(400);
    let prerequisite_delay = Duration::from_millis(200);
    let client = GrpcResourcePoolBackend::new(
        proxy.endpoint.clone(),
        Some(credential_files(&directory, &authority, None)),
    )
    .expect("stalled TLS backend")
    .with_timeouts(connect_budget, connect_budget);
    let lock_holder = client.clone();
    let first_call = tokio::spawn(async move { lock_holder.describe(description()).await });
    proxy.wait_for_connection(1).await;

    let client = client.with_timeouts(connect_budget, call_budget);
    let started = Instant::now();
    let mut waiting_call = Box::pin(client.describe(description()));
    tokio::select! {
        result = &mut waiting_call => panic!("call finished while refresh was locked: {result:?}"),
        () = tokio::time::sleep(prerequisite_delay) => {},
    }
    assert_eq!(
        *proxy.accepted.borrow(),
        1,
        "second call must still be waiting for refresh"
    );
    first_call.abort();
    assert!(
        bounded(first_call)
            .await
            .expect_err("cancel lock holder")
            .is_cancelled()
    );

    // Cancellation releases the refresh lock. The second call now connects,
    // but the time it already spent waiting remains charged to its deadline.
    let result =
        tokio::time::timeout_at(started + call_budget + TIMER_TOLERANCE, &mut waiting_call)
            .await
            .expect("call deadline includes the prerequisite wait");
    assert!(matches!(
        result,
        Err(GrpcResourcePoolError::DeadlineExceeded)
    ));
    assert!(started.elapsed() >= call_budget);
    assert_eq!(*proxy.accepted.borrow(), 2);
    drop(waiting_call);
    drop(client);
    proxy.stop().await;
}

#[tokio::test]
async fn a_cached_channel_bounds_a_stalled_reconnect_and_recovers_with_unchanged_credentials() {
    let authority = Authority::new();
    let server_identity = authority.issue("127.0.0.1");
    let server = TestServer::start_tls(provider(), server_tls(&server_identity, None)).await;
    let target = format!(
        "127.0.0.1:{}",
        server.endpoint.port().expect("provider port")
    )
    .parse()
    .expect("provider socket address");
    let mut proxy = HandshakeProxy::start(Some(target), Duration::ZERO).await;
    let directory = TempDir::new().expect("credential directory");
    let connect_budget = Duration::from_millis(200);
    let client = GrpcResourcePoolBackend::new(
        proxy.endpoint.clone(),
        Some(credential_files(&directory, &authority, None)),
    )
    .expect("TLS backend through proxy")
    .with_timeouts(connect_budget, TEST_TIMEOUT);
    assert_description(&client).await;
    proxy.forwarding.store(false, Ordering::SeqCst);
    proxy.disconnect.notify_one();
    proxy.wait_for_close(1).await;

    let started = Instant::now();
    let result = tokio::time::timeout(connect_budget + TIMER_TOLERANCE, async {
        let result = client.describe(description()).await;
        if *proxy.accepted.borrow() == 1 {
            // The cached HTTP/2 connection may report its close before tonic
            // starts reconnecting. This is a separate caller attempt.
            assert!(matches!(result, Err(GrpcResourcePoolError::Rpc(_))));
            client.describe(description()).await
        } else {
            result
        }
    })
    .await
    .expect("cached reconnect obeyed the connection budget");
    assert!(
        matches!(result, Err(GrpcResourcePoolError::DeadlineExceeded)),
        "unexpected reconnect result: {result:?}"
    );
    assert_eq!(*proxy.accepted.borrow(), 2);
    assert!(started.elapsed() >= connect_budget);
    proxy.wait_for_close(2).await;

    proxy.forwarding.store(true, Ordering::SeqCst);
    assert_description(&client).await;
    assert_eq!(*proxy.accepted.borrow(), 3);
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
}

#[tokio::test]
async fn keepalive_closes_an_established_blackhole_before_the_next_call_recovers() {
    let authority = Authority::new();
    let server_identity = authority.issue("127.0.0.1");
    let received_timeout = Arc::new(Mutex::new(None));
    let timeout_header = received_timeout.clone();
    let service = ResourcePoolServer::with_interceptor(provider(), move |request: Request<()>| {
        let timeout = request
            .metadata()
            .get("grpc-timeout")
            .expect("request advertises remaining budget")
            .to_str()
            .expect("ASCII timeout header");
        *timeout_header.lock().expect("timeout header lock") = Some(grpc_timeout(timeout));
        Ok::<_, Status>(request)
    });
    let server = TestServer::start_service(service, Some(server_tls(&server_identity, None))).await;
    let target = format!(
        "127.0.0.1:{}",
        server.endpoint.port().expect("provider port")
    )
    .parse()
    .expect("provider socket address");
    let handshake_delay = Duration::from_millis(100);
    let mut proxy = HandshakeProxy::start(Some(target), handshake_delay).await;
    let directory = TempDir::new().expect("credential directory");
    let connect_budget = Duration::from_millis(200);
    let call_budget = Duration::from_millis(400);
    let client = GrpcResourcePoolBackend::new(
        proxy.endpoint.clone(),
        Some(credential_files(&directory, &authority, None)),
    )
    .expect("TLS backend through proxy")
    .with_timeouts(connect_budget, call_budget);
    assert_description(&client).await;
    let advertised = received_timeout
        .lock()
        .expect("timeout header lock")
        .take()
        .expect("first request timeout");
    assert!(
        advertised <= call_budget - handshake_delay / 2,
        "TLS establishment must reduce the advertised budget: {advertised:?}"
    );
    // Age the last received frame so the keepalive ping is sent while the
    // following call is active, before its own deadline ends the stream.
    tokio::time::sleep(call_budget / 4).await;
    let started = Instant::now();
    proxy.blackhole.notify_one();
    bounded(proxy.blackholed.notified()).await;

    let result = tokio::time::timeout(
        call_budget + TIMER_TOLERANCE,
        client.describe(description()),
    )
    .await
    .expect("call to established blackhole exceeded its budget");
    assert!(matches!(
        result,
        Err(GrpcResourcePoolError::DeadlineExceeded)
    ));
    // Once sent during the call, the unanswered keepalive ping must expire
    // even after the call has ended and the connection is idle again.
    tokio::time::timeout_at(
        started + call_budget + connect_budget + TIMER_TOLERANCE,
        proxy.wait_for_close(1),
    )
    .await
    .expect("keepalive did not close the established blackhole");
    assert_description(&client).await;
    assert_eq!(*proxy.accepted.borrow(), 2);
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
}
