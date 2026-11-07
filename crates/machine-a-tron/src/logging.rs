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
use std::fs::File;
use std::io::{self, Write};
use std::sync::Arc;

use machine_a_tron::LogFormat;
use tracing::Subscriber;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{fmt, registry};

#[derive(Clone)]
enum LogWriter {
    Stdout,
    File(Arc<File>),
}

impl LogWriter {
    fn new(filename: Option<&str>) -> io::Result<Self> {
        match filename {
            Some(filename) => Ok(Self::File(Arc::new(File::create(filename)?))),
            None => Ok(Self::Stdout),
        }
    }
}

impl Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Stdout => io::stdout().write(buffer),
            Self::File(file) => {
                let mut file = file.as_ref();
                file.write(buffer)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Stdout => io::stdout().flush(),
            Self::File(file) => {
                let mut file = file.as_ref();
                file.flush()
            }
        }
    }
}

pub(super) fn init_logging(
    format: LogFormat,
    filename: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let writer = LogWriter::new(filename)?;
    let env_filter = env_filter();

    match format {
        LogFormat::Compact => registry()
            .with(
                fmt::Layer::default()
                    .compact()
                    .with_writer(move || writer.clone()),
            )
            .with(env_filter)
            .try_init()?,
        LogFormat::Logfmt => registry()
            .with(logfmt_layer(writer))
            .with(env_filter)
            .try_init()?,
    }

    Ok(())
}

fn logfmt_layer<S>(writer: LogWriter) -> logfmt::LogFmtLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    logfmt::layer()
        .with_event_fields([logfmt::EventField::with_default(
            "component",
            "nico-machine-a-tron",
        )])
        .with_writer(Arc::new(move || Box::new(writer.clone())))
}

fn env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("hickory_proto=warn".parse().unwrap())
        .add_directive("hickory_resolver=warn".parse().unwrap())
        .add_directive("h2=warn".parse().unwrap())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn logfmt_output_contains_structured_fields() {
        let output = NamedTempFile::new().unwrap();
        let writer = LogWriter::new(Some(output.path().to_str().unwrap())).unwrap();
        let subscriber = registry().with(logfmt_layer(writer));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(answer = 42, "hello from machine-a-tron");
        });

        let output = fs::read_to_string(output.path()).unwrap();
        assert!(output.starts_with(
            "level=INFO component=nico-machine-a-tron msg=\"hello from machine-a-tron\" answer=42"
        ));
    }
}
