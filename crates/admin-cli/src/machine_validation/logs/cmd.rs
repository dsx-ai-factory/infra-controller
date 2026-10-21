/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::io::{self, Write};
use std::time::Duration;

use carbide_uuid::machine_validation::MachineValidationId;
use rpc::forge as forgerpc;
use rpc::forge::{
    MachineValidationAttemptLogChunk, MachineValidationAttemptLogStream, MachineValidationRunItem,
};

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
    fn write_chunk(
        &mut self,
        output: &mut impl Write,
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
            output.write_all(chunk.content.as_bytes())?;
            output.flush()?;
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
            self.flush_pending(output)?;
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
            write!(
                output,
                "[{} {} #{}] ",
                pending.timestamp,
                stream_label(stream),
                pending.sequence
            )?;
            output.write_all(&pending.content.as_bytes()[..=end])?;
            pending.content.drain(..=end);
            pending.timestamp.clone_from(&timestamp);
            pending.sequence = chunk.sequence;
        }
        output.flush()?;
        Ok(())
    }

    fn finish(&mut self, output: &mut impl Write, options: &Options) -> CarbideCliResult<()> {
        if options.raw {
            return Ok(());
        }
        self.flush_pending(output)
    }

    fn flush_pending(&mut self, output: &mut impl Write) -> CarbideCliResult<()> {
        if !self.pending.content.is_empty() {
            let stream = self.pending_stream.ok_or_else(|| {
                CarbideCliError::GenericError("attempt log has no stream".to_owned())
            })?;
            writeln!(
                output,
                "[{} {} #{}] {}",
                self.pending.timestamp,
                stream_label(stream),
                self.pending.sequence,
                self.pending.content
            )?;
            self.pending.content.clear();
        }
        output.flush()?;
        Ok(())
    }
}

pub(super) async fn show(options: Options, client: &ApiClient) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    let mut sequence = 0;
    let mut renderer = LogRenderer::default();
    let mut output = io::stdout();
    fetch_pages(
        client,
        &attempt_id,
        &options,
        &mut sequence,
        &mut renderer,
        &mut output,
    )
    .await?;
    renderer.finish(&mut output, &options)
}

pub(super) async fn follow(options: Options, client: &ApiClient) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    let mut output = io::stdout();
    follow_attempt(client, &attempt_id, &options, POLL_INTERVAL, &mut output).await
}

async fn follow_attempt<S: LogSource>(
    source: &S,
    attempt_id: &str,
    options: &Options,
    poll_interval: Duration,
    output: &mut impl Write,
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
            renderer.finish(output, options)?;
            return Ok(());
        }
        tokio::time::sleep(poll_interval).await;
    }
}

pub(super) async fn attempts(options: AttemptOptions, client: &ApiClient) -> CarbideCliResult<()> {
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
        println!(
            "attempt={} id={} state={}",
            attempt.attempt_number, attempt_id.value, attempt.state
        );
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
    output: &mut impl Write,
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
            renderer.write_chunk(output, &chunk, options)?;
        }
        if !page.has_more {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

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

    #[test]
    fn raw_output_preserves_chunk_content_and_stream_filtering() {
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
        let mut output = Vec::new();
        let mut renderer = LogRenderer::default();
        renderer
            .write_chunk(&mut output, &stdout_chunk, &options)
            .expect("stdout filtered");
        assert!(output.is_empty());

        let stderr_chunk = chunk(
            2,
            MachineValidationAttemptLogStream::Stderr,
            "stderr without newline",
        );
        renderer
            .write_chunk(&mut output, &stderr_chunk, &options)
            .expect("stderr written");
        assert_eq!(output, b"stderr without newline");
    }

    #[test]
    fn default_output_joins_lines_split_across_chunks() {
        let mut output = Vec::new();
        let mut renderer = LogRenderer::default();
        let options = options(false);
        renderer
            .write_chunk(
                &mut output,
                &chunk(1, MachineValidationAttemptLogStream::Stdout, "hello "),
                &options,
            )
            .unwrap();
        assert!(output.is_empty());
        renderer
            .write_chunk(
                &mut output,
                &chunk(2, MachineValidationAttemptLogStream::Stdout, "world\nnext"),
                &options,
            )
            .unwrap();
        assert_eq!(
            String::from_utf8(output.clone()).unwrap(),
            "[ stdout #1] hello world\n"
        );
        renderer.finish(&mut output, &options).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[ stdout #1] hello world\n[ stdout #2] next\n"
        );
    }

    #[test]
    fn default_output_preserves_stream_order() {
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
            .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[ stdout #1] stdout partial\n[ stderr #2] stderr line\n"
        );
    }

    struct FakeSource {
        pages: Mutex<VecDeque<forgerpc::MachineValidationAttemptLogList>>,
        states: Mutex<VecDeque<String>>,
        cursors: Mutex<Vec<u32>>,
    }

    impl LogSource for FakeSource {
        async fn get_logs(
            &self,
            _attempt_id: &str,
            after_sequence: u32,
        ) -> CarbideCliResult<forgerpc::MachineValidationAttemptLogList> {
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
        };
        let mut output = Vec::new();
        follow_attempt(
            &source,
            "attempt",
            &options(true),
            Duration::ZERO,
            &mut output,
        )
        .await
        .unwrap();
        assert_eq!(output, b"abc");
        assert_eq!(*source.cursors.lock().unwrap(), [0, 1, 2, 2]);
    }
}
