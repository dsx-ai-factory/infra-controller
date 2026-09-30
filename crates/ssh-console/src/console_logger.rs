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
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use carbide_uuid::machine::MachineId;
use chrono::Utc;
use russh::ChannelMsg;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::bmc::message_proxy::ToFrontendMessage;
use crate::config::Config;
use crate::fork_cancel_token;
use crate::shutdown_handle::ShutdownHandle;

/// Channel for requesting snapshots of existing logs going back a given number of lines. Capacity
/// represents how many clients can be simultaneously requesting snapshots to this BMC.
static ACTOR_COMMAND_QUEUE_CAPACITY: usize = 16;
/// Channel for giving messages to a live subscriber. Capacity represents how many messages to keep
/// in memory in case the subscriber is slow to consume them.
static LIVE_SUBSCRIBER_QUEUE_CAPACITY: usize = 1024;

/// Spawn a background task which logs all output from a BMC
pub(crate) fn spawn(
    machine_id: MachineId,
    addr: SocketAddr,
    message_rx: broadcast::Receiver<ToFrontendMessage>,
    config: Arc<Config>,
    cancel_token: CancellationToken,
) -> (ConsoleLoggerHandle, ConsoleLogClient) {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);
    let (broadcast_tx, _) = broadcast::channel(LIVE_SUBSCRIBER_QUEUE_CAPACITY);
    let (command_tx, command_rx) = mpsc::channel(ACTOR_COMMAND_QUEUE_CAPACITY);
    let (health_tx, health_rx) = watch::channel(LoggerHealth::Starting);
    let client = ConsoleLogClient {
        broadcast_tx: broadcast_tx.downgrade(),
        command_tx,
        health_rx,
    };
    let console_logger = ConsoleLogger::new(
        config,
        machine_id,
        addr,
        broadcast_tx,
        command_rx,
        health_tx,
    );

    let join_handle = tokio::spawn(console_logger.run(cancel_token, message_rx));

    (
        ConsoleLoggerHandle {
            drop_guard,
            join_handle,
        },
        client,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct PublishedLine {
    pub(crate) sequence: u64,
    pub(crate) data: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub(crate) enum LoggerHealth {
    Starting,
    Healthy,
    Failed(Arc<str>),
    Stopped,
}

#[derive(Clone)]
pub(crate) struct ConsoleLogClient {
    broadcast_tx: broadcast::WeakSender<Arc<PublishedLine>>,
    command_tx: mpsc::Sender<ClientCommand>,
    health_rx: watch::Receiver<LoggerHealth>,
}

impl ConsoleLogClient {
    pub(crate) fn health(&self) -> LoggerHealth {
        self.health_rx.borrow().clone()
    }

    pub(crate) fn subscribe(
        &self,
    ) -> Result<broadcast::Receiver<Arc<PublishedLine>>, LoggerHealth> {
        self.broadcast_tx
            .upgrade()
            .map(|tx| tx.subscribe())
            .ok_or_else(|| self.health_rx.borrow().clone())
    }

    pub(crate) async fn snapshot(&self) -> Result<Snapshot, LoggerHealth> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::Snapshot { reply_tx })
            .await
            .map_err(|_| self.health_rx.borrow().clone())?;
        reply_rx
            .await
            .map_err(|_| self.health_rx.borrow().clone())?
    }
}

pub(crate) struct Snapshot {
    pub(crate) watermark: u64,
    files: Vec<SnapshotFile>,
}

struct SnapshotFile {
    file: tokio::fs::File,
    // Represents the length of the file at the time the snapshot was taken. Reading up to this many
    // bytes gives you the logs that were in that file at the moment of the snapshot.
    len: u64,
}

enum ClientCommand {
    Snapshot {
        reply_tx: oneshot::Sender<Result<Snapshot, LoggerHealth>>,
    },
}

pub(crate) struct ConsoleLoggerHandle {
    drop_guard: DropGuard,
    join_handle: JoinHandle<()>,
}

impl ShutdownHandle<()> for ConsoleLoggerHandle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

struct ConsoleLogger {
    config: Arc<Config>,
    machine_id: MachineId,
    log_path: PathBuf,
    live_tx: broadcast::Sender<Arc<PublishedLine>>,
    command_rx: mpsc::Receiver<ClientCommand>,
    health_tx: watch::Sender<LoggerHealth>,
    sequence: u64,
}

impl ConsoleLogger {
    fn new(
        config: Arc<Config>,
        machine_id: MachineId,
        addr: SocketAddr,
        live_tx: broadcast::Sender<Arc<PublishedLine>>,
        command_rx: mpsc::Receiver<ClientCommand>,
        health_tx: watch::Sender<LoggerHealth>,
    ) -> Self {
        Self {
            machine_id,
            log_path: config.console_logs_path.as_path().join(format!(
                "{}_{}.log",
                machine_id,
                addr.ip()
            )),
            config,
            live_tx,
            command_rx,
            health_tx,
            sequence: 0,
        }
    }

    async fn run(
        mut self,
        cancel_token: CancellationToken,
        mut message_rx: broadcast::Receiver<ToFrontendMessage>,
    ) {
        let mut log_file = match RotatableLogFile::open(
            self.log_path.clone(),
            self.config.log_rotate_max_size.bytes() as _,
            self.config.log_rotate_max_rotated_files,
        )
        .await
        {
            Ok(file) => file,
            Err(error) => {
                tracing::error!(path = self.log_path.display().to_string(), machine_id=%self.machine_id, %error, "could not open log file for writing");
                self.fail(error);
                return;
            }
        };

        self.health_tx.send_replace(LoggerHealth::Healthy);
        if let Err(error) = self
            .write_and_publish(
                &mut log_file,
                format!(
                    "\n--- ssh-console started at {} ---\n",
                    Utc::now().to_rfc3339()
                )
                .into_bytes(),
            )
            .await
        {
            self.fail(error);
            return;
        }

        let mut buffer: Vec<u8> = Vec::new();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    break;
                }

                request = self.command_rx.recv() => {
                    let Some(request) = request else {
                        // This should only happen if somehow the client channel closes before we
                        // get the shutdown signal, or if this BMC is removed from NiCo. It should
                        // stay open for as long as we are stored in the BmcConnectionStore
                        tracing::error!("console_logger: client command channel closed, shutting down");
                        break;
                    };
                    match request {
                        ClientCommand::Snapshot { reply_tx } => {
                            let result = self.capture_snapshot(&mut log_file).await;
                            if let Err(error) = &result {
                                tracing::warn!(
                                    machine_id = %self.machine_id,
                                    ?error,
                                    "console_logger: snapshot capture failed"
                                );
                            }
                            reply_tx.send(result).ok();
                        }
                    }
                }

                // incoming SSH data
                res = message_rx.recv() => match res {
                    Ok(msg) => {
                        let msg = Arc::<ChannelMsg>::from(msg);
                        if let ChannelMsg::Data { data } = msg.as_ref() {
                            // append new bytes to our buffer
                            buffer.extend_from_slice(data.as_ref());

                            // process all complete lines
                            while let Some(nl) = buffer.iter().position(|&b| b == b'\n') {
                                // drain through and including the newline
                                let line_bytes: Vec<u8> = buffer.drain(..=nl).collect();

                                // strip ANSI escapes (preserves the newline byte)
                                let clean = strip_ansi_escapes::strip(&line_bytes);

                                // write it out
                                if let Err(error) = self.write_and_publish(&mut log_file, clean).await {
                                    self.fail(error);
                                    return;
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let msg = format!("console logger is lagged by {count} messages (typically bytes). Data may be missing from log");
                        tracing::warn!(
                            machine_id = %self.machine_id,
                            lagged_message_count = count,
                            "console logger lagged; data may be missing from log"
                        );
                        if let Err(error) = self.write_and_publish(&mut log_file, format!("\n--- {msg} ---\n").into_bytes()).await {
                            self.fail(error);
                            return;
                        }
                    }
                },
            }
        }

        tracing::debug!(machine_id=%self.machine_id, "shutting down console logger");
        self.write_and_publish(
            &mut log_file,
            format!(
                "\n--- ssh-console shutting down at {} ---\n",
                Utc::now().to_rfc3339()
            )
            .into_bytes(),
        )
        .await
        .ok();
        log_file.flush().await.ok();
        self.health_tx.send_replace(LoggerHealth::Stopped);
    }

    async fn write_and_publish(
        &mut self,
        log_file: &mut RotatableLogFile,
        line: Vec<u8>,
    ) -> io::Result<()> {
        log_file.write_all(&line).await?;
        for complete_line in line.split_inclusive(|byte| *byte == b'\n') {
            if complete_line.last() != Some(&b'\n') {
                continue;
            }
            self.sequence = self
                .sequence
                .checked_add(1)
                .expect("console log sequence overflow");
            self.live_tx
                .send(Arc::new(PublishedLine {
                    sequence: self.sequence,
                    data: Arc::from(complete_line),
                }))
                .ok();
        }
        Ok(())
    }

    async fn capture_snapshot(
        &mut self,
        log_file: &mut RotatableLogFile,
    ) -> Result<Snapshot, LoggerHealth> {
        log_file.flush().await.map_err(LoggerHealth::from)?;
        let mut files = Vec::new();
        let mut paths = Vec::with_capacity(self.config.log_rotate_max_rotated_files + 1);
        paths.push(self.log_path.clone());
        let base = self.log_path.to_string_lossy();
        paths.extend(
            (0..self.config.log_rotate_max_rotated_files)
                .map(|i| PathBuf::from(format!("{base}.{i}"))),
        );
        for path in paths {
            match OpenOptions::new().read(true).open(&path).await {
                Ok(file) => {
                    let len = file.metadata().await.map_err(LoggerHealth::from)?.len();
                    files.push(SnapshotFile { file, len });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && path != self.log_path => {}
                Err(error) => return Err(LoggerHealth::from(error)),
            }
        }
        Ok(Snapshot {
            watermark: self.sequence,
            files,
        })
    }

    fn fail(&self, error: io::Error) {
        self.health_tx.send_replace(LoggerHealth::from(error));
    }
}

impl From<io::Error> for LoggerHealth {
    fn from(error: io::Error) -> Self {
        Self::Failed(Arc::from(error.to_string()))
    }
}

impl Snapshot {
    pub(crate) async fn into_log_tail(mut self, limit: usize) -> io::Result<Vec<Arc<[u8]>>> {
        const BLOCK_SIZE: usize = 8192;
        let mut blocks = Vec::new();
        let mut byte_count = 0;
        let mut complete_lines = 0;
        'files: for snapshot_file in &mut self.files {
            let mut end = snapshot_file.len;
            while end > 0 {
                let start = end.saturating_sub(BLOCK_SIZE as u64);
                let mut block = vec![0; (end - start) as usize];
                snapshot_file
                    .file
                    .seek(std::io::SeekFrom::Start(start))
                    .await?;
                snapshot_file.file.read_exact(&mut block).await?;
                complete_lines += block.iter().filter(|&&byte| byte == b'\n').count();
                byte_count += block.len();
                blocks.push(block);
                end = start;
                if complete_lines > limit {
                    break 'files;
                }
            }
        }

        let mut bytes = Vec::with_capacity(byte_count);
        for block in blocks.into_iter().rev() {
            bytes.extend_from_slice(&block);
        }
        let mut lines = VecDeque::with_capacity(limit);
        for line in bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| line.last() == Some(&b'\n'))
            .rev()
            .take(limit)
            .map(Arc::<[u8]>::from)
        {
            lines.push_front(line);
        }
        Ok(lines.into())
    }
}

struct RotatableLogFile {
    file: tokio::fs::File,
    path: PathBuf,
    max_size: usize,
    max_rotated_files: usize,
    byte_count: usize,
}

impl RotatableLogFile {
    async fn open(path: PathBuf, max_size: usize, max_rotated_files: usize) -> io::Result<Self> {
        let file = Self::open_log_file(path.as_path()).await?;
        Ok(Self {
            file,
            path,
            max_size,
            max_rotated_files,
            byte_count: 0,
        })
    }

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.byte_count += data.len();

        if self.byte_count > self.max_size {
            self.byte_count = data.len();
            match self.rotate_logs().await {
                Ok(()) => {
                    self.file = Self::open_log_file(&self.path).await?;
                }
                // If we couldn't rotate, just keep writing to this file.
                Err(error) => tracing::error!(%error, "error rotating logs"),
            }
        }

        self.file.write_all(data).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.file.flush().await
    }

    async fn rotate_logs(&mut self) -> Result<(), LogRotationError> {
        tracing::info!(path = %self.path.display(), "rotating logs");
        let log_path_as_str = self
            .path
            .to_str()
            .ok_or_else(|| LogRotationError::InvalidPath {
                path: self.path.clone(),
            })?;

        for dst_num in (0..self.max_rotated_files).rev() {
            let src_path = if dst_num == 0 {
                // Move .log to .log.0
                Cow::Borrowed(&self.path)
            } else {
                // Move .log.(i-1) to .log.(i)
                Cow::Owned(
                    PathBuf::from_str(&format!("{}.{}", log_path_as_str, dst_num - 1))
                        // just appending ".0" shouldn't fail.
                        .expect("BUG: known-good log path didn't parse"),
                )
            };

            if !src_path.exists() {
                tracing::debug!(path = %src_path.display(), "no log file found");
                continue;
            }

            let dst_path = if dst_num >= self.max_rotated_files {
                tracing::debug!(path = %src_path.display(), "deleting oldest log file");
                // Oldest log, more than max allowed rotated file count, delete it and continue
                tokio::fs::remove_file(src_path.as_path())
                    .await
                    .map_err(|error| LogRotationError::Io {
                        error,
                        context: format!("Could not delete old log file at {}", src_path.display()),
                    })?;
                continue;
            } else {
                // Renaming from src_path to this
                PathBuf::from_str(&format!("{log_path_as_str}.{dst_num}"))
                    .expect("BUG: known-good log path didn't parse")
            };

            tracing::debug!(
                source_path = %src_path.display(),
                destination_path = %dst_path.display(),
                "renaming log file"
            );

            tokio::fs::rename(src_path.as_path(), dst_path.as_path())
                .await
                .map_err(|error| LogRotationError::Io {
                    error,
                    context: format!(
                        "Could not rename log file from {} to {}",
                        src_path.display(),
                        dst_path.display()
                    ),
                })?;
        }

        Ok(())
    }

    async fn open_log_file(path: &Path) -> io::Result<tokio::fs::File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
    }
}

#[derive(thiserror::Error, Debug)]
enum LogRotationError {
    #[error("invalid log file path: {path}")]
    InvalidPath { path: PathBuf },
    #[error("error rotating logs: {context}: {error}")]
    Io { context: String, error: io::Error },
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;
    use crate::bmc::message_proxy::ToFrontendMessage;

    async fn captured(path: &Path) -> SnapshotFile {
        let file = OpenOptions::new().read(true).open(path).await.unwrap();
        let len = file.metadata().await.unwrap().len();
        SnapshotFile { file, len }
    }

    fn as_strings(lines: Vec<Arc<[u8]>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| String::from_utf8(line.to_vec()).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn reverse_tail_spans_rotations_in_chronological_order() {
        let dir = temp_dir::TempDir::new().unwrap();
        let current = dir.path().join("console.log");
        let rotated = dir.path().join("console.log.0");
        tokio::fs::write(&rotated, b"one\ntwo\n").await.unwrap();
        tokio::fs::write(&current, b"three\nfour\n").await.unwrap();
        let snapshot = Snapshot {
            watermark: 4,
            files: vec![captured(&current).await, captured(&rotated).await],
        };

        assert_eq!(
            as_strings(snapshot.into_log_tail(3).await.unwrap()),
            ["two\n", "three\n", "four\n"]
        );
    }

    #[tokio::test]
    async fn reverse_tail_joins_a_line_split_across_rotations() {
        let dir = temp_dir::TempDir::new().unwrap();
        let current = dir.path().join("console.log");
        let rotated = dir.path().join("console.log.0");
        tokio::fs::write(&rotated, b"one\nsplit-").await.unwrap();
        tokio::fs::write(&current, b"line\nlast\n").await.unwrap();
        let snapshot = Snapshot {
            watermark: 3,
            files: vec![captured(&current).await, captured(&rotated).await],
        };

        assert_eq!(
            as_strings(snapshot.into_log_tail(3).await.unwrap()),
            ["one\n", "split-line\n", "last\n"]
        );
    }

    #[tokio::test]
    async fn reverse_tail_honors_captured_length_and_ignores_partial_line() {
        let dir = temp_dir::TempDir::new().unwrap();
        let path = dir.path().join("console.log");
        let long = "x".repeat(9000);
        tokio::fs::write(&path, format!("{long}\ncomplete\npartial"))
            .await
            .unwrap();
        let file = captured(&path).await;
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap()
            .write_all(b"-later\n")
            .await
            .unwrap();
        let snapshot = Snapshot {
            watermark: 2,
            files: vec![file],
        };

        assert_eq!(
            as_strings(snapshot.into_log_tail(2).await.unwrap()),
            [format!("{long}\n"), "complete\n".to_string()]
        );
    }

    #[tokio::test]
    async fn captured_descriptor_survives_rotation_rename() {
        let dir = temp_dir::TempDir::new().unwrap();
        let path = dir.path().join("console.log");
        tokio::fs::write(&path, b"before\n").await.unwrap();
        let file = captured(&path).await;
        tokio::fs::rename(&path, dir.path().join("console.log.0"))
            .await
            .unwrap();
        tokio::fs::write(&path, b"after\n").await.unwrap();

        let snapshot = Snapshot {
            watermark: 1,
            files: vec![file],
        };
        assert_eq!(
            as_strings(snapshot.into_log_tail(10).await.unwrap()),
            ["before\n"]
        );
    }

    #[tokio::test]
    async fn snapshot_barrier_joins_history_and_split_live_line_once() {
        let dir = temp_dir::TempDir::new().unwrap();
        let config = Config {
            console_logs_path: dir.path().to_path_buf(),
            ..Config::default()
        };
        let machine_id = MachineId::new(MachineIdSource::Tpm, [7; 32], MachineType::Host);
        let (tx, rx) = broadcast::channel(16);
        let cancel_token = CancellationToken::new();
        let (handle, log_client) = spawn(
            machine_id,
            "127.0.0.1:22".parse().unwrap(),
            rx,
            Arc::new(config),
            cancel_token.clone(),
        );
        let mut live = log_client.subscribe().unwrap();
        assert!(
            tx.send(ToFrontendMessage::Channel(Arc::new(ChannelMsg::Data {
                data: Bytes::from_static(b"before\npar"),
            })))
            .is_ok()
        );

        loop {
            let line = live.recv().await.unwrap();
            if line.data.as_ref() == b"before\n" {
                break;
            }
        }
        let snapshot = log_client.snapshot().await.unwrap();
        let watermark = snapshot.watermark;
        let history = as_strings(snapshot.into_log_tail(1000).await.unwrap());
        assert_eq!(history.last().map(String::as_str), Some("before\n"));

        assert!(
            tx.send(ToFrontendMessage::Channel(Arc::new(ChannelMsg::Data {
                data: Bytes::from_static(b"tial\n"),
            })))
            .is_ok()
        );
        let live_line = loop {
            let line = live.recv().await.unwrap();
            if line.sequence > watermark {
                break line;
            }
        };
        assert_eq!(live_line.data.as_ref(), b"partial\n");
        assert!(!history.iter().any(|line| line == "partial\n"));

        cancel_token.cancel();
        handle.join_handle.await.expect("task panicked");
    }
}
