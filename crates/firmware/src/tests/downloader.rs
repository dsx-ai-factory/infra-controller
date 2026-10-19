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

use std::path::Path;
use std::time::Duration;

use carbide_instrument::testing::{
    ApproxHistogramSum, CapturedLog, MetricsCapture, capture_logs, capture_logs_async,
};
use carbide_instrument::{LabelValue, emit};
use carbide_test_support::{Check, check_values};
use sha2::Digest;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::downloader::*;

const ARTIFACT_UNAVAILABLE_METRIC: &str = "carbide_firmware_artifact_unavailable_total";
const DOWNLOAD_DURATION_METRIC: &str = "carbide_firmware_download_duration_seconds";

/// Origin credentials and malformed authority text never appear in the logged URL.
#[test]
fn loggable_url_removes_credentials() {
    carbide_test_support::value_scenarios!(loggable_url:
        "remote URL credentials" {
            // Removing the query must not leave authority credentials behind.
            "https://user:password@firmware.example/bmc.fwpkg?X-Amz-Signature=secret#token"
                => "https://firmware.example/bmc.fwpkg".to_string(),
        }
        "fragment without a query" {
            // A fragment needs removal even when there is no query to truncate.
            "https://firmware.example/bmc.fwpkg#token"
                => "https://firmware.example/bmc.fwpkg".to_string(),
        }
        "local files" {
            // Credential removal leaves the local source path intact.
            "file:///tmp/bmc.fwpkg" => "file:///tmp/bmc.fwpkg".to_string(),
        }
        "malformed URL" {
            // Unparseable authority text cannot safely be echoed back.
            "https://user:password@[invalid?token=secret" => "<invalid firmware URL>".to_string(),
        }
    );
}

/// Polls until the download-duration histogram has recorded `expect`
/// observations under `outcome` -- the completion event is the signal that a
/// background download attempt finished.
async fn wait_for_downloads(metrics: &MetricsCapture, outcome: &str, expect: u64) {
    let mut count = 0;
    while metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", outcome)]) < expect
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
        count += 1;
        if count >= 1000 {
            panic!("No download finished with outcome={outcome}");
        }
    }
}

/// Calls to `available` made while a download is in flight join it instead of
/// starting another, so the source is fetched once.
#[tokio::test]
async fn repeated_available_calls_share_one_download() {
    let requested = std::sync::Arc::new(tokio::sync::Notify::new());
    let body_requested = requested.clone();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(release_rx);
    let mut server = mockito::Server::new_async().await;
    let source = server
        .mock("GET", "/fw.bin")
        .with_chunked_body(move |writer| {
            body_requested.notify_one();
            release_rx
                .lock()
                .unwrap()
                .recv()
                .map_err(std::io::Error::other)?;
            writer.write_all(b"firmware artifact")
        })
        .expect(1)
        .create_async()
        .await;
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    let url = format!("{}/fw.bin", server.url());
    let downloader = FirmwareDownloader::new();

    assert_eq!(
        downloader.available(&filename, &url, "").await,
        ArtifactStatus::Downloading
    );
    tokio::time::timeout(Duration::from_secs(5), requested.notified())
        .await
        .unwrap();
    for _ in 0..9 {
        assert_eq!(
            downloader.available(&filename, &url, "").await,
            ArtifactStatus::Downloading,
        );
    }

    release_tx.send(()).unwrap();
    drop(release_tx);
    assert_eq!(
        status_after_download(&downloader, &filename, &url, "").await,
        ArtifactStatus::Available,
    );
    assert_eq!(std::fs::read(&filename).unwrap(), b"firmware artifact");
    source.assert_async().await;
}

/// Local-file downloads retain optional checksums and are not subject to the
/// HTTP byte limit, even when the source is larger than that configured limit.
#[tokio::test]
async fn test_download_without_checksum() -> Result<(), std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    let filename = temp_dir.path().join("firmware.bin");
    let src_filename = temp_dir.path().join("source.bin");
    let url = format!("file://{}", src_filename.display());

    let mut srcfile = File::create(&src_filename).await?;
    for i in 0..2000 {
        srcfile.write_all(format!("{i}").as_bytes()).await?;
    }
    srcfile.flush().await?;

    let downloader = FirmwareDownloader::with_config(FirmwareDownloadConfig {
        max_download_bytes: 1,
        ..Default::default()
    })
    .unwrap();
    let metrics = MetricsCapture::start();

    let mut count = 0;
    loop {
        if downloader.available(&filename, &url, "").await != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            // >=1 rather than an exact delta: concurrent tests can add ok
            // samples, but if the ok emit vanished nothing could satisfy this.
            wait_for_downloads(&metrics, "ok", 1).await;
            assert_eq!(std::fs::read(&filename)?, std::fs::read(&src_filename)?);
            assert!(std::fs::metadata(&filename)?.len() > 1);
            return Ok(());
        }
    }
}

/// A matching checksum accepts surrounding whitespace and uppercase hexadecimal.
#[tokio::test]
async fn test_available_verifies_sha256_checksum() -> Result<(), std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    let filename = temp_dir.path().join("firmware.bin");
    let src_filename = temp_dir.path().join("source.bin");
    let url = format!("file://{}", src_filename.display());
    let contents = b"firmware artifact";

    let mut srcfile = File::create(&src_filename).await?;
    srcfile.write_all(contents).await?;
    srcfile.flush().await?;

    let downloader = FirmwareDownloader::new();
    let checksum = format!(
        " {} ",
        hex::encode(sha2::Sha256::digest(contents)).to_ascii_uppercase()
    );

    let mut count = 0;
    loop {
        if downloader.available(&filename, &url, &checksum).await != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            // Cache acceptance releases the claim; another caller must not
            // mistake the completed verification for an ongoing download.
            assert_eq!(
                downloader.available(&filename, &url, &checksum).await,
                ArtifactStatus::Available,
            );
            return Ok(());
        }
    }
}

/// A mismatched cached artifact is replaced with bytes that satisfy the configured checksum.
#[tokio::test]
async fn test_available_rejects_stale_cache_with_wrong_sha256() -> Result<(), std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    let filename = temp_dir.path().join("firmware.bin");
    let src_filename = temp_dir.path().join("source.bin");
    let url = format!("file://{}", src_filename.display());
    let contents = b"fresh firmware artifact";

    let mut cached_file = File::create(&filename).await?;
    cached_file.write_all(b"stale firmware artifact").await?;
    cached_file.flush().await?;
    drop(cached_file);

    let mut srcfile = File::create(&src_filename).await?;
    srcfile.write_all(contents).await?;
    srcfile.flush().await?;
    drop(srcfile);

    let downloader = FirmwareDownloader::new();
    let checksum = hex::encode(sha2::Sha256::digest(contents));

    assert_eq!(
        downloader.available(&filename, &url, &checksum).await,
        ArtifactStatus::Downloading,
    );

    let mut count = 0;
    loop {
        if downloader.available(&filename, &url, &checksum).await != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            assert_eq!(tokio::fs::read(&filename).await?, contents);
            return Ok(());
        }
    }
}

/// Checksum failures clean up staging and release the destination for a repaired source.
#[tokio::test]
async fn test_available_checksum_failure_does_not_publish_file() -> Result<(), std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    let filename = temp_dir.path().join("firmware.bin");
    let src_filename = temp_dir.path().join("source.bin");
    let url = format!("file://{}", src_filename.display());

    let contents = b"verified firmware artifact";
    let checksum = hex::encode(sha2::Sha256::digest(contents));
    tokio::fs::write(&src_filename, b"incorrect firmware artifact").await?;

    let downloader = FirmwareDownloader::new();

    let metrics = MetricsCapture::start();
    assert_eq!(
        downloader.available(&filename, &url, &checksum).await,
        ArtifactStatus::Downloading,
    );
    wait_for_downloads(&metrics, "checksum", 1).await;

    let ArtifactStatus::Failed { error } =
        status_after_download(&downloader, &filename, &url, &checksum).await
    else {
        panic!("a checksum mismatch must be reported to the next caller");
    };
    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(!filename.exists());
    assert!(!temp_dir.path().join("firmware.bin.download").exists());

    // Retrying the same checksum must verify the repaired bytes, not merely
    // clear the prior failure or publish the first attempt's staging file.
    tokio::fs::write(&src_filename, contents).await?;
    assert_eq!(
        status_after_download(&downloader, &filename, &url, &checksum).await,
        ArtifactStatus::Available,
    );
    assert_eq!(tokio::fs::read(&filename).await?, contents);
    Ok(())
}

/// A source that cannot be opened is the fetch failure: the attempt counts
/// under `outcome="fetch"` and publishes nothing.
#[tokio::test]
async fn test_available_fetch_failure_counts() -> Result<(), std::io::Error> {
    let temp_dir = tempfile::tempdir()?;
    let filename = temp_dir.path().join("firmware.bin");
    let src_filename = temp_dir.path().join("missing-source.bin");
    let url = format!("file://{}", src_filename.display());

    let downloader = FirmwareDownloader::new();

    let metrics = MetricsCapture::start();
    assert_eq!(
        downloader.available(&filename, &url, "").await,
        ArtifactStatus::Downloading,
    );
    wait_for_downloads(&metrics, "fetch", 1).await;

    let ArtifactStatus::Failed { error } =
        status_after_download(&downloader, &filename, &url, "").await
    else {
        panic!("an unavailable source must be reported to the next caller");
    };
    assert!(error.contains("could not open source"), "{error}");
    assert!(!filename.exists());
    assert!(!temp_dir.path().join("firmware.bin.download").exists());
    Ok(())
}

/// Polls the public API until an attempt completes, retaining the requested checksum.
async fn status_after_download(
    downloader: &FirmwareDownloader,
    filename: &Path,
    url: &str,
    checksum: &str,
) -> ArtifactStatus {
    for _ in 0..1000 {
        let status = downloader.available(filename, url, checksum).await;
        if status != ArtifactStatus::Downloading {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("download from {url} did not finish");
}

/// A failed attempt is reported to the next caller once, and the call after
/// that starts a new attempt, so a caller retries by asking again.
#[tokio::test]
async fn failed_download_is_reported_once_then_retried() {
    // Fetch-failure assertions in other tests use this shared capture lock.
    let _metrics = MetricsCapture::start();
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    let src_filename = temp_dir.path().join("src.bin");
    let url = format!("file://{}", src_filename.display());
    let downloader = FirmwareDownloader::new();

    assert_eq!(
        downloader.available(&filename, &url, "").await,
        ArtifactStatus::Downloading
    );
    let ArtifactStatus::Failed { error } =
        status_after_download(&downloader, &filename, &url, "").await
    else {
        panic!("a missing source should fail the download");
    };
    assert!(
        error.contains(&loggable_url(&url)),
        "error should name the source: {error}"
    );

    std::fs::write(&src_filename, b"firmware artifact").unwrap();
    assert_eq!(
        downloader.available(&filename, &url, "").await,
        ArtifactStatus::Downloading,
    );
    assert_eq!(
        status_after_download(&downloader, &filename, &url, "").await,
        ArtifactStatus::Available,
    );
    assert_eq!(std::fs::read(&filename).unwrap(), b"firmware artifact");
}

/// A broken HTTP body leaves no published or partial file, and the next attempt can succeed.
#[tokio::test]
async fn truncated_http_body_is_removed_before_a_successful_retry() {
    // This test emits the transfer metric. Serialize its observations with
    // the metric-contract tests, which assert exact deltas.
    let metrics = MetricsCapture::start();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial"
                .as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nfirmware".as_slice(),
        ] {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(connection.read_u8().await.unwrap());
            }
            connection.write_all(response).await.unwrap();
            connection.shutdown().await.unwrap();
        }
    });
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    let downloader = FirmwareDownloader::new();

    let ArtifactStatus::Failed { error } =
        status_after_download(&downloader, &filename, &url, "").await
    else {
        panic!("a truncated response must fail");
    };
    assert!(error.contains("body"), "{error}");
    assert!(!filename.exists());
    assert!(!temp_dir.path().join("fw.bin.download").exists());
    assert_eq!(
        metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", "transfer")]),
        1,
    );
    assert_eq!(
        status_after_download(&downloader, &filename, &url, "").await,
        ArtifactStatus::Available,
    );
    assert_eq!(std::fs::read(&filename).unwrap(), b"firmware");
    server.await.unwrap();
}

/// Consumes the request before a loopback server introduces its response failure.
async fn accept_http_request(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(connection.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"GET /fw.bin "));
        connection
    })
    .await
    .expect("the downloader should request the loopback firmware source")
}

/// Clones share admission: saturation neither queues a distinct destination nor
/// consumes its recorded failure, and only a later explicit poll starts work.
#[tokio::test]
async fn saturated_public_downloads_require_an_explicit_retry() {
    let metrics = MetricsCapture::start();
    let temp_dir = tempfile::tempdir().unwrap();
    let first_filename = temp_dir.path().join("first.bin");
    let deferred_filename = temp_dir.path().join("deferred.bin");
    let downloader = FirmwareDownloader::with_config(FirmwareDownloadConfig {
        max_concurrent_downloads: 1,
        ..Default::default()
    })
    .unwrap();
    let other_caller = downloader.clone();

    // Leave a completed failure unconsumed. A saturated poll must not erase it
    // while returning Downloading instead of that failure.
    let missing_url = format!("file://{}", temp_dir.path().join("missing.bin").display());
    assert_eq!(
        other_caller
            .available(&deferred_filename, &missing_url, "")
            .await,
        ArtifactStatus::Downloading,
    );
    wait_for_downloads(&metrics, "fetch", 1).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
    assert_eq!(
        downloader.available(&first_filename, &url, "").await,
        ArtifactStatus::Downloading,
    );
    let mut first_connection = accept_http_request(&listener).await;

    // The accepted request holds the only slot until we supply its response.
    let mut deferred_poll = Box::pin(other_caller.available(&deferred_filename, &url, ""));
    assert_eq!(
        futures_util::poll!(deferred_poll.as_mut()),
        std::task::Poll::Ready(ArtifactStatus::Downloading),
        "saturation must return without waiting for capacity",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "a saturated caller must not start a second request",
    );
    assert!(!temp_dir.path().join("deferred.bin.download").exists());

    first_connection
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nfirmware")
        .await
        .unwrap();
    first_connection.shutdown().await.unwrap();
    assert_eq!(
        status_after_download(&downloader, &first_filename, &url, "").await,
        ArtifactStatus::Available,
    );
    assert_eq!(std::fs::read(&first_filename).unwrap(), b"firmware");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "releasing capacity must not start deferred work automatically",
    );

    let ArtifactStatus::Failed { error } =
        other_caller.available(&deferred_filename, &url, "").await
    else {
        panic!("the saturated poll must preserve the earlier source failure");
    };
    assert!(error.contains("could not open source"), "{error}");
    assert_eq!(
        metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", "fetch")]),
        1,
    );
    assert_eq!(
        other_caller.available(&deferred_filename, &url, "").await,
        ArtifactStatus::Downloading,
    );
    let mut retry_connection = accept_http_request(&listener).await;
    retry_connection
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nfirmware")
        .await
        .unwrap();
    retry_connection.shutdown().await.unwrap();
    assert_eq!(
        status_after_download(&other_caller, &deferred_filename, &url, "").await,
        ArtifactStatus::Available,
    );
    assert_eq!(std::fs::read(&deferred_filename).unwrap(), b"firmware");
}

/// HTTP deadlines release staging and the sole download slot, including when
/// a server keeps supplying bytes often enough to avoid the read-idle timeout.
#[tokio::test]
async fn http_deadlines_remove_partial_files_and_allow_retry() {
    #[derive(Clone, Copy, Debug)]
    enum ResponsePhase {
        TlsHandshake,
        Headers,
        IdleBody,
        ProgressingBody,
    }

    struct Case {
        phase: ResponsePhase,
        read_timeout_seconds: u64,
        request_timeout_seconds: u64,
    }

    for case in [
        // TLS negotiation is part of connecting, before either longer timer.
        Case {
            phase: ResponsePhase::TlsHandshake,
            read_timeout_seconds: 5,
            request_timeout_seconds: 5,
        },
        // An accepted connection without response headers must still finish.
        Case {
            phase: ResponsePhase::Headers,
            read_timeout_seconds: 1,
            request_timeout_seconds: 5,
        },
        // Receiving headers and some bytes does not permit an idle body to hang.
        Case {
            phase: ResponsePhase::IdleBody,
            read_timeout_seconds: 1,
            request_timeout_seconds: 5,
        },
        // Progress resets the idle timer, but cannot extend the whole request.
        Case {
            phase: ResponsePhase::ProgressingBody,
            read_timeout_seconds: 1,
            request_timeout_seconds: 2,
        },
    ] {
        // Fetch and transfer failures share the exact metric capture window.
        let metrics = MetricsCapture::start();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let phase = case.phase;
        let scheme = if matches!(phase, ResponsePhase::TlsHandshake) {
            "https"
        } else {
            "http"
        };
        let url = format!("{scheme}://{}/fw.bin", listener.local_addr().unwrap());
        let retry_url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut connection = if matches!(phase, ResponsePhase::TlsHandshake) {
                let (mut connection, _) = listener.accept().await.unwrap();
                // Observe a TLS handshake record, but send no ServerHello or
                // certificate. The TCP connection itself has already succeeded.
                assert_eq!(connection.read_u8().await.unwrap(), 0x16);
                connection
            } else {
                accept_http_request(&listener).await
            };
            let response_started = std::time::Instant::now();
            if matches!(
                phase,
                ResponsePhase::IdleBody | ResponsePhase::ProgressingBody
            ) {
                connection
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\nf\r\n")
                    .await
                    .unwrap();
            }

            let mut progress = tokio::time::interval(Duration::from_millis(100));
            let mut chunk_count = 0;
            let mut bytes = [0; 4096];
            loop {
                tokio::select! {
                    received = connection.read(&mut bytes) => {
                        match received {
                            Ok(0) => break,
                            // Drain the remaining ClientHello without making
                            // progress on the stalled server handshake.
                            Ok(_) if matches!(phase, ResponsePhase::TlsHandshake) => {},
                            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                            other => panic!("expected the timed-out client to close its connection, got {other:?}"),
                        }
                    }
                    _ = progress.tick(), if matches!(phase, ResponsePhase::ProgressingBody) => {
                        if let Err(error) = connection.write_all(b"1\r\nf\r\n").await {
                            // The request deadline can close the socket while
                            // this next chunk is being written.
                            assert!(matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset,
                            ), "{error}");
                            break;
                        }
                        chunk_count += 1;
                    }
                }
            }
            let response_elapsed = response_started.elapsed();
            drop(connection);

            // A repaired source must be fetched again, rather than accepting
            // the partial staging file or leaving the only permit occupied.
            let mut connection = accept_http_request(&listener).await;
            connection
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nfirmware",
                )
                .await
                .unwrap();
            connection.shutdown().await.unwrap();
            (chunk_count, response_elapsed)
        });
        let temp_dir = tempfile::tempdir().unwrap();
        let filename = temp_dir.path().join("fw.bin");
        let downloader = FirmwareDownloader::with_config(FirmwareDownloadConfig {
            max_concurrent_downloads: 1,
            connect_timeout_seconds: 1,
            read_timeout_seconds: case.read_timeout_seconds,
            request_timeout_seconds: case.request_timeout_seconds,
            ..Default::default()
        })
        .unwrap();

        // Connect and read-idle cases have a five-second request deadline.
        // Failure before that deadline isolates their one-second phase timer.
        let status = tokio::time::timeout(
            Duration::from_secs(4),
            status_after_download(&downloader, &filename, &url, ""),
        )
        .await
        .expect("the configured HTTP phase should expire before the test deadline");
        let ArtifactStatus::Failed { error } = status else {
            panic!("{phase:?} must expire before the test's polling deadline");
        };
        assert!(error.contains("timed out"), "{phase:?}: {error}");
        let expected_metric = match phase {
            ResponsePhase::TlsHandshake | ResponsePhase::Headers => "fetch",
            ResponsePhase::IdleBody | ResponsePhase::ProgressingBody => "transfer",
        };
        assert_eq!(
            metrics
                .histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", expected_metric)]),
            1,
            "{phase:?}",
        );
        assert!(!filename.exists(), "{phase:?}");
        assert!(
            !temp_dir.path().join("fw.bin.download").exists(),
            "{phase:?}"
        );
        assert_eq!(
            status_after_download(&downloader, &filename, &retry_url, "").await,
            ArtifactStatus::Available,
            "{phase:?}",
        );
        assert_eq!(std::fs::read(&filename).unwrap(), b"firmware", "{phase:?}");
        let (chunk_count, response_elapsed) = server.await.unwrap();
        if matches!(phase, ResponsePhase::ProgressingBody) {
            assert!(
                chunk_count >= 3,
                "the source must progress before timing out"
            );
            assert!(
                response_elapsed >= Duration::from_millis(1500),
                "progress must keep the one-second read timer alive: {response_elapsed:?}",
            );
        }
    }
}

/// The byte limit counts the complete body with or without Content-Length;
/// overflow cannot publish partial bytes or prevent a repaired-source retry.
#[tokio::test]
async fn http_bodies_enforce_the_cumulative_byte_limit() {
    struct Case {
        scenario: &'static str,
        response_start: &'static [u8],
        response_end: &'static [u8],
        wait_for_first_chunk: bool,
        exceeds_limit: bool,
    }

    const CHUNKED_START: &[u8] =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nfirm\r\n";
    for case in [
        // Equality is allowed, even when the body arrives in separate chunks.
        Case {
            scenario: "chunked exact limit",
            response_start: CHUNKED_START,
            response_end: b"4\r\nware\r\n0\r\n\r\n",
            wait_for_first_chunk: true,
            exceeds_limit: false,
        },
        // Each chunk fits independently; only the accumulated count exceeds eight.
        Case {
            scenario: "chunked over limit",
            response_start: CHUNKED_START,
            response_end: b"5\r\nwares\r\n0\r\n\r\n",
            wait_for_first_chunk: true,
            exceeds_limit: true,
        },
        // A declared body length does not exempt the transfer from the cap.
        Case {
            scenario: "Content-Length over limit",
            response_start:
                b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nfirm",
            response_end: b"wares",
            wait_for_first_chunk: false,
            exceeds_limit: true,
        },
    ] {
        let scenario = case.scenario;
        let metrics = MetricsCapture::start();
        let temp_dir = tempfile::tempdir().unwrap();
        let filename = temp_dir.path().join("fw.bin");
        let staged_filename = temp_dir.path().join("fw.bin.download");
        let server_staged_filename = staged_filename.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut connection = accept_http_request(&listener).await;
            connection.write_all(case.response_start).await.unwrap();
            // Wait for the first chunk to reach disk so socket coalescing cannot
            // turn the chunked cases into a single oversized segment. A known
            // Content-Length may also be rejected before writing any bytes.
            if case.wait_for_first_chunk {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if std::fs::metadata(&server_staged_filename)
                            .is_ok_and(|metadata| metadata.len() == 4)
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the first chunk should be written before sending the next");
            }
            connection.write_all(case.response_end).await.unwrap();
            connection.shutdown().await.unwrap();
            if case.exceeds_limit {
                let mut connection = accept_http_request(&listener).await;
                connection
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nfirmware")
                    .await
                    .unwrap();
                connection.shutdown().await.unwrap();
            }
        });
        let downloader = FirmwareDownloader::with_config(FirmwareDownloadConfig {
            max_concurrent_downloads: 1,
            max_download_bytes: 8,
            ..Default::default()
        })
        .unwrap();

        let status = status_after_download(&downloader, &filename, &url, "").await;
        if case.exceeds_limit {
            let ArtifactStatus::Failed { error } = status else {
                panic!("{scenario} must reject the cumulative body length");
            };
            assert!(error.contains("exceeds max_download_bytes"), "{error}");
            assert_eq!(
                metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", "transfer")]),
                1,
                "{scenario}",
            );
            assert!(!filename.exists());
            assert!(!staged_filename.exists());
            assert_eq!(
                status_after_download(&downloader, &filename, &url, "").await,
                ArtifactStatus::Available,
                "a rejected body must release the sole download slot",
            );
        } else {
            assert_eq!(status, ArtifactStatus::Available, "{scenario}");
        }
        assert_eq!(std::fs::read(&filename).unwrap(), b"firmware", "{scenario}");
        assert!(!staged_filename.exists(), "{scenario}");
        server.await.unwrap();
    }
}

/// Filesystem failures before fetch and after transfer both release staging and permit retry.
#[tokio::test]
async fn filesystem_failures_release_the_destination_for_retry() {
    #[derive(Clone, Copy, Debug)]
    enum Failure {
        Staging,
        Publish,
    }

    for failure in [
        // A non-directory parent rejects staging before any source request.
        Failure::Staging,
        // A destination directory introduced after cache inspection rejects
        // publication only after the response body has been downloaded.
        Failure::Publish,
    ] {
        let metrics = MetricsCapture::start();
        let temp_dir = tempfile::tempdir().unwrap();
        let cache_dir = temp_dir.path().join("cache");
        let filename = cache_dir.join("fw.bin");
        let contents = b"verified firmware artifact";
        let checksum = hex::encode(sha2::Sha256::digest(contents));
        let requested = std::sync::Arc::new(tokio::sync::Notify::new());
        let body_requested = requested.clone();
        let first_request = std::sync::atomic::AtomicBool::new(true);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let mut server = mockito::Server::new_async().await;
        let source = server
            .mock("GET", "/fw.bin")
            .with_chunked_body(move |writer| {
                if matches!(failure, Failure::Publish)
                    && first_request.swap(false, std::sync::atomic::Ordering::Relaxed)
                {
                    body_requested.notify_one();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv()
                        .map_err(std::io::Error::other)?;
                }
                writer.write_all(contents)
            })
            .expect(match failure {
                Failure::Staging => 1,
                Failure::Publish => 2,
            })
            .create_async()
            .await;
        let url = format!("{}/fw.bin", server.url());
        let downloader = FirmwareDownloader::new();

        if matches!(failure, Failure::Staging) {
            std::fs::write(&cache_dir, b"not a directory").unwrap();
        }
        assert_eq!(
            downloader.available(&filename, &url, &checksum).await,
            ArtifactStatus::Downloading,
            "{failure:?}",
        );
        if matches!(failure, Failure::Publish) {
            // The request proves cache inspection and staging completed;
            // holding its body keeps rename from racing this filesystem edit.
            tokio::time::timeout(Duration::from_secs(5), requested.notified())
                .await
                .unwrap();
            std::fs::create_dir(&filename).unwrap();
            release_tx.send(()).unwrap();
        }
        drop(release_tx);
        wait_for_downloads(&metrics, "io", 1).await;
        assert!(!filename.is_file(), "{failure:?}");
        assert!(!cache_dir.join("fw.bin.download").exists(), "{failure:?}");

        // Remove the injected obstruction before polling so cached-path
        // validation cannot mask the saved download failure.
        let expected_error = match failure {
            Failure::Staging => {
                std::fs::remove_file(&cache_dir).unwrap();
                "unable to create directory"
            }
            Failure::Publish => {
                std::fs::remove_dir(&filename).unwrap();
                "unable to rename"
            }
        };
        let ArtifactStatus::Failed { error } =
            status_after_download(&downloader, &filename, &url, &checksum).await
        else {
            panic!("{failure:?} must retain its failure for the next caller");
        };
        assert!(error.contains(expected_error), "{failure:?}: {error}");
        assert_eq!(
            metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", "io")]),
            1,
            "{failure:?}",
        );

        assert_eq!(
            status_after_download(&downloader, &filename, &url, &checksum).await,
            ArtifactStatus::Available,
            "{failure:?}",
        );
        assert_eq!(std::fs::read(&filename).unwrap(), contents, "{failure:?}");
        source.assert_async().await;
    }
}

/// Public failures and completion events retain source context without URL credentials.
#[tokio::test]
async fn download_failure_and_completion_event_redact_source_credentials() {
    // Fetch failures share the metric capture lock even when this test only logs.
    let _metrics = MetricsCapture::start();
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    // Nothing listens on port 1, so the connection is refused immediately.
    let url = "http://user:password@127.0.0.1:1/fw.bin?X-Amz-Signature=secret#fragment-secret";
    let downloader = FirmwareDownloader::new();

    let (status, logs) = capture_logs_async(async {
        downloader.available(&filename, url, "").await;
        status_after_download(&downloader, &filename, url, "").await
    })
    .await;
    let ArtifactStatus::Failed { error } = status else {
        panic!("a refused connection should fail the download");
    };
    assert!(error.contains("http://127.0.0.1:1/fw.bin"), "{error}");
    assert!(error.contains("error trying to download"), "{error}");
    assert!(!error.contains("secret"), "{error}");
    assert!(!error.contains("user"), "{error}");
    assert!(!error.contains("password"), "{error}");

    // Assert the event produced by this attempt, rather than constructing a
    // sanitized event directly and bypassing the downloader's logging path.
    let completed = logs
        .iter()
        .find(|log| log.metadata_name == "firmware_download_finished")
        .expect("a failed attempt emits its completion event");
    assert_eq!(completed.level, tracing::Level::ERROR);
    assert_eq!(completed.field("url"), Some("http://127.0.0.1:1/fw.bin"));
    assert_eq!(completed.field("error"), Some(error.as_str()));
    assert_eq!(completed.field("outcome"), Some("fetch"));
}

#[test]
fn artifact_unavailable_events_preserve_log_contracts_and_count_reasons() {
    #[derive(Clone, Copy)]
    enum Fixture {
        MissingUrl,
        StaleCacheRemovalFailed,
    }

    #[derive(Debug, PartialEq)]
    struct Contract {
        event_name: String,
        message: String,
        metric_name: Option<String>,
        reason: Option<String>,
        firmware_path: Option<String>,
        filename: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    let metrics = MetricsCapture::start();
    check_values(
        [
            Check {
                scenario: "missing URL retains firmware_path context",
                input: Fixture::MissingUrl,
                expect: Contract {
                    event_name: "firmware_artifact_missing_url".to_string(),
                    message: "Firmware artifact is missing and has no URL".to_string(),
                    metric_name: Some(ARTIFACT_UNAVAILABLE_METRIC.to_string()),
                    reason: Some("missing_url".to_string()),
                    firmware_path: Some("\"/firmware/missing.fwpkg\"".to_string()),
                    filename: None,
                    error: None,
                    counter_delta: 1.0,
                },
            },
            Check {
                scenario: "stale cache removal failure retains filename and error context",
                input: Fixture::StaleCacheRemovalFailed,
                expect: Contract {
                    event_name: "firmware_stale_cached_artifact_removal_failed".to_string(),
                    message: "Failed to remove stale cached firmware artifact".to_string(),
                    metric_name: Some(ARTIFACT_UNAVAILABLE_METRIC.to_string()),
                    reason: Some("stale_cache_removal_failed".to_string()),
                    firmware_path: None,
                    filename: Some("/firmware/stale.fwpkg".to_string()),
                    error: Some("read-only filesystem".to_string()),
                    counter_delta: 1.0,
                },
            },
        ],
        |fixture| {
            let reason = match fixture {
                Fixture::MissingUrl => "missing_url",
                Fixture::StaleCacheRemovalFailed => "stale_cache_removal_failed",
            };
            let logs = capture_logs(|| match fixture {
                Fixture::MissingUrl => emit(FirmwareArtifactMissingUrl {
                    reason: ArtifactUnavailableReason::MissingUrl,
                    firmware_path: format!("{:?}", Path::new("/firmware/missing.fwpkg")),
                }),
                Fixture::StaleCacheRemovalFailed => {
                    emit(FirmwareStaleCachedArtifactRemovalFailed {
                        reason: ArtifactUnavailableReason::StaleCacheRemovalFailed,
                        filename: "/firmware/stale.fwpkg".to_string(),
                        error: "read-only filesystem".to_string(),
                    });
                }
            });

            assert_eq!(logs.len(), 1, "one event should own the error log");
            assert_eq!(logs[0].level, tracing::Level::ERROR);
            Contract {
                event_name: logs[0].metadata_name.clone(),
                message: logs[0].message.clone(),
                metric_name: logs[0].field("metric_name").map(str::to_string),
                reason: logs[0].field("reason").map(str::to_string),
                firmware_path: logs[0].field("firmware_path").map(str::to_string),
                filename: logs[0].field("filename").map(str::to_string),
                error: logs[0].field("error").map(str::to_string),
                counter_delta: metrics
                    .counter_delta(ARTIFACT_UNAVAILABLE_METRIC, &[("reason", reason)]),
            }
        },
    );
}

/// An absent artifact with no download source reports its blocker and existing metric.
#[tokio::test]
async fn unavailable_absent_artifact_without_url_counts_the_blocker() {
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("missing.fwpkg");
    let downloader = FirmwareDownloader::new();
    let metrics = MetricsCapture::start();

    let ((), logs) = capture_logs_async(async {
        assert!(matches!(
            downloader.available(&filename, "", "").await,
            ArtifactStatus::Failed { .. },
        ));
    })
    .await;

    // Tokio also traces semaphore permit release; this contract concerns the
    // firmware error event, not runtime instrumentation.
    let logs: Vec<_> = logs
        .into_iter()
        .filter(|log| log.target == "carbide_firmware::downloader")
        .collect();
    assert_eq!(logs.len(), 1, "{logs:#?}");
    assert_eq!(logs[0].metadata_name, "firmware_artifact_missing_url");
    assert_eq!(
        logs[0].message,
        "Firmware artifact is missing and has no URL"
    );
    assert_eq!(
        logs[0].field("firmware_path"),
        Some(format!("{filename:?}").as_str())
    );
    assert_eq!(
        metrics.counter_delta(ARTIFACT_UNAVAILABLE_METRIC, &[("reason", "missing_url")]),
        1.0,
    );
    assert!(!filename.exists());
    // The missing-URL return must release its claim for subsequent callers.
    assert!(matches!(
        downloader.available(&filename, "", "").await,
        ArtifactStatus::Failed { .. },
    ));
}

/// A stale cache entry that cannot be removed reports the filesystem error before fetching.
#[tokio::test]
async fn unavailable_stale_artifact_removal_failure_counts_the_blocker() {
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("stale.fwpkg");
    std::fs::create_dir(&filename).unwrap();
    let downloader = FirmwareDownloader::new();
    let metrics = MetricsCapture::start();

    let ((), logs) = capture_logs_async(async {
        assert!(matches!(
            downloader
                .available(
                    &filename,
                    "https://firmware.example/stale.fwpkg",
                    "expected-checksum",
                )
                .await,
            ArtifactStatus::Failed { .. },
        ));
    })
    .await;

    assert!(logs.iter().any(|log| {
        log.message == "Cached firmware artifact failed checksum verification"
            && log.level == tracing::Level::WARN
    }));
    let unavailable = logs
        .iter()
        .find(|log| log.metadata_name == "firmware_stale_cached_artifact_removal_failed")
        .expect("removal failure should emit an unavailable-artifact event");
    assert_eq!(unavailable.level, tracing::Level::ERROR);
    assert_eq!(
        unavailable.message,
        "Failed to remove stale cached firmware artifact"
    );
    assert_eq!(
        unavailable.field("filename"),
        Some(filename.to_string_lossy().as_ref())
    );
    assert!(
        unavailable
            .field("error")
            .is_some_and(|error| !error.is_empty())
    );
    assert_eq!(
        metrics.counter_delta(
            ARTIFACT_UNAVAILABLE_METRIC,
            &[("reason", "stale_cache_removal_failed")],
        ),
        1.0,
    );
    assert!(filename.is_dir());
    // An unusable cache is still an immediate failure on the next call,
    // rather than a destination left permanently marked as downloading.
    assert!(matches!(
        downloader
            .available(
                &filename,
                "https://firmware.example/stale.fwpkg",
                "expected-checksum",
            )
            .await,
        ArtifactStatus::Failed { .. },
    ));
}

/// The label vocabulary is the dashboard contract: every download outcome
/// renders as its snake_case name.
#[test]
fn download_outcome_labels_render_snake_case() {
    check_values(
        [
            Check {
                scenario: "success",
                input: DownloadOutcome::Ok,
                expect: "ok".to_string(),
            },
            Check {
                scenario: "request failure",
                input: DownloadOutcome::Fetch,
                expect: "fetch".to_string(),
            },
            Check {
                scenario: "non-success HTTP status",
                input: DownloadOutcome::Status,
                expect: "status".to_string(),
            },
            Check {
                scenario: "broken body stream",
                input: DownloadOutcome::Transfer,
                expect: "transfer".to_string(),
            },
            Check {
                scenario: "checksum verification failure",
                input: DownloadOutcome::Checksum,
                expect: "checksum".to_string(),
            },
            Check {
                scenario: "local filesystem failure",
                input: DownloadOutcome::Io,
                expect: "io".to_string(),
            },
        ],
        |outcome| outcome.label_value().to_string(),
    );
}

/// One emit per download attempt: the histogram records the duration under
/// the attempt's outcome, and the event owns the completion line -- INFO for
/// a success, ERROR for any failure, with the URL and error detail as
/// context.
#[test]
fn download_finished_records_duration_and_owns_the_completion_line() {
    let metrics = MetricsCapture::start();
    let logs = capture_logs(|| {
        emit(DownloadFinished::Ok {
            took: Duration::from_secs(30),
            url: "https://firmware.example/bmc.fwpkg".to_string(),
            filename: "/firmware/bmc.fwpkg".to_string(),
        });
        // Tests which emit `status`, `transfer`, or `io` also hold a metric
        // capture, so their observations cannot race these exact deltas.
        emit(DownloadFinished::Status {
            took: Duration::from_secs(2),
            url: "https://firmware.example/bmc.fwpkg".to_string(),
            filename: "/firmware/bmc.fwpkg".to_string(),
            error: "FirmwareDownloader got non-success status trying to download \
                    https://firmware.example/bmc.fwpkg: 404 Not Found"
                .to_string(),
        });
        emit(DownloadFinished::Transfer {
            took: Duration::from_secs(3),
            url: "https://firmware.example/uefi.fwpkg".to_string(),
            filename: "/firmware/uefi.fwpkg".to_string(),
            error: "connection reset by peer".to_string(),
        });
        emit(DownloadFinished::Io {
            took: Duration::from_secs(4),
            url: "https://firmware.example/cec.fwpkg".to_string(),
            filename: "/firmware/cec.fwpkg".to_string(),
            error: "No space left on device".to_string(),
        });
    });

    assert_eq!(logs.len(), 4, "every completion writes its line: {logs:?}");
    assert!(
        logs.iter()
            .all(|entry| entry.message == "Firmware download finished")
    );
    assert_eq!(logs[0].level, tracing::Level::INFO);
    assert!(
        logs[1..]
            .iter()
            .all(|entry| entry.level == tracing::Level::ERROR)
    );

    let field = |entry: &CapturedLog, name: &str| {
        entry
            .fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(field(&logs[1], "outcome").as_deref(), Some("status"));
    assert!(
        field(&logs[1], "url").is_some_and(|url| url.contains("bmc.fwpkg")),
        "the completion line names the URL"
    );
    assert!(
        field(&logs[1], "error").is_some_and(|error| error.contains("404")),
        "the completion line holds the error detail"
    );

    // No delta assertion for `ok`: the end-to-end download tests in this
    // binary complete successful downloads outside any capture window, so
    // that series moves concurrently.
    for (outcome, seconds) in [("status", 2.0), ("transfer", 3.0), ("io", 4.0)] {
        assert_eq!(
            metrics.histogram_count_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", outcome)]),
            1,
            "one observation under outcome={outcome}",
        );
        let sum = metrics.histogram_sum_delta(DOWNLOAD_DURATION_METRIC, &[("outcome", outcome)]);
        assert_eq!(
            sum,
            ApproxHistogramSum(seconds),
            "outcome={outcome} records {seconds}s"
        );
    }
}
