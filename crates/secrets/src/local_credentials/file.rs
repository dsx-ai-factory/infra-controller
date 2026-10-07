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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use carbide_instrument::{Event, LabelValue, emit};
use notify::{PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::CredentialSnapshot;
use crate::SecretsError;
use crate::credentials::{CredentialKey, CredentialReader, Credentials};

const DEFAULT_FILE_POLL_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_CREDENTIALS_FILE_PATH: &str = "secrets.yaml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum StaticCredentialWatcherOperation {
    PrimaryWatch,
    PrimaryWatchSetup,
    PollWatch,
    Reload,
}

/// The static-credential file watcher hit an error. Each variant is the
/// operation that failed.
#[derive(Event)]
#[event(
    event_name = "static_credential_watcher_failed",
    metric_name = "carbide_static_credential_watcher_failures_total",
    component = "nico-api",
    metric = counter,
    describe = "Number of static credential watcher failures, by operation.",
    labels(operation: StaticCredentialWatcherOperation),
)]
enum StaticCredentialWatcherFailed {
    #[event(
        labels(operation = StaticCredentialWatcherOperation::PrimaryWatch),
        log = warn,
        message = "primary static credential watcher error"
    )]
    PrimaryWatch {
        #[context]
        error: String,
    },

    #[event(
        labels(operation = StaticCredentialWatcherOperation::PrimaryWatchSetup),
        log = warn,
        message = "primary static credential watcher unavailable; relying on polling"
    )]
    PrimaryWatchSetup {
        #[context]
        error: String,
        #[context]
        poll_interval_secs: f64,
    },

    #[event(
        labels(operation = StaticCredentialWatcherOperation::PollWatch),
        log = warn,
        message = "credentials file watcher event error"
    )]
    PollWatch {
        #[context]
        error: String,
    },

    #[event(
        labels(operation = StaticCredentialWatcherOperation::Reload),
        log = warn,
        message = "failed to reload credentials file"
    )]
    Reload {
        #[context]
        error: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchEventDelivery {
    Primary,
    Poll,
}

fn forward_watch_event(delivery: WatchEventDelivery, tx: &WatchEventSender, event: notify::Event) {
    if let Err(err) = tx.blocking_send(event) {
        // A closed receiver means this watcher is tearing down, not that file
        // observation or credential reload failed. Keep this as a plain WARN
        // so shutdown cannot increment the live-failure counter.
        match delivery {
            WatchEventDelivery::Primary => {
                tracing::warn!(
                    error = %err,
                    "failed to send static credential watch event",
                );
            }
            WatchEventDelivery::Poll => {
                tracing::warn!(
                    error = %err,
                    "failed to send static credential poll event",
                );
            }
        }
    }
}

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
pub struct FileCredentialsConfig {
    pub enabled: Option<bool>,
    pub path: Option<PathBuf>,
    pub poll_interval: Option<Duration>,
}

impl FileCredentialsConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
            .or_else(|| {
                std::env::var("CARBIDE_CREDENTIALS_FILE_ENABLED")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(false)
    }

    pub fn path(&self) -> PathBuf {
        self.path
            .clone()
            .or_else(|| {
                std::env::var("CARBIDE_CREDENTIALS_FILE_PATH")
                    .ok()
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CREDENTIALS_FILE_PATH))
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval.unwrap_or(DEFAULT_FILE_POLL_INTERVAL)
    }
}

/// Carries file events to the reload task. Watcher errors never travel on it:
/// each callback reports them where they occur, because `PollWatcher::watch`
/// reports a missing path by calling its handler synchronously on the calling
/// thread, where blocking on the channel is not allowed.
type WatchEventSender = mpsc::Sender<notify::Event>;

/// Arms the kernel-backed watcher that forwards create and modify events for
/// `path` to `tx` as they happen.
fn start_primary_watcher(path: &Path, tx: WatchEventSender) -> notify::Result<RecommendedWatcher> {
    let mut primary = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| match res {
            Ok(event) if event.kind.is_create() || event.kind.is_modify() => {
                forward_watch_event(WatchEventDelivery::Primary, &tx, event);
            }
            Ok(_) => {}
            Err(err) => {
                emit(StaticCredentialWatcherFailed::PrimaryWatch {
                    error: err.to_string(),
                });
            }
        },
        notify::Config::default(),
    )?;
    primary.watch(path, RecursiveMode::NonRecursive)?;
    Ok(primary)
}

pub struct FileCredentialsWatcher {
    credentials: Arc<ArcSwap<CredentialSnapshot>>,
    /// `None` when the kernel-backed watcher could not be armed at startup;
    /// the poll watcher then detects every change on its own.
    _primary_watcher: Option<RecommendedWatcher>,
    _secondary_watcher: PollWatcher,
}

impl FileCredentialsWatcher {
    pub async fn new(config: FileCredentialsConfig) -> Result<Self, SecretsError> {
        Self::new_with_primary_watcher(config, start_primary_watcher).await
    }

    /// `start_primary` arms the kernel-backed watcher. Its failure is not
    /// fatal: the poll watcher re-reads the file every `poll_interval` and
    /// compares contents, so it detects every change on its own. A node that
    /// cannot provide another inotify instance therefore degrades to polling
    /// instead of refusing to start. The degradation is only reported once
    /// the initial load succeeds, so a missing or malformed file still fails
    /// startup with its own error rather than a polling warning.
    async fn new_with_primary_watcher(
        config: FileCredentialsConfig,
        start_primary: impl FnOnce(&Path, WatchEventSender) -> notify::Result<RecommendedWatcher>,
    ) -> Result<Self, SecretsError> {
        let path = config.path();
        let poll_interval = config.poll_interval();
        if poll_interval.is_zero() {
            return Err(SecretsError::GenericError(eyre::eyre!(
                "credentials.file.poll_interval must be greater than zero"
            )));
        }
        // Fail on a missing file before arming any watcher. `PollWatcher::watch`
        // registers nothing for a missing path yet returns `Ok`, and the kernel
        // watch failure for it would otherwise read as an inotify shortage.
        tokio::fs::metadata(&path).await.map_err(|err| {
            SecretsError::GenericError(eyre::Report::new(err).wrap_err(format!(
                "credentials file {} is not accessible",
                path.display()
            )))
        })?;
        let (tx, mut rx) = mpsc::channel(4);

        let primary = start_primary(&path, tx.clone());

        let mut secondary = PollWatcher::new(
            move |res: notify::Result<notify::Event>| match res {
                Ok(event) => forward_watch_event(WatchEventDelivery::Poll, &tx, event),
                Err(err) => emit(StaticCredentialWatcherFailed::PollWatch {
                    error: err.to_string(),
                }),
            },
            notify::Config::default()
                .with_poll_interval(poll_interval)
                .with_compare_contents(true),
        )
        .map_err(|err| SecretsError::GenericError(err.into()))?;

        secondary
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|err| SecretsError::GenericError(err.into()))?;

        // Arm the watchers before reading the initial snapshot. Otherwise a
        // replacement between the read and watcher registration could remain
        // invisible until the file changes again. Events received while this
        // read is in flight remain queued for the reload task below.
        let initial = match Self::load_file(&path).await {
            Ok(initial) => initial,
            Err(error) => {
                // Close the receiver before dropping the watchers so callbacks
                // blocked on a full channel can exit during watcher teardown.
                drop(rx);
                return Err(error);
            }
        };
        let primary = match primary {
            Ok(primary) => Some(primary),
            Err(err) => {
                emit(StaticCredentialWatcherFailed::PrimaryWatchSetup {
                    error: err.to_string(),
                    poll_interval_secs: poll_interval.as_secs_f64(),
                });
                None
            }
        };
        let credentials = Arc::new(ArcSwap::from_pointee(initial));
        let watched_path = path.clone();
        let credentials_clone = credentials.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if !event
                    .paths
                    .iter()
                    .any(|event_path| event_path.file_name() == watched_path.file_name())
                {
                    continue;
                }

                match Self::load_file(&watched_path).await {
                    Ok(updated) => {
                        credentials_clone.store(Arc::new(updated));
                    }
                    Err(err) => {
                        emit(StaticCredentialWatcherFailed::Reload {
                            error: err.to_string(),
                        });
                    }
                }
            }
        });

        Ok(Self {
            credentials,
            _primary_watcher: primary,
            _secondary_watcher: secondary,
        })
    }

    async fn load_file(path: &Path) -> Result<CredentialSnapshot, SecretsError> {
        let content = tokio::fs::read(path)
            .await
            .map_err(|err| SecretsError::GenericError(err.into()))?;

        if let Ok(parsed) = serde_json::from_slice::<CredentialSnapshot>(&content) {
            return Ok(parsed);
        }

        let parsed = serde_yaml::from_slice::<CredentialSnapshot>(&content).map_err(|err| {
            let message = match err.location() {
                Some(location) => format!(
                    "failed to parse static credential file as JSON or YAML at line {}, column {}; parse details redacted",
                    location.line(),
                    location.column()
                ),
                None => "failed to parse static credential file as JSON or YAML; parse details redacted"
                    .to_string(),
            };
            SecretsError::GenericError(eyre::eyre!(message))
        })?;
        Ok(parsed)
    }
}

#[async_trait]
impl CredentialReader for FileCredentialsWatcher {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        self.credentials.load().get_credentials(key).await
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs, capture_logs_async};
    use carbide_test_support::{Check, check_values};
    use tempfile::tempdir;

    use super::*;
    use crate::credentials::{CredentialKey, CredentialType, Credentials};

    #[tokio::test]
    async fn loads_json_file_and_reloads_on_change() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.json");
        tokio::fs::write(
            &file_path,
            r#"{
  "dpu_uefi_site_default": {
    "username": "root",
    "password": "json1"
  }
}"#,
        )
        .await
        .expect("write initial json file");

        let provider = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path.clone()),
            poll_interval: Some(Duration::from_secs(1)),
            ..Default::default()
        })
        .await
        .expect("create file provider");

        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };

        let first = provider
            .get_credentials(&key)
            .await
            .expect("load first value");
        assert_eq!(
            first,
            Some(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "json1".to_string(),
            })
        );

        tokio::fs::write(
            &file_path,
            r#"{
  "dpu_uefi_site_default": {
    "username": "root",
    "password": "json2"
  }
}"#,
        )
        .await
        .expect("update json file");
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let second = provider
            .get_credentials(&key)
            .await
            .expect("load reloaded value");
        assert_eq!(
            second,
            Some(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "json2".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn reloads_ufm_credentials_after_atomic_file_replacement() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.yaml");
        tokio::fs::write(
            &file_path,
            r#"ufm_auth_by_fabric:
  default:
    username: ignored-by-ufm
    password: token-before-rotation
"#,
        )
        .await
        .expect("write initial credentials file");

        let provider = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path.clone()),
            poll_interval: Some(Duration::from_millis(50)),
            ..Default::default()
        })
        .await
        .expect("create file provider");
        let key = CredentialKey::UfmAuth {
            fabric: "default".to_string(),
        };

        let replacement_path = dir.path().join("credentials.next.yaml");
        tokio::fs::write(
            &replacement_path,
            r#"ufm_auth_by_fabric:
  default:
    username: ignored-by-ufm
    password: token-after-rotation
"#,
        )
        .await
        .expect("write replacement credentials file");
        tokio::fs::rename(&replacement_path, &file_path)
            .await
            .expect("atomically replace credentials file");

        let expected = Some(Credentials::UsernamePassword {
            username: "ignored-by-ufm".to_string(),
            password: "token-after-rotation".to_string(),
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if provider
                    .get_credentials(&key)
                    .await
                    .expect("read reloaded UFM credentials")
                    == expected
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("watcher must reload atomically replaced UFM credentials");
    }

    fn primary_watcher_unavailable(
        _path: &Path,
        _tx: WatchEventSender,
    ) -> notify::Result<RecommendedWatcher> {
        // EMFILE: what `inotify_init1` returns once the per-user instance
        // limit `fs.inotify.max_user_instances` is exhausted.
        Err(notify::Error::io(std::io::Error::from_raw_os_error(24)))
    }

    #[derive(Clone, Copy)]
    struct PrimaryWatcherCase {
        scenario: &'static str,
        start_primary: fn(&Path, WatchEventSender) -> notify::Result<RecommendedWatcher>,
        /// Substring of the single fallback warning's `error` field, or
        /// `None` when the healthy path must stay silent.
        expect_fallback_error: Option<&'static str>,
    }

    const FALLBACK_MESSAGE: &str =
        "primary static credential watcher unavailable; relying on polling";

    #[tokio::test]
    async fn primary_watcher_failure_falls_back_to_polling() {
        let metrics = MetricsCapture::start();
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        let cases = [
            PrimaryWatcherCase {
                scenario: "kernel watcher armed",
                start_primary: start_primary_watcher,
                expect_fallback_error: None,
            },
            PrimaryWatcherCase {
                scenario: "kernel watcher unavailable",
                start_primary: primary_watcher_unavailable,
                expect_fallback_error: Some("os error 24"),
            },
        ];
        for case in cases {
            let scenario = case.scenario;
            let dir = tempdir().expect("create temp dir");
            let file_path = dir.path().join("credentials.yaml");
            tokio::fs::write(
                &file_path,
                "dpu_uefi_site_default:\n  username: root\n  password: before\n",
            )
            .await
            .expect("write initial credentials file");

            let fallback_label = [("operation", "primary_watch_setup")];
            let counted_before = metrics.counter_delta(WATCHER_FAILURE_METRIC, &fallback_label);
            let (provider, logs) =
                capture_logs_async(FileCredentialsWatcher::new_with_primary_watcher(
                    FileCredentialsConfig {
                        path: Some(file_path.clone()),
                        poll_interval: Some(Duration::from_millis(50)),
                        ..Default::default()
                    },
                    case.start_primary,
                ))
                .await;
            let provider = provider
                .unwrap_or_else(|err| panic!("{scenario}: construction must succeed: {err}"));
            let counted =
                metrics.counter_delta(WATCHER_FAILURE_METRIC, &fallback_label) - counted_before;
            let fallback_logs = logs
                .iter()
                .filter(|log| log.message == FALLBACK_MESSAGE)
                .collect::<Vec<_>>();
            match case.expect_fallback_error {
                None => {
                    assert!(fallback_logs.is_empty(), "{scenario}: must not warn");
                    assert_eq!(counted, 0.0, "{scenario}: must not count a failure");
                }
                Some(expected_error) => {
                    assert_eq!(fallback_logs.len(), 1, "{scenario}: warns exactly once");
                    let log = fallback_logs[0];
                    assert_eq!(log.level, tracing::Level::WARN, "{scenario}");
                    assert_eq!(
                        log.field("operation"),
                        Some("primary_watch_setup"),
                        "{scenario}"
                    );
                    assert_eq!(log.field("poll_interval_secs"), Some("0.05"), "{scenario}");
                    assert!(
                        log.field("error")
                            .is_some_and(|error| error.contains(expected_error)),
                        "{scenario}: error field names the OS error, got {:?}",
                        log.field("error")
                    );
                    assert_eq!(counted, 1.0, "{scenario}: counts one failure");
                }
            }

            assert_eq!(
                provider
                    .get_credentials(&key)
                    .await
                    .expect("read initial credentials"),
                Some(Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "before".to_string(),
                }),
                "{scenario}: initial snapshot loads"
            );

            let replacement_path = dir.path().join("credentials.next.yaml");
            tokio::fs::write(
                &replacement_path,
                "dpu_uefi_site_default:\n  username: root\n  password: after\n",
            )
            .await
            .expect("write replacement credentials file");
            tokio::fs::rename(&replacement_path, &file_path)
                .await
                .expect("atomically replace credentials file");
            let expected = Some(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "after".to_string(),
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if provider
                        .get_credentials(&key)
                        .await
                        .expect("read reloaded credentials")
                        == expected
                    {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{scenario}: watcher must reload the replaced file"));
        }
    }

    #[tokio::test]
    async fn missing_file_fails_startup_without_watcher_diagnostics() {
        let metrics = MetricsCapture::start();
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("does-not-exist.yaml");
        let result = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path),
            ..Default::default()
        })
        .await;
        assert!(result.is_err(), "a missing file must fail startup");
        assert_eq!(
            watcher_failure_delta(&metrics),
            0.0,
            "a missing file is a startup error, not a watcher failure"
        );
    }

    #[tokio::test]
    async fn invalid_initial_file_error_redacts_scalar_values() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.yaml");
        let credential_fragment = "credential-fragment-987654321";
        tokio::fs::write(
            &file_path,
            format!(
                "mqtt_auth_by_credential_type:\n  {credential_fragment}:\n    username: mqtt-user\n    password: mqtt-password\n"
            ),
        )
        .await
        .expect("write invalid credentials file");

        let error = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path),
            ..Default::default()
        })
        .await
        .err()
        .expect("invalid initial file must fail");

        let message = error.to_string();
        assert!(message.contains("failed to parse"));
        assert!(message.contains("parse details redacted"));
        assert!(!message.contains(credential_fragment));
    }

    #[tokio::test]
    async fn zero_poll_interval_returns_error() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.yaml");
        tokio::fs::write(&file_path, "{}")
            .await
            .expect("write credentials file");

        let error = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path),
            poll_interval: Some(Duration::ZERO),
            ..Default::default()
        })
        .await
        .err()
        .expect("zero poll interval must fail");

        assert!(error.to_string().contains("greater than zero"));
    }

    #[tokio::test]
    async fn loads_yaml_file() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.yaml");
        tokio::fs::write(
            &file_path,
            r#"dpu_uefi_site_default:
  username: root
  password: yaml1
"#,
        )
        .await
        .expect("write yaml file");

        let provider = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path.clone()),
            poll_interval: Some(Duration::from_secs(1)),
            ..Default::default()
        })
        .await
        .expect("create yaml file provider");

        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };

        let value = provider
            .get_credentials(&key)
            .await
            .expect("load yaml value");
        assert_eq!(
            value,
            Some(Credentials::UsernamePassword {
                username: "root".to_string(),
                password: "yaml1".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn loads_machine_identity_encryption_keys_yaml_format() {
        let dir = tempdir().expect("create temp dir");
        let file_path = dir.path().join("credentials.yaml");
        tokio::fs::write(
            &file_path,
            r#"machine_identity:
  encryption_keys:
    v1: secret-1
    v2: secret-2
"#,
        )
        .await
        .expect("write yaml file");

        let provider = FileCredentialsWatcher::new(FileCredentialsConfig {
            path: Some(file_path),
            poll_interval: Some(Duration::from_secs(1)),
            ..Default::default()
        })
        .await
        .expect("create file provider");

        let v1 = CredentialKey::MachineIdentityEncryptionKey {
            key_id: "v1".to_string(),
        };
        let v2 = CredentialKey::MachineIdentityEncryptionKey {
            key_id: "v2".to_string(),
        };

        let value_v1 = provider.get_credentials(&v1).await.expect("load v1");
        let value_v2 = provider.get_credentials(&v2).await.expect("load v2");

        assert_eq!(
            value_v1,
            Some(Credentials::UsernamePassword {
                username: "v1".to_string(),
                password: "secret-1".to_string(),
            })
        );
        assert_eq!(
            value_v2,
            Some(Credentials::UsernamePassword {
                username: "v2".to_string(),
                password: "secret-2".to_string(),
            })
        );
    }

    const WATCHER_FAILURE_METRIC: &str = "carbide_static_credential_watcher_failures_total";

    #[derive(Clone, Copy)]
    struct WatcherFailureCase {
        operation: StaticCredentialWatcherOperation,
        operation_label: &'static str,
        error: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct WatcherFailureObservation {
        counter_delta: f64,
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        operation: Option<String>,
        error: Option<String>,
    }

    fn watcher_failure_delta(metrics: &MetricsCapture) -> f64 {
        [
            "primary_watch",
            "primary_watch_setup",
            "poll_watch",
            "reload",
        ]
        .iter()
        .map(|operation| metrics.counter_delta(WATCHER_FAILURE_METRIC, &[("operation", operation)]))
        .sum()
    }

    #[test]
    fn live_watcher_failures_keep_the_existing_diagnostics() {
        let metrics = MetricsCapture::start();

        check_values(
            [
                Check {
                    scenario: "primary watcher reports an error",
                    input: WatcherFailureCase {
                        operation: StaticCredentialWatcherOperation::PrimaryWatch,
                        operation_label: "primary_watch",
                        error: "inotify queue overflow",
                    },
                    expect: WatcherFailureObservation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        metadata_name: "static_credential_watcher_failed".to_string(),
                        message: "primary static credential watcher error".to_string(),
                        event_name: Some("static_credential_watcher_failed".to_string()),
                        metric_name: Some(WATCHER_FAILURE_METRIC.to_string()),
                        operation: Some("primary_watch".to_string()),
                        error: Some("inotify queue overflow".to_string()),
                    },
                },
                Check {
                    scenario: "primary watcher cannot be armed",
                    input: WatcherFailureCase {
                        operation: StaticCredentialWatcherOperation::PrimaryWatchSetup,
                        operation_label: "primary_watch_setup",
                        error: "Too many open files (os error 24)",
                    },
                    expect: WatcherFailureObservation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        metadata_name: "static_credential_watcher_failed".to_string(),
                        message: FALLBACK_MESSAGE.to_string(),
                        event_name: Some("static_credential_watcher_failed".to_string()),
                        metric_name: Some(WATCHER_FAILURE_METRIC.to_string()),
                        operation: Some("primary_watch_setup".to_string()),
                        error: Some("Too many open files (os error 24)".to_string()),
                    },
                },
                Check {
                    scenario: "poll watcher reports an error",
                    input: WatcherFailureCase {
                        operation: StaticCredentialWatcherOperation::PollWatch,
                        operation_label: "poll_watch",
                        error: "stat failed",
                    },
                    expect: WatcherFailureObservation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        metadata_name: "static_credential_watcher_failed".to_string(),
                        message: "credentials file watcher event error".to_string(),
                        event_name: Some("static_credential_watcher_failed".to_string()),
                        metric_name: Some(WATCHER_FAILURE_METRIC.to_string()),
                        operation: Some("poll_watch".to_string()),
                        error: Some("stat failed".to_string()),
                    },
                },
                Check {
                    scenario: "credential reload fails",
                    input: WatcherFailureCase {
                        operation: StaticCredentialWatcherOperation::Reload,
                        operation_label: "reload",
                        error: "invalid yaml",
                    },
                    expect: WatcherFailureObservation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        metadata_name: "static_credential_watcher_failed".to_string(),
                        message: "failed to reload credentials file".to_string(),
                        event_name: Some("static_credential_watcher_failed".to_string()),
                        metric_name: Some(WATCHER_FAILURE_METRIC.to_string()),
                        operation: Some("reload".to_string()),
                        error: Some("invalid yaml".to_string()),
                    },
                },
            ],
            |case| {
                let mut logs = capture_logs(|| {
                    let error = case.error.to_string();
                    emit(match case.operation {
                        StaticCredentialWatcherOperation::PrimaryWatch => {
                            StaticCredentialWatcherFailed::PrimaryWatch { error }
                        }
                        StaticCredentialWatcherOperation::PrimaryWatchSetup => {
                            StaticCredentialWatcherFailed::PrimaryWatchSetup {
                                error,
                                poll_interval_secs: 60.0,
                            }
                        }
                        StaticCredentialWatcherOperation::PollWatch => {
                            StaticCredentialWatcherFailed::PollWatch { error }
                        }
                        StaticCredentialWatcherOperation::Reload => {
                            StaticCredentialWatcherFailed::Reload { error }
                        }
                    });
                });
                assert_eq!(logs.len(), 1, "one watcher failure logs once");
                let log = logs.pop().expect("the watcher failure log");
                let field = |name: &str| log.field(name).map(str::to_owned);

                WatcherFailureObservation {
                    counter_delta: metrics.counter_delta(
                        WATCHER_FAILURE_METRIC,
                        &[("operation", case.operation_label)],
                    ),
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: field("event_name"),
                    metric_name: field("metric_name"),
                    operation: field("operation"),
                    error: field("error"),
                }
            },
        );
    }

    #[derive(Clone, Copy)]
    struct ClosedReceiverCase {
        delivery: WatchEventDelivery,
        message: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct ClosedReceiverObservation {
        counter_delta: f64,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        error: Option<String>,
    }

    #[test]
    fn closed_receiver_warnings_do_not_count_as_live_watcher_failures() {
        let metrics = MetricsCapture::start();

        check_values(
            [
                Check {
                    scenario: "primary receiver is closed",
                    input: ClosedReceiverCase {
                        delivery: WatchEventDelivery::Primary,
                        message: "failed to send static credential watch event",
                    },
                    expect: ClosedReceiverObservation {
                        counter_delta: 0.0,
                        level: tracing::Level::WARN,
                        message: "failed to send static credential watch event".to_string(),
                        event_name: None,
                        metric_name: None,
                        error: Some("channel closed".to_string()),
                    },
                },
                Check {
                    scenario: "poll receiver is closed",
                    input: ClosedReceiverCase {
                        delivery: WatchEventDelivery::Poll,
                        message: "failed to send static credential poll event",
                    },
                    expect: ClosedReceiverObservation {
                        counter_delta: 0.0,
                        level: tracing::Level::WARN,
                        message: "failed to send static credential poll event".to_string(),
                        event_name: None,
                        metric_name: None,
                        error: Some("channel closed".to_string()),
                    },
                },
            ],
            |case| {
                let (tx, rx) = mpsc::channel(1);
                drop(rx);
                let logs = capture_logs(|| {
                    forward_watch_event(case.delivery, &tx, notify::Event::default());
                });
                let matching_logs = logs
                    .into_iter()
                    .filter(|log| log.message == case.message)
                    .collect::<Vec<_>>();
                assert_eq!(
                    matching_logs.len(),
                    1,
                    "one failed delivery writes its historical WARN"
                );
                let log = matching_logs
                    .into_iter()
                    .next()
                    .expect("the failed delivery log");
                let event_name = log.field("event_name").map(str::to_owned);
                let metric_name = log.field("metric_name").map(str::to_owned);
                let error = log.field("error").map(str::to_owned);

                ClosedReceiverObservation {
                    counter_delta: watcher_failure_delta(&metrics),
                    level: log.level,
                    message: log.message,
                    event_name,
                    metric_name,
                    error,
                }
            },
        );
    }
}
