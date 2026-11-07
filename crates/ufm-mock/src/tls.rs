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

use std::io;
use std::time::Duration;

use axum_server::tls_rustls::RustlsConfig;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::config::TlsConfig;

/// How often the PEM files are re-read; a rotated certificate is served within this delay.
const RELOAD_INTERVAL: Duration = Duration::from_secs(30);

/// Certificate chain and private key bytes as read from the configured paths.
#[derive(Clone, Eq, PartialEq)]
struct PemPair {
    cert: Vec<u8>,
    key: Vec<u8>,
}

/// Outcome of one check of the PEM files.
#[derive(Debug)]
enum Reload {
    /// The files match the pair read at the previous check.
    Unchanged,
    /// The changed pair is now served.
    Replaced,
    /// The served pair is kept. A pair that does not load is not retried until the files change;
    /// a read failure is retried at the next check.
    Rejected(io::Error),
}

/// Listener TLS material that follows the PEM files on disk, so a rotated pair is served without
/// a restart.
pub(crate) struct ReloadableTls {
    paths: TlsConfig,
    config: RustlsConfig,
    last_seen: PemPair,
}

impl ReloadableTls {
    /// Loads the pair at `paths`.
    pub(crate) async fn load(paths: TlsConfig) -> io::Result<Self> {
        // Other workspace crates link the `ring` provider next to `aws-lc-rs`, so rustls cannot
        // pick a process default on its own. Installing is a no-op when a provider is already
        // installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let pair = read_pair(&paths).await?;
        let config = RustlsConfig::from_pem(pair.cert.clone(), pair.key.clone()).await?;
        Ok(Self {
            paths,
            config,
            last_seen: pair,
        })
    }

    /// Handle for the listener; pairs swapped in through `self` are visible to it.
    pub(crate) fn config(&self) -> RustlsConfig {
        self.config.clone()
    }

    /// Re-reads the files and swaps them in when they differ from the previous check.
    async fn reload_if_changed(&mut self) -> Reload {
        let pair = match read_pair(&self.paths).await {
            Ok(pair) => pair,
            Err(error) => return Reload::Rejected(error),
        };
        if pair == self.last_seen {
            return Reload::Unchanged;
        }
        let result = self
            .config
            .reload_from_pem(pair.cert.clone(), pair.key.clone())
            .await;
        self.last_seen = pair;
        match result {
            Ok(()) => Reload::Replaced,
            Err(error) => Reload::Rejected(error),
        }
    }

    /// Checks the files every [`RELOAD_INTERVAL`] until `cancellation` fires. A pair that does
    /// not load is logged once per distinct failure.
    pub(crate) async fn watch(mut self, cancellation: CancellationToken) {
        let mut ticker = tokio::time::interval(RELOAD_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut last_rejection: Option<String> = None;
        while cancellation
            .run_until_cancelled(ticker.tick())
            .await
            .is_some()
        {
            match self.reload_if_changed().await {
                Reload::Unchanged => last_rejection = None,
                Reload::Replaced => {
                    last_rejection = None;
                    tracing::info!(
                        cert_path = %self.paths.cert_path.display(),
                        key_path = %self.paths.key_path.display(),
                        "Reloaded TLS certificate and key"
                    );
                }
                Reload::Rejected(error) => {
                    let message = error.to_string();
                    if last_rejection.as_deref() != Some(message.as_str()) {
                        tracing::warn!(
                            cert_path = %self.paths.cert_path.display(),
                            key_path = %self.paths.key_path.display(),
                            error = %error,
                            "Changed TLS files did not load; keeping the served certificate"
                        );
                        last_rejection = Some(message);
                    }
                }
            }
        }
    }
}

async fn read_pair(paths: &TlsConfig) -> io::Result<PemPair> {
    Ok(PemPair {
        cert: tokio::fs::read(&paths.cert_path).await?,
        key: tokio::fs::read(&paths.key_path).await?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn self_signed() -> (String, String) {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        (certified.cert.pem(), certified.signing_key.serialize_pem())
    }

    fn write_pair(paths: &TlsConfig, cert: &str, key: &str) {
        std::fs::write(&paths.cert_path, cert).unwrap();
        std::fs::write(&paths.key_path, key).unwrap();
    }

    /// A rotated pair replaces the served configuration, a pair that does not load keeps it and
    /// is not retried until the files change, and unchanged files are left alone.
    #[tokio::test]
    async fn reload_follows_the_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TlsConfig {
            cert_path: dir.path().join("tls.crt"),
            key_path: dir.path().join("tls.key"),
        };
        let (initial_cert, initial_key) = self_signed();
        let (rotated_cert, rotated_key) = self_signed();
        write_pair(&paths, &initial_cert, &initial_key);
        let mut tls = ReloadableTls::load(paths.clone()).await.unwrap();

        struct Row<'a> {
            scenario: &'a str,
            cert: &'a str,
            key: &'a str,
            expected: fn(&Reload) -> bool,
            swapped: bool,
        }
        // Rows run in order against the same reloader.
        let cases = [
            Row {
                scenario: "unchanged files",
                cert: &initial_cert,
                key: &initial_key,
                expected: |r| matches!(r, Reload::Unchanged),
                swapped: false,
            },
            Row {
                scenario: "rotated pair",
                cert: &rotated_cert,
                key: &rotated_key,
                expected: |r| matches!(r, Reload::Replaced),
                swapped: true,
            },
            Row {
                scenario: "certificate rotated before its key",
                cert: &rotated_cert,
                key: &initial_key,
                expected: |r| matches!(r, Reload::Rejected(_)),
                swapped: false,
            },
            Row {
                scenario: "same unusable pair is not retried",
                cert: &rotated_cert,
                key: &initial_key,
                expected: |r| matches!(r, Reload::Unchanged),
                swapped: false,
            },
            Row {
                scenario: "rotation completed",
                cert: &initial_cert,
                key: &initial_key,
                expected: |r| matches!(r, Reload::Replaced),
                swapped: true,
            },
        ];
        for row in cases {
            let before = tls.config().get_inner();
            write_pair(&paths, row.cert, row.key);
            let outcome = tls.reload_if_changed().await;
            assert!((row.expected)(&outcome), "{}: {outcome:?}", row.scenario);
            assert_eq!(
                !Arc::ptr_eq(&before, &tls.config().get_inner()),
                row.swapped,
                "{}",
                row.scenario
            );
        }
    }
}
