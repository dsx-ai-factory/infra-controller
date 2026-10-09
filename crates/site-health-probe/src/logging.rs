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

use std::error::Error;
use std::io::Write;
use std::sync::Arc;

use tracing::Subscriber;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry;
use tracing_subscriber::registry::LookupSpan;

type WriterFactory = Arc<dyn Fn() -> Box<dyn Write> + Send + Sync>;

pub(super) fn init_logging() -> Result<(), Box<dyn Error>> {
    registry()
        .with(logfmt_layer(Arc::new(|| Box::new(std::io::stdout()))))
        .with(env_filter())
        .try_init()?;
    Ok(())
}

fn logfmt_layer<S>(writer: WriterFactory) -> logfmt::LogFmtLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    logfmt::layer()
        .with_event_fields([logfmt::EventField::with_default(
            "component",
            "nico-site-health-probe",
        )])
        .with_writer(writer)
}

/// Base filter: info everywhere, with the chatty transport crates quieted.
const QUIET_DEFAULTS: &str =
    "info,tower=warn,rustls=warn,hyper=warn,hyper_util=warn,h2=warn,reqwest=warn";

/// `RUST_LOG` directives are appended after the quiet defaults, so an operator
/// can re-raise any of them (later directives take precedence).
fn env_filter() -> EnvFilter {
    let directives = match std::env::var("RUST_LOG") {
        Ok(user) if !user.is_empty() => format!("{QUIET_DEFAULTS},{user}"),
        _ => QUIET_DEFAULTS.to_string(),
    };
    EnvFilter::builder().parse_lossy(directives)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer mutex")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn logfmt_output_contains_structured_fields() {
        let buffer = SharedBuffer::default();
        let writer = buffer.clone();
        let subscriber = registry().with(logfmt_layer(Arc::new(move || Box::new(writer.clone()))));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(answer = 42, "hello from the probe");
        });

        let output =
            String::from_utf8(buffer.0.lock().expect("buffer mutex").clone()).expect("utf-8 log");
        assert!(
            output.starts_with(
                "level=INFO component=nico-site-health-probe msg=\"hello from the probe\" answer=42"
            ),
            "unexpected logfmt output: {output:?}"
        );
    }
}
