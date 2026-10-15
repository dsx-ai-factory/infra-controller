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

use carbide_instrument::testing::{ApproxHistogramSum, CapturedLog, MetricsCapture, capture_logs};
use carbide_instrument::{LabelValue, emit};
use carbide_test_support::{Check, check_values};
use sha2::Digest;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use crate::downloader::*;

const ARTIFACT_UNAVAILABLE_METRIC: &str = "carbide_firmware_artifact_unavailable_total";
const DOWNLOAD_DURATION_METRIC: &str = "carbide_firmware_download_duration_seconds";

#[test]
fn loggable_url_drops_query_parameters() {
    assert_eq!(
        loggable_url("https://firmware.example/bmc.fwpkg?X-Amz-Signature=secret"),
        "https://firmware.example/bmc.fwpkg",
    );
    assert_eq!(
        loggable_url("file:///tmp/bmc.fwpkg"),
        "file:///tmp/bmc.fwpkg"
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
    let mut server = mockito::Server::new_async().await;
    let source = server
        .mock("GET", "/fw.bin")
        .with_body("firmware artifact")
        .expect(1)
        .create_async()
        .await;
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    let url = format!("{}/fw.bin", server.url());
    let downloader = FirmwareDownloader::new();

    // The test runtime is single-threaded, so the download task cannot run
    // until the test yields: every call below is made while it is pending.
    for _ in 0..9 {
        assert_eq!(
            downloader.available(&filename, &url, ""),
            ArtifactStatus::Downloading,
        );
    }

    assert_eq!(
        status_after_download(&downloader, &filename, &url).await,
        ArtifactStatus::Available,
    );
    source.assert_async().await;
}

#[tokio::test]
async fn test_download_without_checksum() -> Result<(), std::io::Error> {
    let filename = Path::new("/tmp/test_firmware_without_checksum");
    let src_filename = "/tmp/test_firmware_without_checksum_src";
    let url = format!("file://{src_filename}");

    let mut srcfile = File::create(src_filename).await?;
    for i in 0..2000 {
        srcfile.write_all(format!("{i}").as_bytes()).await?;
    }
    srcfile.flush().await?;

    let _ = std::fs::remove_file(filename);
    let downloader = FirmwareDownloader::new();
    let metrics = MetricsCapture::start();

    let mut count = 0;
    loop {
        if downloader.available(filename, &url, "") != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            // >=1 rather than an exact delta: concurrent tests can add ok
            // samples, but if the ok emit vanished nothing could satisfy this.
            wait_for_downloads(&metrics, "ok", 1).await;
            let _ = std::fs::remove_file(filename);
            let _ = std::fs::remove_file(src_filename);
            return Ok(());
        }
    }
}

#[tokio::test]
async fn test_available_verifies_sha256_checksum() -> Result<(), std::io::Error> {
    let filename = Path::new("/tmp/test_firmware_sha256_checksum");
    let src_filename = "/tmp/test_firmware_sha256_checksum_src";
    let url = format!("file://{src_filename}");
    let contents = b"firmware artifact";

    let mut srcfile = File::create(src_filename).await?;
    srcfile.write_all(contents).await?;
    srcfile.flush().await?;

    let _ = std::fs::remove_file(filename);
    let downloader = FirmwareDownloader::new();
    let checksum = format!(
        " {} ",
        hex::encode(sha2::Sha256::digest(contents)).to_ascii_uppercase()
    );

    let mut count = 0;
    loop {
        if downloader.available(filename, &url, &checksum) != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            let _ = std::fs::remove_file(filename);
            let _ = std::fs::remove_file(src_filename);
            return Ok(());
        }
    }
}

#[tokio::test]
async fn test_available_rejects_stale_cache_with_wrong_sha256() -> Result<(), std::io::Error> {
    let filename = Path::new("/tmp/test_firmware_stale_cache_wrong_checksum");
    let src_filename = "/tmp/test_firmware_stale_cache_wrong_checksum_src";
    let url = format!("file://{src_filename}");
    let contents = b"fresh firmware artifact";

    let mut cached_file = File::create(filename).await?;
    cached_file.write_all(b"stale firmware artifact").await?;
    cached_file.flush().await?;
    drop(cached_file);

    let mut srcfile = File::create(src_filename).await?;
    srcfile.write_all(contents).await?;
    srcfile.flush().await?;
    drop(srcfile);

    let downloader = FirmwareDownloader::new();
    let checksum = hex::encode(sha2::Sha256::digest(contents));

    assert_eq!(
        downloader.available(filename, &url, &checksum),
        ArtifactStatus::Downloading,
    );

    let mut count = 0;
    loop {
        if downloader.available(filename, &url, &checksum) != ArtifactStatus::Available {
            tokio::time::sleep(Duration::from_millis(10)).await;
            count += 1;
            if count >= 1000 {
                panic!("Should not have taken this long");
            }
        } else {
            assert_eq!(tokio::fs::read(filename).await?, contents);
            let _ = std::fs::remove_file(filename);
            let _ = std::fs::remove_file(src_filename);
            return Ok(());
        }
    }
}

#[tokio::test]
async fn test_available_checksum_failure_does_not_publish_file() -> Result<(), std::io::Error> {
    let filename = Path::new("/tmp/test_firmware_sha256_checksum_failure");
    let src_filename = "/tmp/test_firmware_sha256_checksum_failure_src";
    let url = format!("file://{src_filename}");

    let mut srcfile = File::create(src_filename).await?;
    srcfile.write_all(b"firmware artifact").await?;
    srcfile.flush().await?;

    let _ = std::fs::remove_file(filename);
    let downloader = FirmwareDownloader::new();

    let metrics = MetricsCapture::start();
    assert_eq!(
        downloader.available(filename, &url, &"0".repeat(64)),
        ArtifactStatus::Downloading,
    );
    wait_for_downloads(&metrics, "checksum", 1).await;

    assert!(!filename.exists());
    let _ = std::fs::remove_file(src_filename);
    Ok(())
}

/// A source that cannot be opened is the fetch failure: the attempt counts
/// under `outcome="fetch"` and publishes nothing.
#[tokio::test]
async fn test_available_fetch_failure_counts() -> Result<(), std::io::Error> {
    let filename = Path::new("/tmp/test_firmware_fetch_failure");
    let src_filename = "/tmp/test_firmware_fetch_failure_missing_src";
    let url = format!("file://{src_filename}");

    let _ = std::fs::remove_file(filename);
    let _ = std::fs::remove_file(src_filename);
    let downloader = FirmwareDownloader::new();

    let metrics = MetricsCapture::start();
    assert_eq!(
        downloader.available(filename, &url, ""),
        ArtifactStatus::Downloading,
    );
    wait_for_downloads(&metrics, "fetch", 1).await;

    assert!(!filename.exists());
    Ok(())
}

/// Polls until the attempt in flight ends and returns what the next caller sees.
async fn status_after_download(
    downloader: &FirmwareDownloader,
    filename: &Path,
    url: &str,
) -> ArtifactStatus {
    for _ in 0..1000 {
        let status = downloader.available(filename, url, "");
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
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    let src_filename = temp_dir.path().join("src.bin");
    let url = format!("file://{}", src_filename.display());
    let downloader = FirmwareDownloader::new();

    assert_eq!(
        downloader.available(&filename, &url, ""),
        ArtifactStatus::Downloading
    );
    let ArtifactStatus::Failed { error } =
        status_after_download(&downloader, &filename, &url).await
    else {
        panic!("a missing source should fail the download");
    };
    assert!(
        error.contains(&url),
        "error should name the source: {error}"
    );

    std::fs::write(&src_filename, b"firmware artifact").unwrap();
    assert_eq!(
        downloader.available(&filename, &url, ""),
        ArtifactStatus::Downloading,
    );
    assert_eq!(
        status_after_download(&downloader, &filename, &url).await,
        ArtifactStatus::Available,
    );
}

/// The failure text reaches operators, so a presigned URL's query string must
/// not survive in it, including inside the HTTP client's own error.
#[tokio::test]
async fn download_failure_error_omits_url_query() {
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("fw.bin");
    // Nothing listens on port 1, so the connection is refused immediately.
    let url = "http://127.0.0.1:1/fw.bin?X-Amz-Signature=secret";
    let downloader = FirmwareDownloader::new();

    downloader.available(&filename, url, "");
    let ArtifactStatus::Failed { error } = status_after_download(&downloader, &filename, url).await
    else {
        panic!("a refused connection should fail the download");
    };
    assert!(error.contains("http://127.0.0.1:1/fw.bin"), "{error}");
    assert!(!error.contains("secret"), "{error}");
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

#[test]
fn unavailable_absent_artifact_without_url_counts_the_blocker() {
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("missing.fwpkg");
    let downloader = FirmwareDownloader::new();
    let metrics = MetricsCapture::start();

    let logs = capture_logs(|| {
        assert!(matches!(
            downloader.available(&filename, "", ""),
            ArtifactStatus::Failed { .. },
        ));
    });

    assert_eq!(logs.len(), 1);
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
}

#[test]
fn unavailable_stale_artifact_removal_failure_counts_the_blocker() {
    let temp_dir = tempfile::tempdir().unwrap();
    let filename = temp_dir.path().join("stale.fwpkg");
    std::fs::create_dir(&filename).unwrap();
    let downloader = FirmwareDownloader::new();
    let metrics = MetricsCapture::start();

    let logs = capture_logs(|| {
        assert!(matches!(
            downloader.available(
                &filename,
                "https://firmware.example/stale.fwpkg",
                "expected-checksum",
            ),
            ArtifactStatus::Failed { .. },
        ));
    });

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
        // Failure labels deliberately disjoint from the labels the
        // end-to-end download tests in this binary reach (`ok`, `checksum`,
        // `fetch`): the capture mutex serializes only capture-holding tests,
        // so a label a capture-less test can move would race these deltas.
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
