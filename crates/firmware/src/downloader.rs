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

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use eyre::{Report, WrapErr, eyre};
use futures_util::StreamExt;
use reqwest_middleware::ClientWithMiddleware as Client;
use reqwest_tracing::ReqwestOtelSpanBackend;
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

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

/// `loggable_url` removes userinfo, query parameters, and fragments from
/// artifact URLs before they reach logs or operator-visible errors.
pub(crate) fn loggable_url(url: &str) -> String {
    let Ok(url) = url::Url::parse(url) else {
        return "<invalid firmware URL>".to_string();
    };
    format!(
        "{}{}",
        &url[..url::Position::BeforeUsername],
        &url[url::Position::BeforeHost..url::Position::AfterPath],
    )
}

/// `redacted_error_chain` formats diagnostic sources without changing the typed
/// errors or exposing the credentials in their URLs.
fn redacted_error_chain(error: &(dyn Error + 'static)) -> String {
    let chain: Vec<_> = std::iter::successors(Some(error), |&error| error.source()).collect();
    let mut message = chain
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    // Custom redirect policies can return another reqwest error with its own URL.
    // Keep the typed sources intact and redact each URL when presenting them.
    let mut urls: Vec<_> = chain
        .into_iter()
        .filter_map(|error| {
            error
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::url)
                .or_else(|| {
                    error
                        .downcast_ref::<reqwest_middleware::Error>()
                        .and_then(reqwest_middleware::Error::url)
                })
        })
        .collect();
    // A redirected URL can extend the original query. Redact it first so
    // replacing the shorter URL cannot leave the added credentials behind.
    urls.sort_unstable_by_key(|url| std::cmp::Reverse(url.as_str().len()));
    for url in urls {
        message = message.replace(url.as_str(), &loggable_url(url.as_str()));
    }
    message
}

/// Retains HTTP request tracing without recording raw URL credentials.
struct FirmwareDownloadSpanBackend;

impl ReqwestOtelSpanBackend for FirmwareDownloadSpanBackend {
    fn on_request_start(
        request: &reqwest::Request,
        extensions: &mut http::Extensions,
    ) -> tracing::Span {
        reqwest_tracing::DefaultSpanBackend::on_request_start(request, extensions)
    }

    fn on_request_end(
        span: &tracing::Span,
        result: &reqwest_middleware::Result<reqwest::Response>,
        _: &mut http::Extensions,
    ) {
        match result {
            Ok(response) => reqwest_tracing::default_on_request_success(span, response),
            Err(error) => {
                // The default backend records raw errors before the downloader
                // receives them, including URLs in custom redirect-policy errors.
                let message = redacted_error_chain(error);
                span.record(reqwest_tracing::OTEL_STATUS_CODE, "ERROR");
                span.record(reqwest_tracing::ERROR_MESSAGE, &message);
                span.record(reqwest_tracing::ERROR_CAUSE_CHAIN, &message);
                if let Some(status) = error.status() {
                    span.record(reqwest_tracing::HTTP_RESPONSE_STATUS_CODE, status.as_u16());
                }
            }
        }
    }
}

/// Applies the downloader's tracing and redaction policy to the supplied HTTP client.
fn download_client(client: reqwest::Client) -> Client {
    reqwest_middleware::ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::<
            FirmwareDownloadSpanBackend,
        >::new())
        .build()
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

/// What [`FirmwareDownloader::available`] found for an artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactStatus {
    /// The file is present and satisfies the supplied checksum, when provided.
    Available,
    /// An attempt is active and no cached file satisfies this caller's checksum yet.
    Downloading,
    /// The artifact cannot be made available. URL credentials, query parameters,
    /// and fragments are removed from `error`, so it can be shown to operators.
    Failed {
        /// Diagnostic detail with URL credentials removed.
        error: String,
    },
}

/// Coordinates background firmware downloads and cache verification across
/// cloned handles.
#[derive(Clone, Debug)]
pub struct FirmwareDownloader {
    // Actual structure wrapped in an Arc so that we can clone the FirmwareDownloader and have the clones all point to one instance.
    actual: Arc<Mutex<FirmwareDownloaderActual>>,
}

#[derive(Debug)]
struct FirmwareDownloaderActual {
    downloading: HashSet<PathBuf>,
    // Error of the last failed attempt per file, kept until one caller is told about it.
    failures: HashMap<PathBuf, String>,
    client: Option<Client>,
}

/// Holds one destination's registration until all filesystem work stops.
struct DownloadAttempt {
    actual: Arc<Mutex<FirmwareDownloaderActual>>,
    filename: PathBuf,
    span: tracing::Span,
    dispatch: tracing::Dispatch,
}

impl DownloadAttempt {
    /// Restores the initiating context during blocking work and deferred cleanup.
    fn in_scope<T>(&self, operation: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || self.span.in_scope(operation))
    }
}

impl Drop for DownloadAttempt {
    fn drop(&mut self) {
        // Removing this registration is safe after poisoning and avoids a
        // second panic if the attempt is already unwinding.
        self.actual
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .downloading
            .remove(&self.filename);
    }
}

/// Removes a partial file before releasing the destination for another attempt.
struct StagedDownload {
    path: PathBuf,
    attempt: Arc<DownloadAttempt>,
    published: bool,
}

impl Drop for StagedDownload {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // `attempt` stays alive until after cleanup. A new attempt must not
        // reuse this path while a cancelled blocking worker can still remove it.
        self.attempt.in_scope(|| {
            if let Err(error) = std::fs::remove_file(&self.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "Failed to remove partial firmware download",
                );
            }
        });
    }
}

impl StagedDownload {
    /// Transfers cleanup to the checksum worker until its verified file is returned.
    async fn verify(self, checksum: String) -> Result<Self, DownloadError> {
        // Dropping the awaiting future cannot stop `spawn_blocking`. The
        // worker owns cleanup and destination registration until verification finishes;
        // only the awaiting owner is allowed to publish the verified file.
        let context = self.attempt.clone();
        tokio::task::spawn_blocking(move || {
            context.in_scope(|| {
                verify_sha256(&self.path, &checksum).map_err(fail(DownloadOutcome::Checksum))?;
                Ok(self)
            })
        })
        .await
        .wrap_err("firmware checksum worker failed")
        .map_err(fail(DownloadOutcome::Io))?
    }
}

impl Default for FirmwareDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl FirmwareDownloader {
    /// Creates an empty coordinator; its shared HTTP client is initialized on
    /// the first download.
    pub fn new() -> FirmwareDownloader {
        FirmwareDownloader {
            actual: Arc::new(Mutex::new(FirmwareDownloaderActual {
                downloading: HashSet::new(),
                failures: HashMap::new(),
                client: None, // Not created until we actually need it
            })),
        }
    }

    /// Reports whether the given file is present, starting a background download
    /// when it is not.
    /// A caller sharing an active attempt gets `Available` if a read-only cache
    /// check satisfies its checksum, otherwise `Downloading`; it never removes
    /// the cache entry.
    /// A failed download is reported as `Failed` to the next caller only; the
    /// check after that starts a new download, so a caller retries by calling again.
    /// It verifies cached and downloaded files against SHA-256 when a checksum
    /// is provided. Cache verification is awaited on a blocking worker; a started
    /// download continues in the background across polling calls. Cancelling
    /// cache verification releases its destination registration after the
    /// blocking worker finishes.
    pub async fn available(&self, filename: &Path, url: &str, sha256: &str) -> ArtifactStatus {
        let span = tracing::Span::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let attempt = {
            let mut state = self
                .actual
                .lock()
                .expect("firmware downloader state mutex poisoned");
            if state.downloading.insert(filename.to_path_buf()) {
                Some(Arc::new(DownloadAttempt {
                    actual: self.actual.clone(),
                    filename: filename.to_path_buf(),
                    span: span.clone(),
                    dispatch: dispatch.clone(),
                }))
            } else {
                None
            }
        };
        let cached_status = {
            let attempt = attempt.clone();
            let filename = filename.to_path_buf();
            let checksum = sha256.to_owned();
            tokio::task::spawn_blocking(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    span.in_scope(|| match attempt {
                        Some(attempt) => cached_file_status(&attempt.filename, &checksum),
                        None => {
                            // The active attempt owns removal and publication. Other
                            // callers can still use a valid cache entry while it runs.
                            if filename.exists() && verify_sha256(&filename, &checksum).is_ok() {
                                CachedFileStatus::Available
                            } else {
                                CachedFileStatus::NeedsDownload
                            }
                        }
                    })
                })
            })
            .await
        };
        match cached_status {
            Ok(CachedFileStatus::Available) => return ArtifactStatus::Available,
            Ok(CachedFileStatus::NeedsDownload) => {}
            Ok(CachedFileStatus::Unusable { error }) => return ArtifactStatus::Failed { error },
            Err(error) => {
                tracing::warn!(
                    filename = %filename.display(),
                    %error,
                    "Firmware cache verification worker failed",
                );
                return ArtifactStatus::Failed {
                    error: format!("firmware cache verification worker failed: {error}"),
                };
            }
        }

        let Some(attempt) = attempt else {
            return ArtifactStatus::Downloading;
        };

        if url.is_empty() {
            emit(FirmwareArtifactMissingUrl {
                reason: ArtifactUnavailableReason::MissingUrl,
                firmware_path: format!("{filename:?}"),
            });
            return ArtifactStatus::Failed {
                error: format!(
                    "firmware artifact {} is missing and has no URL",
                    filename.display()
                ),
            };
        }

        let client = {
            let mut state = self
                .actual
                .lock()
                .expect("firmware downloader state mutex poisoned");
            if let Some(error) = state.failures.remove(filename) {
                return ArtifactStatus::Failed { error };
            }
            state
                .client
                .get_or_insert_with(|| download_client(reqwest::Client::new()))
                .clone()
        };

        let source_url = url.to_owned();
        let sha256 = sha256.to_owned();
        tokio::spawn(
            async move {
                let started = Instant::now();
                let result =
                    download_and_publish(attempt.clone(), &source_url, &client, &sha256).await;
                let failure = result.err().map(|failure| {
                    (
                        failure.outcome,
                        redacted_error_chain(failure.report.as_ref()),
                    )
                });
                let (took, url, filename) = (
                    started.elapsed(),
                    loggable_url(&source_url),
                    attempt.filename.display().to_string(),
                );
                emit(match &failure {
                    None => DownloadFinished::Ok {
                        took,
                        url,
                        filename,
                    },
                    Some((outcome, error)) => {
                        let error = error.clone();
                        match outcome {
                            DownloadOutcome::Ok | DownloadOutcome::Fetch => {
                                DownloadFinished::Fetch {
                                    took,
                                    url,
                                    filename,
                                    error,
                                }
                            }
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
                if let Some((_, error)) = failure {
                    // Record the failure before dropping the registration so
                    // the next caller sees it before starting a retry.
                    attempt
                        .actual
                        .lock()
                        .expect("firmware downloader state mutex poisoned")
                        .failures
                        .insert(attempt.filename.clone(), error);
                }
            }
            .in_current_span()
            .with_current_subscriber(),
        );
        ArtifactStatus::Downloading
    }
}

/// Downloads to the staging file, verifies the artifact against the expected
/// checksum, and renames it into place. Failures come back tagged with the
/// bounded cause the metric label uses.
async fn download_and_publish(
    attempt: Arc<DownloadAttempt>,
    url: &str,
    client: &Client,
    sha256: &str,
) -> Result<(), DownloadError> {
    // File creation and its cleanup guard share one blocking operation. Cancelling
    // the await cannot leave a late-created file behind after cleanup has run.
    let context = attempt.clone();
    let (staged, mut file) = tokio::task::spawn_blocking(move || {
        context.in_scope(|| {
            let dirname = attempt.filename.parent().ok_or_else(|| {
                fail(DownloadOutcome::Io)(eyre!("firmware destination has no parent directory"))
            })?;
            std::fs::create_dir_all(dirname)
                .wrap_err_with(|| format!("unable to create directory {}", dirname.display()))
                .map_err(fail(DownloadOutcome::Io))?;
            let mut path = attempt.filename.as_os_str().to_owned();
            path.push(".download");
            let staged = StagedDownload {
                path: path.into(),
                attempt,
                published: false,
            };
            let file = std::fs::File::create(&staged.path)
                .wrap_err_with(|| format!("unable to create file {}", staged.path.display()))
                .map_err(fail(DownloadOutcome::Io))?;
            Ok::<_, DownloadError>((staged, File::from_std(file)))
        })
    })
    .await
    .wrap_err("firmware staging worker failed")
    .map_err(fail(DownloadOutcome::Io))??;
    download(url, client, &mut file).await?;
    file.flush()
        .await
        .wrap_err("unable to flush downloaded firmware")
        .map_err(fail(DownloadOutcome::Io))?;
    drop(file);
    let mut staged = staged.verify(sha256.to_owned()).await?;
    // No await after verification: a cancelled owner cannot leave behind a
    // blocking worker that publishes the file after a new attempt starts.
    std::fs::rename(&staged.path, &staged.attempt.filename)
        .wrap_err(format!(
            "unable to rename {} to {}",
            staged.path.display(),
            staged.attempt.filename.display()
        ))
        .map_err(fail(DownloadOutcome::Io))?;
    staged.published = true;
    Ok(())
}

enum CachedFileStatus {
    Available,
    NeedsDownload,
    Unusable { error: String },
}

fn cached_file_status(filename: &Path, sha256: &str) -> CachedFileStatus {
    if !filename.exists() {
        return CachedFileStatus::NeedsDownload;
    }

    match verify_sha256(filename, sha256) {
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
                return CachedFileStatus::Unusable {
                    error: format!(
                        "failed to remove stale cached firmware artifact {}: {err}",
                        filename.display()
                    ),
                };
            }

            CachedFileStatus::NeedsDownload
        }
    }
}

async fn download(url: &str, client: &Client, dst_file: &mut File) -> Result<(), DownloadError> {
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
        return tokio::io::copy(&mut src_file, dst_file)
            .await
            .map(|_| ())
            .wrap_err_with(|| format!("unable to copy firmware from {}", loggable_url(url)))
            .map_err(fail(DownloadOutcome::Transfer));
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
        let segment = segment
            .wrap_err_with(|| format!("unable to download firmware from {}", loggable_url(url)))
            .map_err(fail(DownloadOutcome::Transfer))?;
        dst_file
            .write_all(&segment)
            .await
            .wrap_err_with(|| {
                format!(
                    "FirmwareDownloader had problems saving file from {}",
                    loggable_url(url)
                )
            })
            .map_err(fail(DownloadOutcome::Io))?;
    }

    // Success
    Ok(())
}

/// Checks if the given filename uses the given checksum. This is not meant to be security,
/// it's to check against download corruption or retrieving the wrong thing (such as if the vendor changed the URL).
/// We expect the hardware vendor to have done their own signing to ensure that firmware is not compromised.
fn verify_sha256(filename: &Path, checksum: &str) -> Result<(), Report> {
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

#[cfg(test)]
mod lifecycle_tests {
    use std::future::Future;
    use std::pin::Pin;

    use tokio::io::AsyncReadExt;
    use tracing::Instrument;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// Registers the private transfer owner so tests can cancel its awaited work directly.
    fn claim(downloader: &FirmwareDownloader, filename: &Path) -> Arc<DownloadAttempt> {
        assert!(
            downloader
                .actual
                .lock()
                .unwrap()
                .downloading
                .insert(filename.to_path_buf())
        );
        Arc::new(DownloadAttempt {
            actual: downloader.actual.clone(),
            filename: filename.to_path_buf(),
            span: tracing::Span::current(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
        })
    }

    /// Occupies the single blocking thread until the test releases its queued work.
    fn blocking_gate() -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            // Dropping the sender also releases the worker if an assertion fails.
            let _ = release_rx.recv();
        });
        (started_rx, release_tx, worker)
    }

    /// Waits for an observed lifecycle change without relying on a fixed sleep.
    async fn wait_until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("download lifecycle did not finish");
    }

    /// Dropping a stalled transfer removes its partial file and permits a later download.
    #[tokio::test]
    async fn cancelling_a_stalled_body_removes_staging_and_releases_singleflight() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fw.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(connection.read_u8().await.unwrap());
            }
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                .await
                .unwrap();
            connection.read_to_end(&mut Vec::new()).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let filename = directory.path().join("fw.bin");
        let partial = directory.path().join("fw.bin.download");
        let downloader = FirmwareDownloader::new();
        let attempt = claim(&downloader, &filename);
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let source = url.clone();
        let transfer =
            tokio::spawn(async move { download_and_publish(attempt, &source, &client, "").await });
        wait_until(|| partial.metadata().is_ok_and(|metadata| metadata.len() > 0)).await;
        assert_eq!(
            downloader.available(&filename, &url, "").await,
            ArtifactStatus::Downloading
        );

        transfer.abort();
        assert!(matches!(transfer.await, Err(error) if error.is_cancelled()));
        assert!(!partial.exists());
        assert!(!filename.exists());
        assert!(downloader.actual.lock().unwrap().downloading.is_empty());
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();

        let source = directory.path().join("source.bin");
        std::fs::write(&source, b"firmware").unwrap();
        let url = format!("file://{}", source.display());
        assert_eq!(
            downloader.available(&filename, &url, "").await,
            ArtifactStatus::Downloading
        );
        wait_until(|| downloader.actual.lock().unwrap().downloading.is_empty()).await;
        assert_eq!(
            downloader.available(&filename, &url, "").await,
            ArtifactStatus::Available
        );
        assert_eq!(std::fs::read(filename).unwrap(), b"firmware");
    }

    /// Cancellation retains ownership through queued filesystem work and cannot publish later.
    #[test]
    fn cancelled_workers_keep_ownership_until_they_finish() {
        #[derive(Clone, Copy, Debug)]
        enum Stage {
            // A cancelled cache check must finish removing stale content before another attempt starts.
            Cached,
            // Staging creation can outlive cancellation before the transfer has a file guard.
            Staging,
            // A cancelled transfer must clean staging without ever publishing its verified result.
            Downloaded,
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        for stage in [Stage::Cached, Stage::Staging, Stage::Downloaded] {
            runtime.block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let filename = directory.path().join("fw.bin");
                let partial = directory.path().join("fw.bin.download");
                let downloader = FirmwareDownloader::new();
                let checksum = hex::encode(Sha256::digest(b"firmware"));
                let mut server = mockito::Server::new_async().await;
                let response = server
                    .mock("GET", "/fw.bin")
                    .with_body("firmware")
                    .create_async()
                    .await;
                let url = format!("{}/fw.bin", server.url());
                let client = download_client(reqwest::Client::new());
                let (started_rx, mut release_tx, mut blocker) = blocking_gate();
                started_rx.await.unwrap();

                // Each future is the actual publication owner. Holding the
                // blocking pool makes its queued filesystem work observable.
                let mut checking: Pin<Box<dyn Future<Output = ()> + '_>> = match stage {
                    Stage::Cached => {
                        std::fs::write(&filename, b"stale firmware").unwrap();
                        Box::pin(async {
                            downloader
                                .available(&filename, "file:///unused", &checksum)
                                .await;
                        })
                    }
                    Stage::Staging | Stage::Downloaded => {
                        let attempt = claim(&downloader, &filename);
                        Box::pin(async move {
                            download_and_publish(attempt, &url, &client, &checksum)
                                .await
                                .unwrap_or_else(|failure| {
                                    panic!("{}", redacted_error_chain(failure.report.as_ref()))
                                });
                        })
                    }
                };
                assert!(
                    futures_util::poll!(checking.as_mut()).is_pending(),
                    "{stage:?}"
                );

                if matches!(stage, Stage::Downloaded) {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        assert!(
                            Instant::now() < deadline,
                            "transfer never reached verification"
                        );
                        // Run submitted filesystem work, then occupy the
                        // worker again before polling the owner. Once all
                        // body bytes are on disk, the write has completed;
                        // the next poll flushes it and queues verification.
                        let (started_rx, next_release, next_blocker) = blocking_gate();
                        release_tx.send(()).unwrap();
                        blocker.await.unwrap();
                        started_rx.await.unwrap();
                        release_tx = next_release;
                        blocker = next_blocker;
                        let body_written =
                            std::fs::read(&partial).is_ok_and(|bytes| bytes == b"firmware");
                        assert!(futures_util::poll!(checking.as_mut()).is_pending());
                        if body_written {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    response.assert_async().await;
                }
                drop(checking);
                assert!(
                    downloader
                        .actual
                        .lock()
                        .unwrap()
                        .downloading
                        .contains(&filename),
                    "{stage:?}"
                );
                assert!(
                    match stage {
                        Stage::Cached => filename.exists(),
                        Stage::Staging => !partial.exists(),
                        Stage::Downloaded => partial.exists(),
                    },
                    "{stage:?}"
                );
                if !matches!(stage, Stage::Cached) {
                    assert!(
                        !filename.exists(),
                        "{stage:?} published before verification"
                    );
                }

                release_tx.send(()).unwrap();
                blocker.await.unwrap();
                wait_until(|| downloader.actual.lock().unwrap().downloading.is_empty()).await;
                assert!(
                    !filename.exists(),
                    "cancelled {stage:?} verification must not publish"
                );
                assert!(!partial.exists(), "{stage:?}");

                // A real public retry proves cleanup released both the path
                // and its registration, rather than merely hiding the old work.
                let source = directory.path().join("source.bin");
                std::fs::write(&source, b"replacement firmware").unwrap();
                let source_url = format!("file://{}", source.display());
                assert_eq!(
                    downloader.available(&filename, &source_url, "").await,
                    ArtifactStatus::Downloading,
                    "{stage:?}"
                );
                wait_until(|| downloader.actual.lock().unwrap().downloading.is_empty()).await;
                assert_eq!(
                    downloader.available(&filename, &source_url, "").await,
                    ArtifactStatus::Available,
                    "{stage:?}"
                );
                assert_eq!(
                    std::fs::read(&filename).unwrap(),
                    b"replacement firmware",
                    "{stage:?}"
                );
                assert!(!partial.exists(), "{stage:?}");
            });
        }
    }

    /// Overlapping warm-cache readers await verification and both receive the existing artifact.
    #[test]
    fn concurrent_warm_cache_checks_both_report_available() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let filename = directory.path().join("fw.bin");
            std::fs::write(&filename, b"firmware").unwrap();
            let checksum = hex::encode(Sha256::digest(b"firmware"));
            let downloader = FirmwareDownloader::new();
            let (started_rx, release_tx, blocker) = blocking_gate();
            started_rx.await.unwrap();
            let mut first = Box::pin(downloader.available(&filename, "", &checksum));
            let mut second = Box::pin(downloader.available(&filename, "", &checksum));

            // Neither caller can infer a cache miss from another reader's
            // in-flight checksum; both wait for the same valid on-disk bytes.
            assert!(futures_util::poll!(first.as_mut()).is_pending());
            assert!(futures_util::poll!(second.as_mut()).is_pending());
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            assert_eq!(first.await, ArtifactStatus::Available);
            assert_eq!(second.await, ArtifactStatus::Available);
            assert!(downloader.actual.lock().unwrap().downloading.is_empty());
            assert_eq!(std::fs::read(filename).unwrap(), b"firmware");
        });
    }

    /// Cleanup still removes staging and releases ownership after the state mutex is poisoned.
    #[test]
    fn cleanup_does_not_panic_again_after_state_mutex_poisoning() {
        let directory = tempfile::tempdir().unwrap();
        let filename = directory.path().join("fw.bin");
        let partial = directory.path().join("fw.bin.download");
        std::fs::write(&partial, b"partial").unwrap();
        let downloader = FirmwareDownloader::new();
        let staged = StagedDownload {
            path: partial.clone(),
            attempt: claim(&downloader, &filename),
            published: false,
        };
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = downloader.actual.lock().unwrap();
            panic!("poison the state mutex before cleanup");
        }));
        assert!(poisoned.is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(staged))).is_ok());
        assert!(!partial.exists());
        assert!(
            downloader
                .actual
                .lock()
                .unwrap_err()
                .into_inner()
                .downloading
                .is_empty()
        );
    }

    /// Captures initial fields, later updates, and event ancestry from the same subscriber.
    #[derive(Clone, Default)]
    struct SpanRecords(Arc<Mutex<Vec<TraceRecord>>>);

    /// Associates captured fields with their source and enclosing span names.
    struct TraceRecord {
        kind: &'static str,
        fields: String,
        ancestors: Vec<String>,
    }

    /// Collects every field through the visitor's common debug representation.
    #[derive(Default)]
    struct Fields(String);

    impl tracing::field::Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;

            writeln!(self.0, "{field}={value:?}").unwrap();
        }
    }

    impl<S> tracing_subscriber::Layer<S> for SpanRecords
    where
        S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attributes: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields::default();
            attributes.record(&mut fields);
            self.0.lock().unwrap().push(TraceRecord {
                kind: "span",
                fields: fields.0,
                ancestors: context
                    .span(id)
                    .unwrap()
                    .scope()
                    .from_root()
                    .map(|span| span.name().to_string())
                    .collect(),
            });
        }

        fn on_record(
            &self,
            id: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields::default();
            values.record(&mut fields);
            self.0.lock().unwrap().push(TraceRecord {
                kind: "record",
                fields: fields.0,
                ancestors: context
                    .span(id)
                    .unwrap()
                    .scope()
                    .from_root()
                    .map(|span| span.name().to_string())
                    .collect(),
            });
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push(TraceRecord {
                kind: "event",
                fields: fields.0,
                ancestors: context
                    .event_scope(event)
                    .into_iter()
                    .flat_map(|scope| scope.from_root())
                    .map(|span| span.name().to_string())
                    .collect(),
            });
        }
    }

    /// Real redirect failures preserve typed sources while redacting every diagnostic span field.
    #[tokio::test]
    async fn redirect_errors_retain_sources_without_leaking_urls_into_spans() {
        for (scenario, nested_request_error, raw_secrets) in [
            // An unsupported redirect retains the target credentials in its typed error.
            (
                "unsupported redirect",
                false,
                vec!["redirect-user", "redirect-password", "redirect-secret"],
            ),
            // A nested URL extends the outer query; shorter-first replacement would leak its suffix.
            (
                "nested request error",
                true,
                vec!["request-secret", "redirect-secret"],
            ),
        ] {
            let mut server = mockito::Server::new_async().await;
            let url = format!("{}/fw.bin?request-secret", server.url());
            let request_client = if nested_request_error {
                // A custom redirect failure can wrap a second request error. Its URL
                // extends the first, so replacing the shorter URL first leaks the suffix.
                let failed_response = server
                    .mock("GET", "/fw.bin")
                    .match_query("request-secret&next=redirect-secret")
                    .with_status(500)
                    .create_async()
                    .await;
                let nested = reqwest::Client::new()
                    .get(format!("{url}&next=redirect-secret"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap_err();
                failed_response.assert_async().await;
                let nested = Mutex::new(Some(nested));
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                        attempt.error(nested.lock().unwrap().take().expect("one redirect attempt"))
                    }))
                    .build()
                    .unwrap()
            } else {
                reqwest::Client::new()
            };
            let redirect = server
                .mock("GET", "/fw.bin")
                .match_query(mockito::Matcher::Any)
                .with_status(302)
                .with_header(
                    "location",
                    concat!(
                        "ftp://redirect-user:redirect-password@firmware.example/fw.bin",
                        "?redirect-secret#fragment-secret"
                    ),
                )
                .create_async()
                .await;
            let directory = tempfile::tempdir().unwrap();
            let filename = directory.path().join("fw.bin");
            let downloader = FirmwareDownloader::new();
            let records = SpanRecords::default();
            let subscriber = tracing_subscriber::registry().with(records.clone());
            let client = download_client(request_client);
            let failure = async {
                download_and_publish(claim(&downloader, &filename), &url, &client, "").await
            }
            .with_subscriber(subscriber)
            .await
            .unwrap_err();
            assert!(
                matches!(failure.outcome, DownloadOutcome::Fetch),
                "{scenario}"
            );
            let report = failure.report;
            assert!(
                matches!(
                    report.downcast_ref::<reqwest_middleware::Error>(),
                    Some(reqwest_middleware::Error::Reqwest(_))
                ),
                "{scenario}"
            );
            if nested_request_error {
                assert!(
                    report.chain().any(|source| source.is::<reqwest::Error>()),
                    "{scenario}"
                );
            }
            let raw = format!("{report:#}");
            for secret in raw_secrets {
                assert!(
                    raw.contains(secret),
                    "{scenario}: missing raw control {secret}: {raw}"
                );
            }
            let message = redacted_error_chain(report.as_ref());
            assert!(message.contains("/fw.bin"), "{scenario}: {message}");
            assert!(
                message.contains("trying to download"),
                "{scenario}: {message}"
            );
            redirect.assert_async().await;
            let records = records.0.lock().unwrap();
            assert!(
                records
                    .iter()
                    .any(|record| record.kind == "span" && record.fields.contains("GET")),
                "{scenario}"
            );
            let recorded = records
                .iter()
                .map(|record| record.fields.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                recorded.contains("error.cause_chain"),
                "{scenario}: {recorded}"
            );
            assert!(recorded.contains("/fw.bin"), "{scenario}: {recorded}");
            for secret in [
                "request-secret",
                "redirect-secret",
                "redirect-user",
                "redirect-password",
                "fragment-secret",
            ] {
                assert!(!message.contains(secret), "{scenario}: {message}");
                assert!(!recorded.contains(secret), "{scenario}: {recorded}");
            }
            drop(records);
            assert!(!filename.exists(), "{scenario}");
            assert!(
                !directory.path().join("fw.bin.download").exists(),
                "{scenario}"
            );
            assert!(
                downloader.actual.lock().unwrap().downloading.is_empty(),
                "{scenario}"
            );
        }
    }

    /// Public cache checks and background downloads retain the caller span across worker boundaries.
    #[tokio::test]
    async fn public_download_events_keep_the_request_parent() {
        let directory = tempfile::tempdir().unwrap();
        let filename = directory.path().join("fw.bin");
        let unusable = directory.path().join("unusable.bin");
        // A directory cannot be hashed or removed as a stale file, so the
        // cache event must come from the real blocking cache-check worker.
        std::fs::create_dir(&unusable).unwrap();
        let mut server = mockito::Server::new_async().await;
        let response = server
            .mock("GET", "/fw.bin")
            .with_body("firmware")
            .create_async()
            .await;
        let url = format!("{}/fw.bin", server.url());
        let downloader = FirmwareDownloader::new();
        let records = SpanRecords::default();
        let subscriber = tracing_subscriber::registry().with(records.clone());
        async {
            async {
                assert!(matches!(
                    downloader.available(&unusable, &url, "checksum").await,
                    ArtifactStatus::Failed { .. }
                ));
                assert_eq!(
                    downloader.available(&filename, &url, "").await,
                    ArtifactStatus::Downloading
                );
                wait_until(|| downloader.actual.lock().unwrap().downloading.is_empty()).await;
                assert_eq!(
                    downloader.available(&filename, &url, "").await,
                    ArtifactStatus::Available
                );
            }
            .instrument(tracing::info_span!("firmware_poll"))
            .await;
        }
        .with_subscriber(subscriber)
        .await;
        response.assert_async().await;
        assert_eq!(std::fs::read(filename).unwrap(), b"firmware");

        let records = records.0.lock().unwrap();
        for (scenario, kind, field) in [
            // Cache verification leaves the async task, but its failure still belongs to that poll.
            (
                "cache worker",
                "event",
                "firmware_stale_cached_artifact_removal_failed",
            ),
            // The HTTP middleware span is created inside the detached transfer task.
            ("request middleware", "span", "GET"),
            // Completion must preserve that parent after the blocking checksum worker returns.
            ("download completion", "event", "firmware_download_finished"),
        ] {
            let record = records
                .iter()
                .find(|record| record.kind == kind && record.fields.contains(field))
                .unwrap_or_else(|| panic!("missing {scenario} record"));
            assert!(
                record.ancestors.iter().any(|name| name == "firmware_poll"),
                "{scenario}: {:?}",
                record.ancestors
            );
        }
    }
}
