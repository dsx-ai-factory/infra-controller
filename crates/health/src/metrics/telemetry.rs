/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Shared telemetry snapshots and direct Prometheus text encoding.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufWriter, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use flate2::Compression;
use flate2::write::GzEncoder;
use prometheus::{Encoder, Error, TextEncoder, proto};
use tokio_util::sync::CancellationToken;

use super::{GaugeData, GaugeMetrics, MetricsManager};
use crate::sink::MetricSemantics;

// These batches bound work between cooperative cancellation checks.
const CAPTURE_BATCH: usize = 1024;
const SORT_BATCH: usize = 4096;
const ENCODE_BATCH: usize = 64 * 1024;

#[cfg(test)]
type BeforeEncodeHook = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

#[cfg(test)]
type BeforeMutationHook = Arc<dyn Fn() + Send + Sync>;

// Compact membership changes acquire this mutex before registry locks. Generic
// mutations release it before entering the registry, whose callbacks may reenter.
// Capture clones handles here and releases it before taking reading-map guards.
// Updates take only map guards. Release compact-path guards before sorting,
// encoding, compression, or awaiting.
#[derive(Default)]
pub(super) struct RegistryState {
    pub(super) generic: bool,
    pub(super) gauges: HashMap<(u64, u64), Arc<GaugeMetrics>>,

    #[cfg(test)]
    // Called after snapshot capture with all registry and reading guards released.
    pub(super) before_encode: Option<BeforeEncodeHook>,

    #[cfg(test)]
    pub(super) before_mutation: Option<BeforeMutationHook>,
}

pub(super) fn lock(state: &Mutex<RegistryState>) -> MutexGuard<'_, RegistryState> {
    match state.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            // Generic gathering preserves registered collectors if bookkeeping was interrupted.
            state.generic = true;
            state.gauges.clear();
            state
        }
    }
}

pub(super) fn lock_compact(state: &Mutex<RegistryState>) -> Option<MutexGuard<'_, RegistryState>> {
    let state = lock(state);

    #[cfg(test)]
    let hook = state.before_mutation.clone();

    let state = (!state.generic).then_some(state);

    #[cfg(test)]
    if let Some(hook) = hook {
        hook();
    }

    state
}

struct Snapshot {
    streams: Vec<Stream>,
}

struct Stream {
    prefix: String,
    help: String,
    labels: Arc<[proto::LabelPair]>,
    readings: Vec<Arc<GaugeData>>,
}

#[derive(Clone, Copy)]
struct Row<'a> {
    reading: &'a GaugeData,
    labels: &'a [proto::LabelPair],
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct FamilyKey<'a> {
    prefix: &'a str,
    name: &'a str,
    metric_type: &'a str,
    unit: &'a str,
    counter: bool,
}

struct Family<'a> {
    help: &'a str,
    counter: bool,
    rows: Vec<Row<'a>>,
}

impl Snapshot {
    fn capture(
        manager: &MetricsManager,
        cancel: &CancellationToken,
    ) -> Result<Option<Self>, Error> {
        check_cancel(cancel)?;

        let handles = {
            let state = lock(&manager.telemetry_state);

            if state.generic {
                return Ok(None);
            }

            state.gauges.values().cloned().collect::<Vec<_>>()
        };

        let mut streams = Vec::with_capacity(handles.len());

        for stream in handles {
            check_cancel(cancel)?;

            let mut readings = Vec::with_capacity(stream.gauges.len());

            for reading in stream.gauges.iter() {
                if readings.len().is_multiple_of(CAPTURE_BATCH) {
                    check_cancel(cancel)?;
                }

                readings.push(reading.value().clone());
            }

            streams.push(Stream {
                prefix: stream.metric_name_prefix.clone(),
                help: stream.metric_help.clone(),
                labels: stream.static_labels.clone(),
                readings,
            });
        }

        Ok(Some(Self { streams }))
    }

    fn families(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<BTreeMap<String, Family<'_>>>, Error> {
        let mut groups = HashMap::new();
        let mut visited = 0usize;

        for stream in &self.streams {
            for reading in &stream.readings {
                if visited.is_multiple_of(CAPTURE_BATCH) {
                    check_cancel(cancel)?;
                }

                visited += 1;

                let key = FamilyKey {
                    prefix: &stream.prefix,
                    name: &reading.name,
                    metric_type: &reading.metric_type,
                    unit: &reading.unit,
                    counter: false,
                };

                let row = Row {
                    reading,
                    labels: &stream.labels,
                };

                if !push_row(&mut groups, key, &stream.help, row) {
                    return Ok(None);
                }

                if let MetricSemantics::CumulativeCounter { description, .. } = reading.semantics
                    && reading.semantics.exports_counter(reading.value)
                    && !push_row(
                        &mut groups,
                        FamilyKey {
                            counter: true,
                            ..key
                        },
                        description,
                        row,
                    )
                {
                    return Ok(None);
                }
            }
        }

        let mut families = BTreeMap::<String, Family<'_>>::new();

        for (key, mut family) in groups {
            check_cancel(cancel)?;

            let suffix = if key.counter { "_total" } else { "" };

            let name = format!(
                "{}_{}_{}_{}{suffix}",
                key.prefix, key.name, key.metric_type, key.unit
            );

            match families.entry(name) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(family);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().counter != family.counter || entry.get().help != family.help {
                        return Ok(None);
                    }

                    entry.get_mut().rows.append(&mut family.rows);
                }
            }
        }

        Ok(Some(families))
    }
}

fn push_row<'a>(
    groups: &mut HashMap<FamilyKey<'a>, Family<'a>>,
    key: FamilyKey<'a>,
    help: &'a str,
    row: Row<'a>,
) -> bool {
    let family = groups.entry(key).or_insert_with(|| Family {
        help,
        counter: key.counter,
        rows: Vec::new(),
    });

    if family.help != help {
        return false;
    }

    family.rows.push(row);
    true
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "telemetry export cancelled").into(),
        );
    }

    Ok(())
}

/// Encodes telemetry directly into an identity or gzip response buffer.
pub(super) fn export(
    manager: &MetricsManager,
    gzip_level: Option<u32>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Error> {
    check_cancel(cancel)?;

    if let Some(level) = gzip_level {
        let mut gzip = GzEncoder::new(Vec::new(), Compression::new(level));

        {
            let mut buffered = BufWriter::with_capacity(ENCODE_BATCH, &mut gzip);
            encode(manager, &mut buffered, cancel)?;
            buffered.flush()?;
        }

        check_cancel(cancel)?;

        let body = gzip.finish()?;
        check_cancel(cancel)?;

        return Ok(body);
    }

    let mut buffered = BufWriter::with_capacity(ENCODE_BATCH, Vec::new());
    encode(manager, &mut buffered, cancel)?;

    let body = buffered
        .into_inner()
        .map_err(|error| Error::Io(error.into_error()))?;

    check_cancel(cancel)?;

    Ok(body)
}

fn encode(
    manager: &MetricsManager,
    output: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut writer = CancellableWriter {
        inner: output,
        cancel,
        remaining: ENCODE_BATCH,
    };

    if let Some(snapshot) = Snapshot::capture(manager, cancel)? {
        #[cfg(test)]
        let hook = { lock(&manager.telemetry_state).before_encode.clone() };

        #[cfg(test)]
        if let Some(hook) = hook {
            hook(cancel);
        }

        check_cancel(cancel)?;

        if let Some(families) = snapshot.families(cancel)? {
            encode_families(families, &mut writer, cancel)?;
            return Ok(());
        }
    }

    // Raw collectors and ambiguous family metadata retain the registry encoder's behavior.
    check_cancel(cancel)?;

    let families = manager.telemetry_registry.gather();
    check_cancel(cancel)?;
    TextEncoder::new().encode(&families, &mut writer)
}

fn compare_rows(left: &Row<'_>, right: &Row<'_>) -> Ordering {
    let count_order = (left.labels.len() + left.reading.labels.len())
        .cmp(&(right.labels.len() + right.reading.labels.len()));

    if !count_order.is_eq() {
        return count_order;
    }

    let left_values = left
        .labels
        .iter()
        .map(|label| label.value())
        .chain(left.reading.labels.iter().map(|(_, value)| value.as_str()));

    let right_values = right
        .labels
        .iter()
        .map(|label| label.value())
        .chain(right.reading.labels.iter().map(|(_, value)| value.as_str()));

    left_values.cmp(right_values)
}

fn sort_rows(rows: &mut Vec<Row<'_>>, cancel: &CancellationToken) -> Result<(), Error> {
    for chunk in rows.chunks_mut(SORT_BATCH) {
        check_cancel(cancel)?;
        chunk.sort_by(compare_rows);
    }

    if rows.len() <= SORT_BATCH {
        return Ok(());
    }

    let mut width = SORT_BATCH;
    let mut scratch = Vec::with_capacity(rows.len());

    while width < rows.len() {
        scratch.clear();

        for chunk in rows.chunks(width * 2) {
            let (left, right) = chunk.split_at(width.min(chunk.len()));
            merge_rows(left, right, &mut scratch, cancel)?;
        }

        std::mem::swap(rows, &mut scratch);
        width *= 2;
    }

    Ok(())
}

fn merge_rows<'a>(
    left: &[Row<'a>],
    right: &[Row<'a>],
    output: &mut Vec<Row<'a>>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut left = left.iter().peekable();
    let mut right = right.iter().peekable();

    loop {
        if output.len().is_multiple_of(CAPTURE_BATCH) {
            check_cancel(cancel)?;
        }

        let next = match (left.peek(), right.peek()) {
            (Some(a), Some(b)) if compare_rows(a, b).is_le() => left.next(),
            (Some(_), Some(_)) | (None, Some(_)) => right.next(),
            (Some(_), None) => left.next(),
            (None, None) => break,
        };

        if let Some(row) = next {
            output.push(*row);
        }
    }

    Ok(())
}

fn encode_families(
    families: BTreeMap<String, Family<'_>>,
    writer: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    for (name, mut family) in families {
        sort_rows(&mut family.rows, cancel)?;

        if !family.help.is_empty() {
            write!(writer, "# HELP {name} ")?;
            write_escaped(writer, family.help, false)?;
            writer.write_all(b"\n")?;
        }

        let kind = if family.counter { "counter" } else { "gauge" };
        writeln!(writer, "# TYPE {name} {kind}")?;

        for row in &family.rows {
            write_row(writer, &name, row)?;
        }
    }

    Ok(())
}

fn write_escaped(writer: &mut impl Write, value: &str, quotes: bool) -> io::Result<()> {
    for part in value.split_inclusive(|c| c == '\\' || c == '\n' || (quotes && c == '"')) {
        let escaped = if let Some(prefix) = part.strip_suffix('\\') {
            Some((prefix, b"\\\\".as_slice()))
        } else if let Some(prefix) = part.strip_suffix('\n') {
            Some((prefix, b"\\n".as_slice()))
        } else if quotes {
            part.strip_suffix('"')
                .map(|prefix| (prefix, b"\\\"".as_slice()))
        } else {
            None
        };

        if let Some((prefix, escaped)) = escaped {
            writer.write_all(prefix.as_bytes())?;
            writer.write_all(escaped)?;
        } else {
            writer.write_all(part.as_bytes())?;
        }
    }

    Ok(())
}

fn write_row(writer: &mut impl Write, name: &str, row: &Row<'_>) -> io::Result<()> {
    writer.write_all(name.as_bytes())?;

    let labels = row
        .labels
        .iter()
        .map(|label| (label.name(), label.value()))
        .chain(
            row.reading
                .labels
                .iter()
                .map(|(name, value)| (name.as_ref(), value.as_str())),
        );

    let mut separator = "{";
    let mut has_labels = false;

    for (name, value) in labels {
        write!(writer, "{separator}{name}=\"")?;
        write_escaped(writer, value, true)?;
        writer.write_all(b"\"")?;

        separator = ",";
        has_labels = true;
    }

    if has_labels {
        writer.write_all(b"}")?;
    }

    writeln!(writer, " {}", row.reading.value)
}

struct CancellableWriter<'a, W> {
    inner: &'a mut W,
    cancel: &'a CancellationToken,
    remaining: usize,
}

impl<W: Write> Write for CancellableWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            // write_all retries Interrupted; cancellation must stop the write.
            check_cancel(self.cancel)
                .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted, error))?;

            self.remaining = ENCODE_BATCH;
        }

        let (chunk, _) = bytes.split_at(bytes.len().min(self.remaining));
        let count = self.inner.write(chunk)?;
        self.remaining -= count;

        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        check_cancel(self.cancel)
            .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted, error))?;

        self.inner.flush()
    }
}

#[cfg(test)]
mod tests;
