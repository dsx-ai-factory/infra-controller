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

// Coordinates downloading firmware in the background with multiple possible requestors

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use eyre::{Report, WrapErr, eyre};
use futures_util::StreamExt;
use reqwest_middleware::ClientWithMiddleware as Client;
use sha2::{Digest, Sha256};
use tokio::fs::File;

/// `ArtifactUnavailableReason` names the bounded blocker that kept an
/// artifact from reaching the download path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum ArtifactUnavailableReason {
    MissingUrl,
    StaleCacheRemovalFailed,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_firmware_artifact_unavailable_total",
    kind = counter,
    component = "carbide-firmware",
    describe = "Number of firmware artifacts unavailable before download, by reason."
)]
pub(crate) struct FirmwareArtifactUnavailable {
    reason: ArtifactUnavailableReason,
}

// Both failures record the same counter, but their existing log fields differ.
// Separate Event types keep each record intact instead of adding empty context;
// the family above holds the one description they share.
#[derive(Event)]
#[event(
    event_name = "firmware_artifact_missing_url",
    metric_family = FirmwareArtifactUnavailable,
    log = error,
    message = "Firmware artifact is missing and has no URL"
)]
pub(crate) struct FirmwareArtifactMissingUrl {
    #[label]
    pub reason: ArtifactUnavailableReason,
    #[context]
    pub firmware_path: String,
}

#[derive(Event)]
#[event(
    event_name = "firmware_stale_cached_artifact_removal_failed",
    metric_family = FirmwareArtifactUnavailable,
    log = error,
    message = "Failed to remove stale cached firmware artifact"
)]
pub(crate) struct FirmwareStaleCachedArtifactRemovalFailed {
    #[label]
    pub reason: ArtifactUnavailableReason,
    #[context]
    pub filename: String,
    #[context]
    pub error: String,
}

/// How a background firmware download attempt ended, as a bounded metric
/// label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum DownloadOutcome {
    /// Downloaded, verified, and renamed into place.
    Ok,
    /// The request never produced a response: connection, DNS, or TLS
    /// trouble (or, for `file://` sources, a source that cannot be opened).
    Fetch,
    /// The server answered, but with a non-success HTTP status.
    Status,
    /// The response body broke off mid-transfer.
    Transfer,
    /// The downloaded artifact failed SHA-256 verification.
    Checksum,
    /// Local filesystem trouble: creating the cache directory or staging
    /// file, writing downloaded bytes, or renaming the artifact into place.
    Io,
}

/// One firmware download. Every attempt records its duration; each variant
/// keeps the level that result already had.
#[derive(Event)]
#[event(
    event_name = "firmware_download_finished",
    metric_name = "carbide_firmware_download_duration_seconds",
    component = "carbide-firmware",
    metric = histogram,
    describe = "Duration of background firmware artifact downloads, by outcome; an ok attempt \
                spans fetch, checksum verification, and publish, and the _count series, split \
                by outcome, is the download and failure rate.",
    labels(outcome: DownloadOutcome),
)]
pub(crate) enum DownloadFinished {
    #[event(
        labels(outcome = DownloadOutcome::Ok),
        log = info,
        message = "Firmware download finished"
    )]
    Ok {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
    },

    #[event(
        labels(outcome = DownloadOutcome::Fetch),
        log = error,
        message = "Firmware download finished"
    )]
    Fetch {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = DownloadOutcome::Status),
        log = error,
        message = "Firmware download finished"
    )]
    Status {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = DownloadOutcome::Transfer),
        log = error,
        message = "Firmware download finished"
    )]
    Transfer {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = DownloadOutcome::Checksum),
        log = error,
        message = "Firmware download finished"
    )]
    Checksum {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = DownloadOutcome::Io),
        log = error,
        message = "Firmware download finished"
    )]
    Io {
        #[observation]
        took: Duration,
        #[context]
        url: String,
        #[context]
        filename: String,
        #[context]
        error: String,
    },
}

/// The URL as it may be logged: everything after `?` is dropped, so a
/// presigned or tokenized artifact URL never lands its credentials in the
/// log line while the location stays identifiable.
pub(crate) fn loggable_url(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}
/// A failed download attempt: the bounded cause for the metric label, plus
/// the detailed report for the log line.
struct DownloadError {
    outcome: DownloadOutcome,
    report: Report,
}

/// Tags a failure report with its bounded cause, for `map_err`.
fn fail(outcome: DownloadOutcome) -> impl FnOnce(Report) -> DownloadError {
    move |report| DownloadError { outcome, report }
}

#[derive(Clone, Debug)]
pub struct FirmwareDownloader {
    // Actual structure wrapped in an Arc so that we can clone the FirmwareDownloader and have the clones all point to one instance.
    actual: Arc<Mutex<FirmwareDownloaderActual>>,
}

#[derive(Debug)]
struct FirmwareDownloaderActual {
    downloading: HashSet<String>,
    client: Option<Client>,
}

impl Default for FirmwareDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl FirmwareDownloader {
    pub fn new() -> FirmwareDownloader {
        FirmwareDownloader {
            actual: Arc::new(Mutex::new(FirmwareDownloaderActual {
                downloading: HashSet::new(),
                client: None, // Not created until we actually need it
            })),
        }
    }

    /// available will return true if the given file is present, otherwise it will return false after starting a download in the background.
    /// Anything trying to check the same file while it is downloading will get the exact same result, but will not start a new download.
    /// It verifies the downloaded file against sha256 when a checksum is provided.
    pub fn available(&self, filename: &Path, url: &str, sha256: &str) -> bool {
        self.available_actual(filename, url, sha256, None)
    }

    // Implementation behind available(). Tests call this directly to control async timing.
    pub(crate) fn available_actual(
        &self,
        filename: &Path,
        url: &str,
        sha256: &str,
        fake_sleep: Option<Duration>,
    ) -> bool {
        match cached_file_status(filename, sha256) {
            CachedFileStatus::Available => return true,
            CachedFileStatus::NeedsDownload => {}
            CachedFileStatus::Unusable => return false,
        }

        if url.is_empty() {
            emit(FirmwareArtifactMissingUrl {
                reason: ArtifactUnavailableReason::MissingUrl,
                firmware_path: format!("{filename:?}"),
            });
            return false;
        }

        let filename_string = filename.to_str().unwrap().to_string();

        let mut state = self.actual.lock().unwrap();
        if state.downloading.contains(&filename_string) {
            // We are already downloading this
            return false;
        }

        // Slight timing hole, recheck for the file
        match cached_file_status(filename, sha256) {
            CachedFileStatus::Available => return true,
            CachedFileStatus::NeedsDownload => {}
            CachedFileStatus::Unusable => return false,
        }

        state.downloading.insert(filename_string.clone());
        if state.client.is_none() {
            // The `reqwest-tracing` middleware injects the current span's W3C trace context into
            // every outgoing request (#2438).
            state.client = Some(
                reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                    .with(reqwest_tracing::TracingMiddleware::default())
                    .build(),
            );
        }

        let filename = filename.to_path_buf();
        let url = url.to_owned();
        let sha256 = sha256.to_owned();
        let client = state.client.clone().unwrap();
        let actual = self.actual.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let dst_filename = format!("{filename_string}.download");
            let result =
                download_and_publish(&filename, &url, &dst_filename, client, fake_sleep, &sha256)
                    .await;
            if result.is_err() {
                std::fs::remove_file(&dst_filename).ok();
            }
            let (took, url, filename) = (
                started.elapsed(),
                loggable_url(&url),
                filename_string.clone(),
            );
            emit(match result {
                Ok(()) => DownloadFinished::Ok {
                    took,
                    url,
                    filename,
                },
                Err(failure) => {
                    let error = format!("{:#}", failure.report);
                    match failure.outcome {
                        DownloadOutcome::Ok | DownloadOutcome::Fetch => DownloadFinished::Fetch {
                            took,
                            url,
                            filename,
                            error,
                        },
                        DownloadOutcome::Status => DownloadFinished::Status {
                            took,
                            url,
                            filename,
                            error,
                        },
                        DownloadOutcome::Transfer => DownloadFinished::Transfer {
                            took,
                            url,
                            filename,
                            error,
                        },
                        DownloadOutcome::Checksum => DownloadFinished::Checksum {
                            took,
                            url,
                            filename,
                            error,
                        },
                        DownloadOutcome::Io => DownloadFinished::Io {
                            took,
                            url,
                            filename,
                            error,
                        },
                    }
                }
            });
            actual
                .lock()
                .unwrap()
                .clear_download_state(&filename_string);
        });
        false
    }
}

/// Downloads to the staging file, verifies the artifact against the expected
/// checksum, and renames it into place. Failures come back tagged with the
/// bounded cause the metric label uses.
async fn download_and_publish(
    filename: &Path,
    url: &String,
    dst_filename: &String,
    client: Client,
    fake_sleep: Option<Duration>,
    sha256: &str,
) -> Result<(), DownloadError> {
    download(filename, url, dst_filename, client, fake_sleep).await?;
    verify_sha256(dst_filename, sha256)
        .wrap_err(format!(
            "downloaded artifact from {} failed verification",
            loggable_url(url)
        ))
        .map_err(fail(DownloadOutcome::Checksum))?;
    std::fs::rename(dst_filename, filename)
        .wrap_err(format!(
            "unable to rename {dst_filename} to {}",
            filename.display()
        ))
        .map_err(fail(DownloadOutcome::Io))?;
    Ok(())
}

impl FirmwareDownloaderActual {
    fn clear_download_state(&mut self, filename: &String) {
        self.downloading.remove(filename);
    }
}

enum CachedFileStatus {
    Available,
    NeedsDownload,
    Unusable,
}

fn cached_file_status(filename: &Path, sha256: &str) -> CachedFileStatus {
    let filename_str = filename.to_string_lossy();

    if !filename.exists() {
        return CachedFileStatus::NeedsDownload;
    }

    match verify_sha256(&filename_str, sha256) {
        Ok(()) => CachedFileStatus::Available,
        Err(err) => {
            tracing::warn!(
                filename = %filename.display(),
                error = %err,
                "Cached firmware artifact failed checksum verification",
            );

            if let Err(err) = std::fs::remove_file(filename) {
                emit(FirmwareStaleCachedArtifactRemovalFailed {
                    reason: ArtifactUnavailableReason::StaleCacheRemovalFailed,
                    filename: filename.display().to_string(),
                    error: err.to_string(),
                });
                return CachedFileStatus::Unusable;
            }

            CachedFileStatus::NeedsDownload
        }
    }
}

async fn download(
    filename: &Path,
    url: &String,
    dst_filename: &String,
    client: Client,
    fake_sleep: Option<Duration>,
) -> Result<(), DownloadError> {
    // Actual downloader.  We aren't able to return errors to callers here, we just print to the log, and will retry on the next request.
    let dirname = match Path::parent(filename) {
        Some(x) => x,
        None => {
            return Err(fail(DownloadOutcome::Io)(eyre!(
                "could not find dirname of {}",
                filename.to_string_lossy()
            )));
        }
    };

    std::fs::create_dir_all(dirname)
        .wrap_err(format!("unable to create directory {}", dirname.display()))
        .map_err(fail(DownloadOutcome::Io))?;
    let mut dst_file = File::create(dst_filename)
        .await
        .wrap_err(format!("unable to create file {dst_filename}"))
        .map_err(fail(DownloadOutcome::Io))?;

    if let Some(duration) = fake_sleep {
        // For testing only, wait a given amount of time then write an empty file
        tokio::time::sleep(duration).await;
        return Ok(());
    }

    if url.starts_with("file://") {
        // Just copies a local file, for testing
        let src_filename = url.strip_prefix("file:/").unwrap(); // Leave the second / for the root
        let mut src_file = File::open(src_filename)
            .await
            .wrap_err(format!(
                "FirmwareDownloader could not open source {}",
                loggable_url(url)
            ))
            .map_err(fail(DownloadOutcome::Fetch))?;
        return tokio::io::copy(&mut src_file, &mut dst_file)
            .await
            .map(|_| ())
            .map_err(|e| {
                fail(DownloadOutcome::Transfer)(eyre!(
                    "FirmwareDownloader had problems saving file from {}: {e}",
                    loggable_url(url)
                ))
            });
    }

    let res = client
        .get(url)
        .send()
        .await
        .wrap_err(format!(
            "FirmwareDownloader got error trying to download {}",
            loggable_url(url)
        ))
        .map_err(fail(DownloadOutcome::Fetch))?;
    if !res.status().is_success() {
        return Err(fail(DownloadOutcome::Status)(eyre!(
            "FirmwareDownloader got non-success status trying to download {}: {}",
            loggable_url(url),
            res.status()
        )));
    }
    let mut body = res.bytes_stream();
    while let Some(segment) = body.next().await {
        match segment {
            Err(e) => {
                return Err(fail(DownloadOutcome::Transfer)(eyre!(
                    "FirmwareDownloader had problems downloading {}: {e}",
                    loggable_url(url)
                )));
            }
            Ok(segment) => {
                tokio::io::copy(&mut segment.as_ref(), &mut dst_file)
                    .await
                    .wrap_err(format!(
                        "FirmwareDownloader had problems saving file from {}",
                        loggable_url(url)
                    ))
                    .map_err(fail(DownloadOutcome::Io))?;
            }
        }
    }

    // Success
    Ok(())
}

/// Checks if the given filename uses the given checksum. This is not meant to be security,
/// it's to check against download corruption or retrieving the wrong thing (such as if the vendor changed the URL).
/// We expect the hardware vendor to have done their own signing to ensure that firmware is not compromised.
fn verify_sha256(filename: &str, checksum: &str) -> Result<(), Report> {
    let checksum = checksum.trim().to_ascii_lowercase();
    if checksum.is_empty() {
        return Ok(());
    }

    let mut file = std::fs::File::open(filename)?;

    let mut context = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }

    let checksum_actual = hex::encode(context.finalize());

    if checksum_actual != checksum {
        return Err(eyre!(
            "checksum mismatch: expected {checksum} downloaded {checksum_actual}"
        ));
    }
    Ok(())
}
