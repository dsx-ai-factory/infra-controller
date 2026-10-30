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

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::routing::get;
use futures_util::{StreamExt, stream};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::task::JoinSet;

// Bound process startup, the local exchange, and shutdown independently so a
// stalled child cannot hold the test suite open.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const IMAGE: &str = "checked firmware image";
const PARTIAL_IMAGE: &[u8] = b"partial firmware image";

/// The public command must stage an image without a device config, accept an
/// uppercase digest, and remove the checked image when the query cannot start.
#[tokio::test]
async fn preflight_cleans_up_when_the_query_is_unavailable() {
    let scout = ScoutFixture::new();
    let fetches = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/image.bin",
        get({
            let fetches = Arc::clone(&fetches);
            move || {
                fetches.fetch_add(1, Ordering::SeqCst);
                async { IMAGE }
            }
        }),
    );
    let (image_url, mut server) = start_server(app).await;

    let result = tokio::time::timeout(TEST_TIMEOUT, scout.command(&image_url).output()).await;
    tokio::time::timeout(TEST_TIMEOUT, server.shutdown())
        .await
        .expect("image server did not stop");
    let output = result
        .expect("preflight did not finish")
        .expect("run public preflight command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mlxfwmanager query failed: command mlxfwmanager --dev 03:00.0 --query-format xml did not start: No such file or directory"),
        "{stderr}"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_dir(scout.temporary_directory())
            .expect("read child's temporary directory")
            .count(),
        0,
        "failed inspection must remove its staged image"
    );
}

/// Both operator signals must unwind an active download and remove its files.
/// Waiting for bytes on disk distinguishes cleanup from exiting before Scout
/// has created anything, and the error identifies the signal that cancelled it.
#[tokio::test]
async fn preflight_signals_remove_a_partial_download() {
    for signal in ["SIGINT", "SIGTERM"] {
        let scout = ScoutFixture::new();
        let app = Router::new().route(
            "/image.bin",
            get(|| async {
                Body::from_stream(
                    stream::once(async {
                        Ok::<_, std::io::Error>(Bytes::from_static(PARTIAL_IMAGE))
                    })
                    .chain(stream::pending()),
                )
            }),
        );
        let (image_url, mut server) = start_server(app).await;
        let mut child = scout.command(&image_url).spawn().expect("start preflight");

        let cancellation = tokio::time::timeout(TEST_TIMEOUT, async {
            let staging_directory =
                wait_for_partial_image(&scout.temporary_directory(), &mut child).await?;
            let pid = child
                .id()
                .ok_or_else(|| eyre::eyre!("preflight exited before {signal}"))?;
            let sent = Command::new("/bin/kill")
                .args(["-s", signal])
                .arg(pid.to_string())
                .kill_on_drop(true)
                .output()
                .await?;
            eyre::ensure!(
                sent.status.success(),
                "failed to send {signal}: {}",
                String::from_utf8_lossy(&sent.stderr)
            );
            Ok::<_, eyre::Report>(staging_directory)
        })
        .await;

        // Reap the child and join the server before checking either result.
        // kill_on_drop also covers a panic or a timeout while collecting output.
        let stop_result = if matches!(&cancellation, Ok(Ok(_))) {
            Ok(())
        } else {
            child.start_kill()
        };
        let result = tokio::time::timeout(TEST_TIMEOUT, child.wait_with_output()).await;
        tokio::time::timeout(TEST_TIMEOUT, server.shutdown())
            .await
            .expect("image server did not stop");

        stop_result.expect("stop preflight after test failure");
        let staging_directory = cancellation
            .unwrap_or_else(|error| panic!("{signal}: partial download timed out: {error}"))
            .unwrap_or_else(|error| panic!("{signal}: {error}"));
        let output = result
            .unwrap_or_else(|error| panic!("{signal}: preflight did not stop: {error}"))
            .unwrap_or_else(|error| panic!("{signal}: collect preflight output: {error}"));
        assert!(!output.status.success(), "{signal}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("firmware preflight cancelled by {signal}")),
            "{signal}: {stderr}"
        );
        assert!(!staging_directory.exists(), "{signal}: staged files remain");
        assert_eq!(
            std::fs::read_dir(scout.temporary_directory())
                .expect("read child's temporary directory")
                .count(),
            0,
            "{signal}: staging directory remains"
        );
    }
}

struct ScoutFixture {
    directory: tempfile::TempDir,
}

impl ScoutFixture {
    /// `new` isolates Scout's executable search and temporary files so tests
    /// cannot query local hardware or observe another process's staging.
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create process fixture");
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).expect("create empty executable search directory");
        std::fs::create_dir(directory.path().join("tmp"))
            .expect("create child temporary directory");
        Self { directory }
    }

    /// `temporary_directory` isolates Scout's staging under its own `TMPDIR`,
    /// making cleanup observable without searching the shared temporary path.
    fn temporary_directory(&self) -> PathBuf {
        self.directory.path().join("tmp")
    }

    /// `command` runs an image-only preflight with an uppercase digest. The
    /// child's `PATH` contains only an empty directory, making `mlxfwmanager`
    /// unavailable regardless of the host installation.
    fn command(&self, image_url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge-scout"));
        command
            .args([
                "--mode",
                "standalone",
                "mlx",
                "firmware",
                "preflight",
                "--device",
                "03:00.0",
                "--part-number",
                "test-part",
                "--psid",
                "test-psid",
                "--version",
                "32.43.1014",
                "--image-url",
                image_url,
                "--image-sha256",
            ])
            .arg(hex::encode_upper(Sha256::digest(IMAGE.as_bytes())))
            .env("PATH", self.directory.path().join("bin"))
            .env("TMPDIR", self.temporary_directory())
            .env("RUST_LOG", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

/// `start_server` keeps the loopback listener task owned until the test joins
/// it; dropping the `JoinSet` also stops the listener if the test panics.
async fn start_server(app: Router) -> (String, JoinSet<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind image server");
    let address = listener.local_addr().expect("read image server address");
    let mut server = JoinSet::new();
    server.spawn(async move { axum::serve(listener, app).await });
    (format!("http://{address}/image.bin"), server)
}

/// `wait_for_partial_image` returns the staging directory only after response
/// bytes reach disk. Receiving HTTP headers alone does not prove that cleanup
/// has a partially downloaded file to remove.
async fn wait_for_partial_image(
    temporary_directory: &Path,
    child: &mut Child,
) -> eyre::Result<PathBuf> {
    loop {
        if let Some(status) = child.try_wait()? {
            eyre::bail!("preflight exited before writing partial bytes: {status}");
        }
        for entry in std::fs::read_dir(temporary_directory)? {
            let staging_directory = entry?.path();
            match std::fs::read(staging_directory.join("downloads/image.bin")) {
                Ok(bytes) if bytes == PARTIAL_IMAGE => return Ok(staging_directory),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
