/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use carbide_uuid::machine_validation::MachineValidationId;
use eyre::WrapErr as _;
use rpc::forge as forgerpc;
use rpc::forge::{
    MachineValidationAttemptLogChunk, MachineValidationAttemptLogStream, MachineValidationRunItem,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::args::{AttemptOptions, Options};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

trait LogSource {
    async fn get_logs(
        &self,
        attempt_id: &str,
        after_sequence: u32,
    ) -> CarbideCliResult<forgerpc::MachineValidationAttemptLogList>;
    async fn attempt_state(&self, attempt_id: &str) -> CarbideCliResult<String>;
}

impl LogSource for ApiClient {
    async fn get_logs(
        &self,
        attempt_id: &str,
        after_sequence: u32,
    ) -> CarbideCliResult<forgerpc::MachineValidationAttemptLogList> {
        self.get_machine_validation_attempt_logs(attempt_id, after_sequence)
            .await
    }

    async fn attempt_state(&self, attempt_id: &str) -> CarbideCliResult<String> {
        Ok(self.get_machine_validation_attempt(attempt_id).await?.state)
    }
}

#[derive(Default)]
struct PendingLine {
    content: String,
    timestamp: String,
    sequence: u32,
}

#[derive(Default)]
struct LogRenderer {
    pending: PendingLine,
    pending_stream: Option<MachineValidationAttemptLogStream>,
}

fn stream_label(stream: MachineValidationAttemptLogStream) -> &'static str {
    match stream {
        MachineValidationAttemptLogStream::Stdout => "stdout",
        MachineValidationAttemptLogStream::Stderr => "stderr",
        MachineValidationAttemptLogStream::Unspecified => "unspecified",
    }
}

impl LogRenderer {
    async fn write_chunk(
        &mut self,
        output: &mut (dyn AsyncWrite + Unpin),
        chunk: &MachineValidationAttemptLogChunk,
        options: &Options,
    ) -> CarbideCliResult<()> {
        let stream = MachineValidationAttemptLogStream::try_from(chunk.stream)
            .unwrap_or(MachineValidationAttemptLogStream::Unspecified);
        if (options.stdout_only && stream != MachineValidationAttemptLogStream::Stdout)
            || (options.stderr_only && stream != MachineValidationAttemptLogStream::Stderr)
        {
            return Ok(());
        }
        if options.raw {
            output
                .write_all(chunk.content.as_bytes())
                .await
                .wrap_err("write machine validation attempt log")?;
            output
                .flush()
                .await
                .wrap_err("write machine validation attempt log")?;
            return Ok(());
        }
        if stream == MachineValidationAttemptLogStream::Unspecified {
            return Err(CarbideCliError::GenericError(
                "attempt log has an invalid stream".to_owned(),
            ));
        }
        if self
            .pending_stream
            .is_some_and(|previous| previous != stream)
        {
            self.flush_pending(output).await?;
        }
        self.pending_stream = Some(stream);
        let pending = &mut self.pending;
        let timestamp = chunk
            .created_at
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        if pending.content.is_empty() {
            pending.timestamp.clone_from(&timestamp);
            pending.sequence = chunk.sequence;
        }
        pending.content.push_str(&chunk.content);
        while let Some(end) = pending.content.find('\n') {
            let prefix = format!(
                "[{} {} #{}] ",
                pending.timestamp,
                stream_label(stream),
                pending.sequence
            );
            output
                .write_all(prefix.as_bytes())
                .await
                .wrap_err("write machine validation attempt log")?;
            output
                .write_all(&pending.content.as_bytes()[..=end])
                .await
                .wrap_err("write machine validation attempt log")?;
            pending.content.drain(..=end);
            pending.timestamp.clone_from(&timestamp);
            pending.sequence = chunk.sequence;
        }
        output
            .flush()
            .await
            .wrap_err("write machine validation attempt log")?;
        Ok(())
    }

    async fn finish(
        &mut self,
        output: &mut (dyn AsyncWrite + Unpin),
        options: &Options,
    ) -> CarbideCliResult<()> {
        if options.raw {
            return Ok(());
        }
        self.flush_pending(output).await
    }

    async fn flush_pending(
        &mut self,
        output: &mut (dyn AsyncWrite + Unpin),
    ) -> CarbideCliResult<()> {
        if !self.pending.content.is_empty() {
            let stream = self.pending_stream.ok_or_else(|| {
                CarbideCliError::GenericError("attempt log has no stream".to_owned())
            })?;
            let line = format!(
                "[{} {} #{}] {}\n",
                self.pending.timestamp,
                stream_label(stream),
                self.pending.sequence,
                self.pending.content
            );
            output
                .write_all(line.as_bytes())
                .await
                .wrap_err("write machine validation attempt log")?;
            self.pending.content.clear();
        }
        output
            .flush()
            .await
            .wrap_err("write machine validation attempt log")?;
        Ok(())
    }
}

pub(super) async fn show(
    options: Options,
    client: &ApiClient,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    let mut sequence = 0;
    let mut renderer = LogRenderer::default();
    fetch_pages(
        client,
        &attempt_id,
        &options,
        &mut sequence,
        &mut renderer,
        output,
    )
    .await?;
    renderer.finish(output, &options).await
}

pub(super) async fn follow(
    options: Options,
    client: &ApiClient,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    follow_attempt(client, &attempt_id, &options, POLL_INTERVAL, output).await
}

async fn follow_attempt<S: LogSource>(
    source: &S,
    attempt_id: &str,
    options: &Options,
    poll_interval: Duration,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let mut sequence = 0;
    let mut renderer = LogRenderer::default();
    loop {
        fetch_pages(
            source,
            attempt_id,
            options,
            &mut sequence,
            &mut renderer,
            output,
        )
        .await?;
        let state = source.attempt_state(attempt_id).await?;
        if !matches!(state.as_str(), "Pending" | "Running") {
            // The writer drains its log queue before recording the terminal state.
            fetch_pages(
                source,
                attempt_id,
                options,
                &mut sequence,
                &mut renderer,
                output,
            )
            .await?;
            renderer.finish(output, options).await?;
            return Ok(());
        }
        tokio::time::sleep(poll_interval).await;
    }
}

pub(super) async fn attempts(
    options: AttemptOptions,
    client: &ApiClient,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let item = find_run_item(client, options.validation_id, &options.test_id).await?;
    let run_item_id = item
        .run_item_id
        .ok_or_else(|| CarbideCliError::GenericError("run item has no ID".to_owned()))?;
    let attempts = client
        .find_machine_validation_attempts(&run_item_id.value)
        .await?;
    for attempt in attempts.attempts {
        let attempt_id = attempt
            .attempt_id
            .ok_or_else(|| CarbideCliError::GenericError("attempt has no ID".to_owned()))?;
        crate::async_writeln!(
            output,
            "attempt={} id={} state={}",
            attempt.attempt_number,
            attempt_id.value,
            attempt.state
        )
        .wrap_err("write machine validation attempt")?;
    }
    Ok(())
}

async fn resolve_attempt_id(options: &Options, client: &ApiClient) -> CarbideCliResult<String> {
    if let Some(attempt_id) = options.attempt_id {
        return Ok(attempt_id.to_string());
    }
    let validation_id = options.validation_id.ok_or_else(|| {
        CarbideCliError::GenericError("provide --attempt-id or --validation-id".to_owned())
    })?;
    let test_id = options.test_id.as_deref().ok_or_else(|| {
        CarbideCliError::GenericError("--test-id is required with --validation-id".to_owned())
    })?;
    let item = find_run_item(client, validation_id, test_id).await?;
    item.current_attempt_id.map(|id| id.value).ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "test {test_id} has no current attempt; use --attempt-id for a previous attempt"
        ))
    })
}

async fn find_run_item(
    client: &ApiClient,
    validation_id: MachineValidationId,
    test_id: &str,
) -> CarbideCliResult<MachineValidationRunItem> {
    let mut matches = client
        .find_machine_validation_run_items(validation_id)
        .await?
        .into_iter()
        .filter(|item| item.test_id == test_id);
    let item = matches.next().ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "test {test_id} was not found in validation run {validation_id}"
        ))
    })?;
    if matches.next().is_some() {
        return Err(CarbideCliError::GenericError(format!(
            "multiple run items match test {test_id}; use --attempt-id"
        )));
    }
    Ok(item)
}

async fn fetch_pages<S: LogSource>(
    source: &S,
    attempt_id: &str,
    options: &Options,
    sequence: &mut u32,
    renderer: &mut LogRenderer,
    output: &mut (dyn AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    loop {
        let page = source.get_logs(attempt_id, *sequence).await?;
        if page.has_more && page.chunks.is_empty() {
            return Err(CarbideCliError::GenericError(
                "attempt-log API returned an empty page with more logs".to_owned(),
            ));
        }
        for chunk in page.chunks {
            if chunk.sequence <= *sequence {
                return Err(CarbideCliError::GenericError(
                    "attempt-log API returned an out-of-order chunk".to_owned(),
                ));
            }
            *sequence = chunk.sequence;
            renderer.write_chunk(output, &chunk, options).await?;
        }
        if !page.has_more {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll};

    use super::*;

    enum OutputFailure {
        Write { remaining_writes: usize },
        Flush,
    }

    #[derive(Default)]
    struct TestOutput {
        bytes: Vec<u8>,
        flushes: usize,
        failure: Option<OutputFailure>,
    }

    impl AsyncWrite for TestOutput {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if let Some(OutputFailure::Write { remaining_writes }) = &mut self.failure {
                if *remaining_writes == 0 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "write failed",
                    )));
                }
                *remaining_writes -= 1;
            }
            self.bytes.extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flushes += 1;
            Poll::Ready(if matches!(self.failure, Some(OutputFailure::Flush)) {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "flush failed"))
            } else {
                Ok(())
            })
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct SharedTestOutput<'a>(&'a Mutex<TestOutput>);

    impl AsyncWrite for SharedTestOutput<'_> {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut *self.0.lock().unwrap()).poll_write(cx, bytes)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut *self.0.lock().unwrap()).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut *self.0.lock().unwrap()).poll_shutdown(cx)
        }
    }

    fn chunk(
        sequence: u32,
        stream: MachineValidationAttemptLogStream,
        content: &str,
    ) -> MachineValidationAttemptLogChunk {
        MachineValidationAttemptLogChunk {
            attempt_id: None,
            sequence,
            stream: stream as i32,
            created_at: None,
            content: content.to_owned(),
        }
    }

    fn options(raw: bool) -> Options {
        Options {
            attempt_id: None,
            validation_id: None,
            test_id: None,
            stdout_only: false,
            stderr_only: false,
            raw,
        }
    }

    #[tokio::test]
    async fn raw_output_preserves_chunk_content_and_stream_filtering() {
        let options = Options {
            attempt_id: None,
            validation_id: None,
            test_id: None,
            stdout_only: false,
            stderr_only: true,
            raw: true,
        };
        let stdout_chunk = chunk(
            1,
            MachineValidationAttemptLogStream::Stdout,
            "stdout without newline",
        );
        let mut output = TestOutput::default();
        let mut renderer = LogRenderer::default();
        renderer
            .write_chunk(&mut output, &stdout_chunk, &options)
            .await
            .expect("stdout filtered");
        assert!(output.bytes.is_empty());
        assert_eq!(output.flushes, 0);

        let stderr_chunk = chunk(
            2,
            MachineValidationAttemptLogStream::Stderr,
            "stderr without newline",
        );
        renderer
            .write_chunk(&mut output, &stderr_chunk, &options)
            .await
            .expect("stderr written");
        assert_eq!(output.bytes, b"stderr without newline");
        assert_eq!(output.flushes, 1);
    }

    #[tokio::test]
    async fn default_output_joins_lines_split_across_chunks() {
        let mut output = TestOutput::default();
        let mut renderer = LogRenderer::default();
        let options = options(false);
        let first_chunk = MachineValidationAttemptLogChunk {
            created_at: Some(
                "2026-10-24T12:00:00Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .expect("first chunk timestamp")
                    .into(),
            ),
            ..chunk(1, MachineValidationAttemptLogStream::Stdout, "hello ")
        };
        let second_chunk = MachineValidationAttemptLogChunk {
            created_at: Some(
                "2026-10-24T12:00:01Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .expect("second chunk timestamp")
                    .into(),
            ),
            ..chunk(2, MachineValidationAttemptLogStream::Stdout, "world\nnext")
        };
        renderer
            .write_chunk(&mut output, &first_chunk, &options)
            .await
            .unwrap();
        assert!(output.bytes.is_empty());
        assert_eq!(output.flushes, 1);
        renderer
            .write_chunk(&mut output, &second_chunk, &options)
            .await
            .unwrap();
        assert_eq!(
            output.bytes,
            b"[2026-10-24T12:00:00Z stdout #1] hello world\n"
        );
        assert_eq!(output.flushes, 2);
        renderer.finish(&mut output, &options).await.unwrap();
        assert_eq!(
            output.bytes,
            b"[2026-10-24T12:00:00Z stdout #1] hello world\n[2026-10-24T12:00:01Z stdout #2] next\n"
        );
        assert_eq!(output.flushes, 3);
    }

    #[tokio::test]
    async fn default_output_preserves_stream_order() {
        let mut output = Vec::new();
        let mut renderer = LogRenderer::default();
        let options = options(false);
        renderer
            .write_chunk(
                &mut output,
                &chunk(
                    1,
                    MachineValidationAttemptLogStream::Stdout,
                    "stdout partial",
                ),
                &options,
            )
            .await
            .unwrap();
        renderer
            .write_chunk(
                &mut output,
                &chunk(
                    2,
                    MachineValidationAttemptLogStream::Stderr,
                    "stderr line\n",
                ),
                &options,
            )
            .await
            .unwrap();
        assert_eq!(
            output,
            b"[ stdout #1] stdout partial\n[ stderr #2] stderr line\n"
        );
    }

    #[tokio::test]
    async fn log_output_propagates_write_and_flush_failures() {
        enum RenderStep {
            RawChunk,
            FormattedLine,
            Finish,
        }

        struct Case {
            scenario: &'static str,
            step: RenderStep,
            failure: OutputFailure,
            expected_source: &'static str,
            expected_bytes: &'static [u8],
        }

        for case in [
            Case {
                scenario: "raw write failure",
                step: RenderStep::RawChunk,
                failure: OutputFailure::Write {
                    remaining_writes: 0,
                },
                expected_source: "write failed",
                expected_bytes: b"",
            },
            Case {
                scenario: "raw flush failure",
                step: RenderStep::RawChunk,
                failure: OutputFailure::Flush,
                expected_source: "flush failed",
                expected_bytes: b"log content",
            },
            Case {
                scenario: "formatted body write failure",
                step: RenderStep::FormattedLine,
                failure: OutputFailure::Write {
                    remaining_writes: 1,
                },
                expected_source: "write failed",
                expected_bytes: b"[ stdout #1] ",
            },
            Case {
                scenario: "formatted flush failure",
                step: RenderStep::FormattedLine,
                failure: OutputFailure::Flush,
                expected_source: "flush failed",
                expected_bytes: b"[ stdout #1] log content\n",
            },
            Case {
                scenario: "pending line write failure during finish",
                step: RenderStep::Finish,
                failure: OutputFailure::Write {
                    remaining_writes: 0,
                },
                expected_source: "write failed",
                expected_bytes: b"",
            },
            Case {
                scenario: "pending line flush failure during finish",
                step: RenderStep::Finish,
                failure: OutputFailure::Flush,
                expected_source: "flush failed",
                expected_bytes: b"[ stdout #1] log content\n",
            },
        ] {
            let mut output = TestOutput::default();
            let mut renderer = LogRenderer::default();
            let options = options(matches!(case.step, RenderStep::RawChunk));
            let log_chunk = chunk(
                1,
                MachineValidationAttemptLogStream::Stdout,
                if matches!(case.step, RenderStep::FormattedLine) {
                    "log content\n"
                } else {
                    "log content"
                },
            );
            if matches!(case.step, RenderStep::Finish) {
                renderer
                    .write_chunk(&mut output, &log_chunk, &options)
                    .await
                    .expect("prepare pending line");
                assert!(output.bytes.is_empty(), "{}", case.scenario);
                assert_eq!(output.flushes, 1, "{}", case.scenario);
            }
            output.failure = Some(case.failure);
            let result = match case.step {
                RenderStep::RawChunk | RenderStep::FormattedLine => {
                    renderer
                        .write_chunk(&mut output, &log_chunk, &options)
                        .await
                }
                RenderStep::Finish => renderer.finish(&mut output, &options).await,
            };
            let error = result.expect_err(case.scenario);
            let CarbideCliError::EyreReport(report) = error else {
                panic!("{}: missing output error context: {error}", case.scenario);
            };
            assert_eq!(
                report.to_string(),
                "write machine validation attempt log",
                "{}",
                case.scenario
            );
            let source = report
                .downcast_ref::<io::Error>()
                .expect("original I/O error");
            assert_eq!(
                source.kind(),
                io::ErrorKind::BrokenPipe,
                "{}",
                case.scenario
            );
            assert_eq!(
                source.to_string(),
                case.expected_source,
                "{}",
                case.scenario
            );
            assert_eq!(output.bytes, case.expected_bytes, "{}", case.scenario);
        }
    }

    struct FakeSource {
        pages: Mutex<VecDeque<forgerpc::MachineValidationAttemptLogList>>,
        states: Mutex<VecDeque<String>>,
        cursors: Mutex<Vec<u32>>,
        output: Mutex<TestOutput>,
    }

    impl LogSource for FakeSource {
        async fn get_logs(
            &self,
            _attempt_id: &str,
            after_sequence: u32,
        ) -> CarbideCliResult<forgerpc::MachineValidationAttemptLogList> {
            if after_sequence == 2 {
                // Output must be visible before another poll, not only after follow exits.
                let output = self.output.lock().unwrap();
                assert_eq!(output.bytes, b"ab");
                assert_eq!(output.flushes, 2);
            }
            self.cursors.lock().unwrap().push(after_sequence);
            Ok(self
                .pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected log page"))
        }

        async fn attempt_state(&self, _attempt_id: &str) -> CarbideCliResult<String> {
            Ok(self
                .states
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected state"))
        }
    }

    #[tokio::test]
    async fn follow_pages_by_cursor_and_fetches_once_more_after_completion() {
        let page =
            |chunks, has_more| forgerpc::MachineValidationAttemptLogList { chunks, has_more };
        let source = FakeSource {
            pages: Mutex::new(VecDeque::from([
                page(
                    vec![chunk(1, MachineValidationAttemptLogStream::Stdout, "a")],
                    true,
                ),
                page(
                    vec![chunk(2, MachineValidationAttemptLogStream::Stderr, "b")],
                    false,
                ),
                page(vec![], false),
                page(
                    vec![chunk(3, MachineValidationAttemptLogStream::Stdout, "c")],
                    false,
                ),
            ])),
            states: Mutex::new(VecDeque::from(["Running".to_owned(), "Success".to_owned()])),
            cursors: Mutex::new(Vec::new()),
            output: Mutex::new(TestOutput::default()),
        };
        let mut output = SharedTestOutput(&source.output);
        follow_attempt(
            &source,
            "attempt",
            &options(true),
            Duration::ZERO,
            &mut output,
        )
        .await
        .unwrap();
        let output = source.output.lock().unwrap();
        assert_eq!(output.bytes, b"abc");
        assert_eq!(output.flushes, 3);
        assert_eq!(*source.cursors.lock().unwrap(), [0, 1, 2, 2]);
    }
}
