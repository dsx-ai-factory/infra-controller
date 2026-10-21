// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;
use std::fs;
use std::io::ErrorKind;

use carbide_test_support::Outcome::{FailsWith, Yields};
use carbide_test_support::{Case, check_cases};

use super::{CredentialFile, GrpcResourcePoolError, MAX_PEM_BYTES, is_timeout, read_pem};
use crate::grpc::PreparationError;

#[test]
fn timeout_detection_requires_a_local_source() {
    let local = tonic::Status::from_error(Box::new(tonic::TimeoutExpired(())));
    // A peer can copy the timeout's code and message without its local source.
    let remote = tonic::Status::cancelled(local.message());
    assert_eq!(remote.code(), local.code());
    assert!(!is_timeout(&remote));
    assert!(is_timeout(&local));
}

#[test]
fn credential_reads_preserve_absence_and_enforce_file_bounds() {
    let directory = tempfile::tempdir().expect("create credential directory");
    let maximum = usize::try_from(MAX_PEM_BYTES).expect("credential limit fits usize");
    for (name, size) in [
        ("empty", 0),
        ("one-byte", 1),
        ("maximum", maximum),
        ("oversized", maximum + 1),
    ] {
        fs::write(directory.path().join(name), vec![b'x'; size]).expect("write credential fixture");
    }
    check_cases(
        [
            Case {
                scenario: "absent path has no credential",
                input: None,
                expect: Yields(None),
            },
            Case {
                scenario: "empty file is invalid",
                input: Some(directory.path().join("empty")),
                expect: FailsWith(ErrorKind::InvalidData),
            },
            Case {
                scenario: "one byte is accepted",
                input: Some(directory.path().join("one-byte")),
                expect: Yields(Some(1)),
            },
            Case {
                scenario: "exact size limit is accepted",
                input: Some(directory.path().join("maximum")),
                expect: Yields(Some(maximum)),
            },
            Case {
                scenario: "one byte above the limit is invalid",
                input: Some(directory.path().join("oversized")),
                expect: FailsWith(ErrorKind::InvalidData),
            },
            #[cfg(unix)]
            Case {
                scenario: "directory is not a regular credential file",
                input: Some(directory.path().to_owned()),
                expect: FailsWith(ErrorKind::InvalidInput),
            },
        ],
        |path| {
            read_pem(path.as_deref(), CredentialFile::ClientKey)
                .map(|bytes| {
                    bytes.map(|bytes| {
                        assert!(bytes.iter().all(|byte| *byte == b'x'));
                        bytes.len()
                    })
                })
                .map_err(|error| match error {
                    GrpcResourcePoolError::Credentials { file, source } => {
                        assert_eq!(file, CredentialFile::ClientKey);
                        source.kind()
                    }
                    other => panic!("expected credential error, got {other:?}"),
                })
        },
    );
}

#[tokio::test]
async fn preparation_errors_hide_panic_payloads_but_preserve_explicit_diagnostics() {
    const PRIVATE_MARKER: &str = "private-resource-pool-preparation-payload";
    let join_error = tokio::spawn(async { panic!("{PRIVATE_MARKER}") })
        .await
        .expect_err("join panicking preparation fixture");
    assert!(format!("{join_error:?}").contains(PRIVATE_MARKER));
    let preparation = PreparationError::from(join_error);
    assert!(preparation.join_error().is_panic());
    assert!(
        preparation
            .join_error()
            .to_string()
            .contains(PRIVATE_MARKER)
    );
    let error = GrpcResourcePoolError::Preparation(preparation);
    let mut source: Option<&(dyn Error + 'static)> = Some(&error);
    while let Some(error) = source {
        assert!(!error.to_string().contains(PRIVATE_MARKER));
        assert!(!format!("{error:?}").contains(PRIVATE_MARKER));
        source = error.source();
    }
}

#[cfg(unix)]
mod native_roots {
    use std::fs::{self, File, OpenOptions};
    use std::future::{Future, poll_fn};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::{Duration, Instant};

    use carbide_resource_pool_api::v1::{DescribeRequest, DescribeResponse};
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;
    use rcgen::{CertificateParams, Issuer, KeyPair};
    use tonic::transport::{Identity, ServerTlsConfig};

    use super::super::Transport;
    use crate::grpc::tests::server::{TEST_TIMEOUT, TestServer, bounded, pool, provider};
    use crate::grpc::{GrpcResourcePoolError, ResourcePoolTlsConfig};

    const CHILD_DIRECTORY: &str = "RESOURCE_POOL_NATIVE_ROOT_TEST_DIRECTORY";
    const CALL_TIMEOUT: Duration = Duration::from_millis(200);
    // Reject a restarted 200 ms budget while allowing local scheduling delay.
    const TIMER_TOLERANCE: Duration = Duration::from_millis(150);
    const CHILD_TIMEOUT: Duration = Duration::from_secs(15);

    struct ChildProcess(Child);

    impl Drop for ChildProcess {
        fn drop(&mut self) {
            // Also reap the child if an assertion fails before normal completion.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn blocked_native_roots_keep_refresh_owned_after_deadline_and_cancellation() {
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY).map(PathBuf::from) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("create native-root test runtime");
            runtime.block_on(exercise_blocked_roots(&directory));
            runtime.shutdown_timeout(Duration::from_secs(1));
            fs::write(directory.join("completed"), b"complete").expect("record child completion");
            return;
        }
        let directory = tempfile::tempdir().expect("create native root test directory");
        let roots = directory.path().join("roots.pem");
        mkfifo(&roots, Mode::S_IRUSR | Mode::S_IWUSR).expect("create native root FIFO");
        let empty_roots = directory.path().join("empty-roots");
        fs::create_dir(&empty_roots).expect("create empty native root directory");
        let (_, module) = module_path!()
            .split_once("::")
            .expect("test module follows the crate name");
        // Native-root settings are process-wide. A child also lets this watchdog
        // terminate a regression that blocks the sole runtime thread or shutdown.
        let mut child = ChildProcess(
            Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    &format!(
                        "{module}::blocked_native_roots_keep_refresh_owned_after_deadline_and_cancellation"
                    ),
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_DIRECTORY, directory.path())
                .env("SSL_CERT_FILE", &roots)
                .env("SSL_CERT_DIR", &empty_roots)
                .spawn()
                .expect("spawn native-root test child"),
        );
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("poll native-root child") {
                break status;
            }
            assert!(
                started.elapsed() < CHILD_TIMEOUT,
                "native-root test child exceeded {CHILD_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "native-root test child failed: {status}");
        assert_eq!(
            fs::read(directory.path().join("completed")).expect("child completed its assertions"),
            b"complete"
        );
    }

    async fn exercise_blocked_roots(directory: &Path) {
        enum CallerExit {
            Deadline,
            Cancelled,
        }

        let mut authority_params = CertificateParams::default();
        authority_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let authority_key = KeyPair::generate().expect("generate authority key");
        let authority = authority_params
            .self_signed(&authority_key)
            .expect("sign authority certificate");
        let issuer = Issuer::new(authority_params.clone(), authority_key);
        let server_key = KeyPair::generate().expect("generate server key");
        let server_certificate = CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .expect("server certificate address")
            .signed_by(&server_key, &issuer)
            .expect("sign server certificate");
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let server = TestServer::start_tls(
            provider(),
            ServerTlsConfig::new().identity(Identity::from_pem(
                server_certificate.pem(),
                server_key.serialize_pem(),
            )),
        )
        .await;
        let transport = Transport::new(server.endpoint.clone(), None)
            .expect("configure provider using native roots")
            .with_timeouts(TEST_TIMEOUT, CALL_TIMEOUT);
        let rpc_calls = Arc::new(AtomicUsize::new(0));
        let roots = directory.join("roots.pem");

        for caller_exit in [CallerExit::Deadline, CallerExit::Cancelled] {
            let started = Instant::now();
            let first = tokio::spawn({
                let transport = transport.clone();
                let rpc_calls = rpc_calls.clone();
                async move { describe(&transport, &rpc_calls).await }
            });
            // A nonblocking writer can open only after the native loader opens
            // its reader. Keep it empty so the actual PEM read remains blocked.
            let mut writer = open_fifo_writer(&roots).await;
            match caller_exit {
                CallerExit::Cancelled => {
                    first.abort();
                    assert!(
                        first
                            .await
                            .expect_err("cancel initial caller")
                            .is_cancelled()
                    );
                }
                CallerExit::Deadline => {
                    assert!(matches!(
                        bounded(first).await.expect("join initial caller"),
                        Err(GrpcResourcePoolError::DeadlineExceeded)
                    ));
                    assert_call_duration(started);
                }
            }
            assert!(transport.inner.cached.try_lock().is_err());

            // Dropping a waiting call and allowing another to expire must leave
            // the original worker holding refresh ownership until its read ends.
            let mut cancelled = Box::pin(describe(&transport, &rpc_calls));
            assert!(
                poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            drop(cancelled);
            let started = Instant::now();
            assert!(matches!(
                bounded(describe(&transport, &rpc_calls)).await,
                Err(GrpcResourcePoolError::DeadlineExceeded)
            ));
            assert_call_duration(started);
            assert!(transport.inner.cached.try_lock().is_err());
            assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 0);
            assert_eq!(rpc_calls.load(Ordering::SeqCst), 0);

            writer
                .write_all(authority.pem().as_bytes())
                .expect("release native root reader with valid trust");
            drop(writer);
            let cached = bounded(transport.inner.cached.lock()).await;
            assert!(
                cached.is_none(),
                "abandoned preparation must not publish a client"
            );
            drop(cached);
            assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 0);
            assert_eq!(rpc_calls.load(Ordering::SeqCst), 0);
            fs::remove_file(&roots).expect("remove released native root FIFO");
            if matches!(caller_exit, CallerExit::Deadline) {
                mkfifo(&roots, Mode::S_IRUSR | Mode::S_IWUSR)
                    .expect("create FIFO for caller cancellation");
            }
        }

        // Keep the same transport and native-root path; a regular CA file lets
        // a later caller rebuild after both abandoned preparations have finished.
        fs::write(&roots, authority.pem()).expect("replace FIFO with native trust bundle");
        let transport = transport.with_timeouts(TEST_TIMEOUT, TEST_TIMEOUT);
        let response = bounded(describe(&transport, &rpc_calls))
            .await
            .expect("recover using native roots");
        assert_eq!(response.authority_id, pool().authority_id);
        assert_eq!(response.pool_id, pool().pool_id);
        assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);
        assert_eq!(rpc_calls.load(Ordering::SeqCst), 1);
        assert!(
            transport
                .inner
                .cached
                .try_lock()
                .expect("refresh completed")
                .is_some()
        );

        let unrelated_key = KeyPair::generate().expect("generate unrelated authority key");
        let unrelated_authority = authority_params
            .self_signed(&unrelated_key)
            .expect("sign unrelated authority certificate");
        let explicit_ca = directory.join("explicit-ca.pem");
        fs::write(&explicit_ca, unrelated_authority.pem()).expect("write explicit trust bundle");
        let explicit = Transport::new(
            server.endpoint.clone(),
            Some(ResourcePoolTlsConfig {
                ca_cert_file: Some(explicit_ca),
                ..Default::default()
            }),
        )
        .expect("configure explicit trust")
        .with_timeouts(TEST_TIMEOUT, TEST_TIMEOUT);
        // The native bundle trusts this server, but an explicit bundle replaces
        // it. Reusing the successful server distinguishes that policy from setup.
        assert!(matches!(
            bounded(describe(&explicit, &rpc_calls)).await,
            Err(GrpcResourcePoolError::Connect(_))
        ));
        assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 2);
        assert_eq!(rpc_calls.load(Ordering::SeqCst), 1);
        drop(explicit);
        drop(transport);
        server.stop().await;
    }

    async fn open_fifo_writer(path: &Path) -> File {
        bounded(async {
            loop {
                match OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)
                {
                    Ok(writer) => return writer,
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(error) => panic!("open native root FIFO writer: {error}"),
                }
            }
        })
        .await
    }

    async fn describe(
        transport: &Transport,
        rpc_calls: &AtomicUsize,
    ) -> Result<DescribeResponse, GrpcResourcePoolError> {
        let pool = pool();
        transport
            .call(
                "Describe",
                DescribeRequest {
                    site_id: pool.site_id,
                    pool_alias: pool.logical_pool,
                },
                |mut client, request| {
                    rpc_calls.fetch_add(1, Ordering::SeqCst);
                    async move { client.describe(request).await }
                },
            )
            .await
    }

    fn assert_call_duration(started: Instant) {
        let elapsed = started.elapsed();
        assert!(
            elapsed >= CALL_TIMEOUT && elapsed < CALL_TIMEOUT + TIMER_TOLERANCE,
            "expected {CALL_TIMEOUT:?} call budget, took {elapsed:?}"
        );
    }
}
