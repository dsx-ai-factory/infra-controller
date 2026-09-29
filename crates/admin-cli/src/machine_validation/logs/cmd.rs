/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::io::{self, Write};
use std::time::Duration;

use rpc::forge::{MachineValidationAttemptLogChunk, MachineValidationAttemptLogStream};

use super::args::Options;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(super) async fn show(options: Options, client: &ApiClient) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    let mut sequence = 0;
    fetch_pages(client, &attempt_id, &options, &mut sequence).await
}

pub(super) async fn follow(options: Options, client: &ApiClient) -> CarbideCliResult<()> {
    let attempt_id = resolve_attempt_id(&options, client).await?;
    let mut sequence = 0;
    loop {
        fetch_pages(client, &attempt_id, &options, &mut sequence).await?;
        let attempt = client.get_machine_validation_attempt(&attempt_id).await?;
        if !matches!(attempt.state.as_str(), "Pending" | "Running") {
            // The writer drains its log queue before recording the terminal state.
            fetch_pages(client, &attempt_id, &options, &mut sequence).await?;
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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
    item.current_attempt_id.map(|id| id.value).ok_or_else(|| {
        CarbideCliError::GenericError(format!(
            "test {test_id} has no current attempt; use --attempt-id for a previous attempt"
        ))
    })
}

async fn fetch_pages(
    client: &ApiClient,
    attempt_id: &str,
    options: &Options,
    sequence: &mut u32,
) -> CarbideCliResult<()> {
    loop {
        let page = client
            .get_machine_validation_attempt_logs(attempt_id, *sequence)
            .await?;
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
            print_chunk(&chunk, options)?;
        }
        if !page.has_more {
            return Ok(());
        }
    }
}

fn print_chunk(
    chunk: &MachineValidationAttemptLogChunk,
    options: &Options,
) -> CarbideCliResult<()> {
    let mut stdout = io::stdout().lock();
    write_chunk(&mut stdout, chunk, options)?;
    stdout.flush()?;
    Ok(())
}

fn write_chunk(
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
    if !options.raw {
        let timestamp = chunk
            .created_at
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        write!(
            output,
            "[{timestamp} {} #{}] ",
            stream.as_str_name(),
            chunk.sequence
        )?;
    }
    output.write_all(chunk.content.as_bytes())?;
    if !options.raw && !chunk.content.ends_with('\n') {
        writeln!(output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let chunk = MachineValidationAttemptLogChunk {
            attempt_id: None,
            sequence: 1,
            stream: MachineValidationAttemptLogStream::Stdout as i32,
            created_at: None,
            content: "stdout without newline".to_owned(),
        };
        let mut output = Vec::new();
        write_chunk(&mut output, &chunk, &options).expect("stdout filtered");
        assert!(output.is_empty());

        let chunk = MachineValidationAttemptLogChunk {
            stream: MachineValidationAttemptLogStream::Stderr as i32,
            content: "stderr without newline".to_owned(),
            ..chunk
        };
        write_chunk(&mut output, &chunk, &options).expect("stderr written");
        assert_eq!(output, b"stderr without newline");
    }
}
