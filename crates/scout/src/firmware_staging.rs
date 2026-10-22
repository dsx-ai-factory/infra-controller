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

//! Digest-checked temporary firmware inputs shared by Scout's script and NIC consumers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use carbide_instrument::emit;
use carbide_utils::none_if_empty::NoneIfEmpty;
use futures_util::TryStreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::metrics::{
    FirmwareDownloadKind, FirmwareDownloadNextAction, ScoutFirmwareDownloadAttemptFailed,
};

// Three retries allow four requests, with 1, 2, and 4 second backoffs.
// The caller's download deadline includes every request and backoff.
const DOWNLOAD_MAX_RETRIES: u32 = 3;

/// Retain the transport source while identifying the input without its credentials.
#[derive(Debug, thiserror::Error)]
#[error("download failed for {url}: {source}")]
struct FirmwareDownloadFailed {
    url: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

/// `FirmwareStaging` keeps checked inputs alive until their consumer finishes.
/// Dropping it removes successful and partial downloads together.
pub(super) struct FirmwareStaging {
    work_dir: tempfile::TempDir,
    download_dir: PathBuf,
}

impl FirmwareStaging {
    pub(super) fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let work_dir = tempfile::tempdir()?;
        let download_dir = work_dir.path().join("downloads");
        // Create directories before yielding so cancellation cannot leave a
        // filesystem worker creating a child after its parent was removed.
        std::fs::create_dir(&download_dir)?;
        Ok(Self {
            work_dir,
            download_dir,
        })
    }

    pub(super) fn work_dir(&self) -> &Path {
        self.work_dir.path()
    }

    pub(super) fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    /// `stage` returns a path only after its SHA-256 matches.
    ///
    /// The deadline covers requests, response bodies, and retry backoffs;
    /// hashing follows the download and is not part of that deadline. Errors
    /// and cancellation leave files owned by this directory until it is dropped.
    /// Each input keeps its URL basename, but a collision gets its own directory
    /// so later inputs cannot overwrite bytes an earlier caller has checked.
    /// The mutable borrow keeps destination reservations and downloads ordered.
    /// Invalid URLs and missing filenames fail before the first attempt.
    pub(super) async fn stage(
        &mut self,
        client: &reqwest::Client,
        url: &str,
        sha256: &str,
        kind: FirmwareDownloadKind,
        download_timeout: Duration,
    ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        let parsed = reqwest::Url::parse(url)?;
        if !is_downloadable_firmware_url(&parsed) {
            return Err("firmware URL must use HTTP or HTTPS and include a host".into());
        }
        let url_context = LoggableFirmwareUrl::new(url);
        let segment = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .none_if_empty()
            .ok_or_else(|| format!("cannot extract filename from URL: {}", url_context.as_str()))?;
        let filename = Path::new(segment)
            .file_name()
            .ok_or_else(|| format!("invalid filename in URL: {}", url_context.as_str()))?;
        let target_dir = match kind {
            FirmwareDownloadKind::Script => self.work_dir(),
            FirmwareDownloadKind::Artifact => self.download_dir(),
        };
        let destination = target_dir.join(filename);
        let destination = if destination.try_exists()? {
            // Keep the ordinary DOWNLOAD_DIR/basename layout when possible.
            // The enclosing TempDir owns this child even after keep().
            tempfile::Builder::new()
                .prefix("input-")
                .tempdir_in(target_dir)?
                .keep()
                .join(filename)
        } else {
            destination
        };

        tokio::time::timeout(
            download_timeout,
            download_file_with_retries(client, &parsed, &destination, kind, &url_context),
        )
        .await
        .map_err(|_| match kind {
            FirmwareDownloadKind::Script => format!(
                "script download timed out after {} seconds",
                download_timeout.as_secs()
            ),
            FirmwareDownloadKind::Artifact => format!(
                "download timed out for {} after {} seconds",
                url_context.as_str(),
                download_timeout.as_secs()
            ),
        })?
        .map_err(|source| FirmwareDownloadFailed {
            url: url_context.as_str().to_string(),
            source,
        })?;

        let actual = sha256_file(&destination).await?;
        if actual != sha256 {
            let input_kind = match kind {
                FirmwareDownloadKind::Script => "script ",
                FirmwareDownloadKind::Artifact => "",
            };
            return Err(format!(
                "checksum mismatch for {input_kind}{}: expected {sha256}, got {actual}",
                url_context.as_str()
            )
            .into());
        }
        match kind {
            FirmwareDownloadKind::Script => tracing::info!(
                script_path = ?destination,
                "Firmware script downloaded and verified",
            ),
            FirmwareDownloadKind::Artifact => tracing::info!(
                artifact_url = %url_context.as_str(),
                "Firmware artifact checksum verified",
            ),
        }
        Ok(destination)
    }
}

pub(super) struct LoggableFirmwareUrl {
    value: String,
    task_forms: Vec<String>,
}

fn is_downloadable_firmware_url(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
}

fn remove_url_credentials(url: &mut reqwest::Url) {
    // `http` and `https` URLs are always base URLs. `Url` only rejects
    // username and password changes for cannot-be-a-base schemes;
    // `LoggableFirmwareUrl::new` rejects those before calling us, so these
    // results are safe to ignore.
    url.set_username("").ok();
    url.set_password(None).ok();
}

impl LoggableFirmwareUrl {
    fn invalid() -> Self {
        Self {
            value: "<invalid firmware URL>".to_string(),
            task_forms: Vec::new(),
        }
    }

    pub(super) fn new(raw: &str) -> Self {
        let Ok(parsed) = reqwest::Url::parse(raw) else {
            return Self::invalid();
        };
        if !is_downloadable_firmware_url(&parsed) {
            return Self::invalid();
        }

        let mut task_forms = vec![raw.to_string(), parsed.to_string()];
        for remove_fragment in [false, true] {
            for remove_query in [false, true] {
                let mut form = parsed.clone();
                if remove_fragment {
                    form.set_fragment(None);
                }
                if remove_query {
                    form.set_query(None);
                }
                task_forms.push(form.to_string());

                remove_url_credentials(&mut form);
                task_forms.push(form.to_string());
            }
        }

        let mut loggable = parsed;
        remove_url_credentials(&mut loggable);
        loggable.set_query(None);
        loggable.set_fragment(None);
        let value = loggable.to_string();

        task_forms.retain(|form| !form.is_empty() && form != &value);
        task_forms
            .sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        task_forms.dedup();

        Self { value, task_forms }
    }

    pub(super) fn redact_from(&self, text: &str) -> String {
        let mut redacted = text.to_string();
        for form in &self.task_forms {
            redacted = redacted.replace(form, &self.value);
        }
        redacted
    }

    pub(super) fn as_str(&self) -> &str {
        &self.value
    }
}

async fn download_file_with_retries(
    client: &reqwest::Client,
    url: &reqwest::Url,
    destination: &Path,
    kind: FirmwareDownloadKind,
    url_context: &LoggableFirmwareUrl,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tryhard::retry_fn(|| download_file(client, url, destination))
        .retries(DOWNLOAD_MAX_RETRIES)
        .exponential_backoff(Duration::from_secs(1))
        .on_retry(|attempt, next_delay, error| {
            let next_action = if next_delay.is_some() {
                FirmwareDownloadNextAction::Retry
            } else {
                FirmwareDownloadNextAction::GiveUp
            };
            let delay = next_delay.unwrap_or_default();
            let error = url_context.redact_from(&error.to_string());
            emit(ScoutFirmwareDownloadAttemptFailed {
                kind,
                next_action,
                attempt: i64::from(attempt),
                url: url_context.as_str().to_string(),
                error,
                retry_delay_seconds: delay.as_secs_f64(),
            });
            std::future::ready(())
        })
        .await
}

/// `download_file` truncates only this input's reserved destination on retry.
async fn download_file(
    client: &reqwest::Client,
    url: &reqwest::Url,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url_context = LoggableFirmwareUrl::new(url.as_str());
    tracing::info!(
        url = %url_context.as_str(),
        destination = ?destination,
        "Downloading firmware input",
    );

    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(reqwest::Error::without_url)?
        .error_for_status()
        .map_err(reqwest::Error::without_url)?;
    // error_for_status rejects only 4xx and 5xx. A redirect that the client
    // did not follow is not a successful artifact response, even if its body
    // happens to match the requested digest.
    if !response.status().is_success() {
        return Err(format!("unexpected HTTP status {}", response.status()).into());
    }
    let mut stream = response.bytes_stream();

    // Opening synchronously prevents a cancelled Tokio open from recreating
    // a file after FirmwareStaging has removed the temporary directory.
    // Subsequent async writes own only this open descriptor, not its path.
    let file = std::fs::File::create(destination)?;
    let mut file = tokio::fs::File::from_std(file);
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

/// `sha256_file` yields between 8 KiB reads rather than hashing a whole
/// firmware image synchronously on the async executor.
async fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::http::StatusCode;
    use axum::routing::get;
    use carbide_instrument::testing::{capture_logs, capture_logs_async};
    use carbide_test_support::{Check, check_values};
    use futures_util::{StreamExt, stream};
    use rpc::scout_firmware_upgrade::{FileArtifact, ScoutFirmwareUpgradeTask};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use super::*;

    /// Serve real response bodies so retry and cancellation tests exercise
    /// reqwest and filesystem work instead of a substituted download function.
    async fn start_server(app: Router) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), server)
    }

    /// Join the intentionally long-lived HTTP fixture before its runtime exits.
    async fn stop_server(server: JoinHandle<()>) {
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn loggable_firmware_url_excludes_credentials() {
        check_values(
            [
                Check {
                    scenario: "path remains available for diagnostics",
                    input: "https://firmware.example/images/fw.bin",
                    expect: "https://firmware.example/images/fw.bin".to_string(),
                },
                Check {
                    scenario: "userinfo query and fragment are removed",
                    input: "https://user:password@firmware.example/images/fw.bin?token=secret#download",
                    expect: "https://firmware.example/images/fw.bin".to_string(),
                },
                Check {
                    scenario: "invalid locations do not echo caller input",
                    input: "not a URL?token=secret",
                    expect: "<invalid firmware URL>".to_string(),
                },
            ],
            |url| LoggableFirmwareUrl::new(url).as_str().to_string(),
        );
    }

    #[test]
    fn task_url_redaction_excludes_only_task_credentials() {
        struct ErrorCase {
            error: &'static str,
            url: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "raw URL is redacted",
                    input: ErrorCase {
                        error: "request failed for https://user:password@firmware.example/fw.bin?token=secret#download",
                        url: "https://user:password@firmware.example/fw.bin?token=secret#download",
                    },
                    expect: "request failed for https://firmware.example/fw.bin".to_string(),
                },
                Check {
                    scenario: "canonicalized URL is redacted independently",
                    input: ErrorCase {
                        error: "request failed for https://user:password@firmware.example/fw.bin?token=secret",
                        url: "HTTPS://user:password@FIRMWARE.EXAMPLE:443/fw.bin?token=secret",
                    },
                    expect: "request failed for https://firmware.example/fw.bin".to_string(),
                },
                Check {
                    scenario: "short invalid URL does not rewrite parse errors",
                    input: ErrorCase {
                        error: "relative URL without a base",
                        url: "a",
                    },
                    expect: "relative URL without a base".to_string(),
                },
                Check {
                    scenario: "empty URL does not rewrite unrelated errors",
                    input: ErrorCase {
                        error: "connection reset",
                        url: "",
                    },
                    expect: "connection reset".to_string(),
                },
                Check {
                    scenario: "unrelated URLs remain untouched",
                    input: ErrorCase {
                        error: "upstream=https://user:password@other.example/fw.bin?token=secret",
                        url: "https://firmware.example/task.bin?token=task-secret",
                    },
                    expect: "upstream=https://user:password@other.example/fw.bin?token=secret"
                        .to_string(),
                },
            ],
            |case| LoggableFirmwareUrl::new(case.url).redact_from(case.error),
        );
    }

    #[test]
    fn malformed_firmware_url_parse_errors_do_not_echo_the_input() {
        const RAW_URL: &str = "MALFORMED_FIRMWARE_TASK_URL";

        let parse_error = reqwest::Url::parse(RAW_URL).unwrap_err().to_string();
        let url_context = LoggableFirmwareUrl::new(RAW_URL);
        let loggable_error = url_context.redact_from(&parse_error);

        assert_eq!(url_context.as_str(), "<invalid firmware URL>");
        assert_eq!(loggable_error, parse_error);
        assert!(!loggable_error.contains(RAW_URL));
    }

    /// Deterministic URL admission failures do not enter the transport retry
    /// loop or emit failed-attempt events.
    #[test]
    fn invalid_locations_fail_before_any_download_attempt() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let logs = capture_logs(|| {
            check_values(
                [
                    // Parse failure cannot identify a downloadable resource.
                    Check {
                        scenario: "malformed URL",
                        input: "not a URL",
                        expect: false,
                    },
                    // A valid URL with an unsupported transport is not retryable.
                    Check {
                        scenario: "unsupported scheme",
                        input: "ftp://firmware.example/image.bin",
                        expect: false,
                    },
                    // An HTTP directory URL cannot name the staged file.
                    Check {
                        scenario: "missing filename",
                        input: "https://firmware.example/",
                        expect: false,
                    },
                ],
                |url| {
                    runtime.block_on(async {
                        FirmwareStaging::new()
                            .unwrap()
                            .stage(
                                &reqwest::Client::new(),
                                url,
                                "unused digest",
                                FirmwareDownloadKind::Artifact,
                                Duration::from_secs(1),
                            )
                            .await
                            .is_ok()
                    })
                },
            );
        });

        assert!(logs.iter().all(|log| {
            log.field("event_name") != Some("scout_firmware_download_attempt_failed")
        }));
    }

    /// Different URLs may share a basename; the script must consume each
    /// checked input's bytes in task order rather than the last download twice.
    #[tokio::test]
    async fn script_consumes_colliding_artifacts_without_overwriting_checked_bytes() {
        let script = r#"#!/bin/sh
set -eu
test "$#" = "2"
test "$(basename "$1")" = "image.bin"
test "$(basename "$2")" = "image.bin"
test "$1" != "$2"
case "$1" in "$DOWNLOAD_DIR"/*) ;; *) exit 1 ;; esac
case "$2" in "$DOWNLOAD_DIR"/*) ;; *) exit 1 ;; esac
printf 'first=%s second=%s' "$(cat "$1")" "$(cat "$2")"
"#;
        let app = Router::new()
            .route("/scripts/upgrade.sh", get(move || async move { script }))
            .route("/first/image.bin", get(|| async { "first-image" }))
            .route("/second/image.bin", get(|| async { "second-image" }));
        let (base, server) = start_server(app).await;
        let artifact = |path: &str, content: &str| FileArtifact {
            url: format!("{base}{path}"),
            sha256: hex::encode(Sha256::digest(content.as_bytes())),
        };
        let task = ScoutFirmwareUpgradeTask {
            upgrade_task_id: "colliding-artifacts".into(),
            component_type: "cpld".into(),
            target_version: "1.2.3".into(),
            script: Some(artifact("/scripts/upgrade.sh", script)),
            execution_timeout_seconds: 30,
            artifact_download_timeout_seconds: 30,
            // Distinct payloads make a later overwrite observable at execution.
            file_artifacts: vec![
                artifact("/first/image.bin", "first-image"),
                artifact("/second/image.bin", "second-image"),
            ],
        };

        let result =
            crate::firmware_upgrade::handle_firmware_upgrade(&reqwest::Client::new(), &task).await;

        assert!(result.success, "upgrade failed: {}", result.error);
        assert_eq!(result.stdout, "first=first-image second=second-image");
        stop_server(server).await;
    }

    /// A transient HTTP failure must retry the same input and return checked
    /// bytes from the later successful response.
    #[tokio::test]
    async fn staging_retries_a_transient_failure_and_checks_the_successful_body() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let request_attempts = attempts.clone();
        let app = Router::new().route(
            "/image.bin",
            get(move || {
                let request_attempts = request_attempts.clone();
                async move {
                    if request_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        (StatusCode::SERVICE_UNAVAILABLE, "retry this request")
                    } else {
                        (StatusCode::OK, "checked image")
                    }
                }
            }),
        );
        let (base, server) = start_server(app).await;
        let mut staging = FirmwareStaging::new().unwrap();
        let path = staging
            .stage(
                &reqwest::Client::new(),
                &format!("{base}/image.bin"),
                &hex::encode(Sha256::digest(b"checked image")),
                FirmwareDownloadKind::Artifact,
                Duration::from_secs(10),
            )
            .await
            .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(&path).unwrap(), b"checked image");
        assert_eq!(path, staging.download_dir().join("image.bin"));
        stop_server(server).await;
    }

    /// A later digest failure must not leak either the earlier checked input
    /// or the rejected bytes when their shared owner is dropped.
    #[tokio::test]
    async fn staging_removes_all_inputs_after_a_later_digest_failure() {
        let app = Router::new()
            .route("/image.bin", get(|| async { "checked image" }))
            .route("/config.bin", get(|| async { "wrong config" }));
        let (base, server) = start_server(app).await;
        let client = reqwest::Client::new();
        let mut staging = FirmwareStaging::new().unwrap();
        let work_dir = staging.work_dir().to_path_buf();
        let image = staging
            .stage(
                &client,
                &format!("{base}/image.bin"),
                &hex::encode(Sha256::digest(b"checked image")),
                FirmwareDownloadKind::Artifact,
                Duration::from_secs(10),
            )
            .await
            .unwrap();

        // The second URL responds successfully; only the digest rejects it.
        let error = staging
            .stage(
                &client,
                &format!("{base}/config.bin"),
                &hex::encode(Sha256::digest(b"expected config")),
                FirmwareDownloadKind::Artifact,
                Duration::from_secs(10),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        assert_eq!(std::fs::read(&image).unwrap(), b"checked image");
        assert!(staging.download_dir().join("config.bin").exists());

        drop(staging);
        assert!(!work_dir.exists());
        stop_server(server).await;
    }

    /// A body that has reached disk remains owned during both deadline expiry
    /// and caller cancellation; neither may leave files or schedule another retry.
    #[tokio::test]
    async fn partial_download_cleanup_on_deadline_or_cancellation() {
        enum Stop {
            Deadline,
            Cancellation,
        }

        for (scenario, stop) in [
            // Expiry after a write proves the deadline includes the response body.
            ("body deadline", Stop::Deadline),
            // Dropping a pending future proves caller cancellation releases ownership.
            ("caller cancellation", Stop::Cancellation),
        ] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let request_attempts = attempts.clone();
            let app = Router::new().route(
                "/image.bin",
                get(move || {
                    request_attempts.fetch_add(1, Ordering::SeqCst);
                    async {
                        Body::from_stream(
                            stream::once(async {
                                Ok::<_, std::io::Error>(Bytes::from_static(b"partial image"))
                            })
                            .chain(stream::pending()),
                        )
                    }
                }),
            );
            let (base, server) = start_server(app).await;
            let client = reqwest::Client::new();
            let mut staging = FirmwareStaging::new().unwrap();
            let work_dir = staging.work_dir().to_path_buf();
            let destination = staging.download_dir().join("image.bin");
            let url = format!("{base}/image.bin");
            let expected = hex::encode(Sha256::digest(b"complete image"));
            let mut download = Box::pin(staging.stage(
                &client,
                &url,
                &expected,
                FirmwareDownloadKind::Artifact,
                Duration::from_secs(30),
            ));

            // Wait for actual filesystem output before advancing time or
            // dropping the future; stopping during connect proves neither case.
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    result = &mut download => panic!("{scenario}: incomplete body finished: {result:?}"),
                    () = async {
                        loop {
                            if std::fs::read(&destination).is_ok_and(|bytes| bytes == b"partial image") {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    } => {}
                }
            })
            .await
            .expect("partial download should reach disk");

            tokio::time::pause();
            match stop {
                Stop::Deadline => {
                    tokio::time::advance(Duration::from_secs(30)).await;
                    let error = download.await.unwrap_err();
                    assert!(
                        error.to_string().contains("download timed out"),
                        "{scenario}"
                    );
                }
                Stop::Cancellation => drop(download),
            }
            assert_eq!(attempts.load(Ordering::SeqCst), 1, "{scenario}");

            drop(staging);
            assert!(!work_dir.exists(), "{scenario}");
            tokio::time::advance(Duration::from_secs(10)).await;
            assert_eq!(attempts.load(Ordering::SeqCst), 1, "{scenario}");
            stop_server(server).await;
            tokio::time::resume();
        }
    }

    /// The caller's deadline also expires during retry backoff, without
    /// leaving a retry scheduled after the staging operation has returned.
    #[tokio::test]
    async fn staging_deadline_expires_during_retry_backoff() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let request_attempts = attempts.clone();
        let app = Router::new().route(
            "/image.bin",
            get(move || {
                request_attempts.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::SERVICE_UNAVAILABLE }
            }),
        );
        let (base, server) = start_server(app).await;
        let client = reqwest::Client::new();
        let mut staging = FirmwareStaging::new().unwrap();
        let work_dir = staging.work_dir().to_path_buf();
        let url = format!("{base}/image.bin");
        // One subscriber must remain attached throughout the future, since
        // reqwest and Tokio can retain spans between polls. The retry event
        // proves we entered backoff before the 750 ms deadline expired;
        // observing only the HTTP request would not establish that phase.
        // The first backoff lasts one second, beyond the enclosing deadline.
        let (result, logs) = capture_logs_async(staging.stage(
            &client,
            &url,
            "unused digest",
            FirmwareDownloadKind::Artifact,
            Duration::from_millis(750),
        ))
        .await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains("download timed out"));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            logs.iter()
                .filter(|log| {
                    log.field("event_name") == Some("scout_firmware_download_attempt_failed")
                        && log.field("next_action") == Some("retry")
                })
                .count(),
            1
        );

        drop(staging);
        assert!(!work_dir.exists());
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        stop_server(server).await;
    }
}
