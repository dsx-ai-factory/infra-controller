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

/// Macro for writing formatted output to a tokio::io::AsyncWrite object
/// Similar to write! but for async writers
/// $writer must be AsyncWrite + Unpin
#[macro_export]
macro_rules! async_write {
    ($writer:expr, $($arg:tt)*) => {
        {
            use tokio::io::AsyncWriteExt;
            let formatted = format!($($arg)*);
            let mut result = $writer.write_all(formatted.as_bytes()).await;
            if result.is_ok() {
                let flush_result = $writer.flush().await;
                if flush_result.is_err() {
                    result = flush_result;
                }
            }
            result
        }
    };
}

/// Macro for writing formatted output with a newline to a tokio::io::AsyncWrite object
/// Similar to writeln! but for async writers
/// $writer must be AsyncWrite + Unpin
#[macro_export]
macro_rules! async_writeln {
    ($writer:expr) => {{
        use tokio::io::AsyncWriteExt;
        $writer.write_all("\n".as_bytes()).await
    }};
    ($writer:expr, $($arg:tt)+) => {{
        use tokio::io::AsyncWriteExt;
        let mut formatted = format!($($arg)+);
        formatted.push('\n');
        let mut result = $writer.write_all(formatted.as_bytes()).await;
        if result.is_ok() {
            let flush_result = $writer.flush().await;
            if flush_result.is_err() {
                result = flush_result;
            }
        }
        result
    }};
}

/// Macro for writing a prettytable table as csv to a tokio::io::AsyncWrite object
/// $writer must be AsyncWrite + Unpin
#[macro_export]
macro_rules! async_write_table_as_csv {
    ($writer:expr, $table:expr) => {{
        use tokio::io::AsyncWriteExt;
        let mut output = Vec::default();
        $table
            .to_csv(&mut output)
            .map_err(|e| $crate::errors::CarbideCliError::GenericError(e.to_string()))?;
        let mut result = $writer.write_all(output.as_slice()).await;
        if result.is_ok() {
            let flush_result = $writer.flush().await;
            if flush_result.is_err() {
                result = flush_result;
            }
        }
        result
    }};
}

#[cfg(test)]
pub(crate) struct CapturedOutput {
    writer: Box<dyn tokio::io::AsyncWrite + Unpin>,
    collect: tokio::task::JoinHandle<Vec<u8>>,
}

#[cfg(test)]
impl CapturedOutput {
    pub(crate) fn new() -> Self {
        let (writer, mut reader) = tokio::io::duplex(64 * 1024);
        let collect = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;

            let mut output = Vec::new();
            reader
                .read_to_end(&mut output)
                .await
                .expect("captured output should be readable");
            output
        });

        Self {
            writer: Box::new(writer),
            collect,
        }
    }

    pub(crate) fn writer(&mut self) -> &mut Box<dyn tokio::io::AsyncWrite + Unpin> {
        &mut self.writer
    }

    pub(crate) async fn into_bytes(self) -> Vec<u8> {
        let Self { writer, collect } = self;
        drop(writer);
        collect.await.expect("output collector should complete")
    }
}
