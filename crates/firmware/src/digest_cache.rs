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

//! A replica-local byte budget for immutable firmware artifacts. The caller
//! supplies retention policy on each operation; this module owns only files,
//! readers, and physical download reservations.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::future::{Future, poll_fn};
use std::io::{self, Read, Seek};
use std::os::unix::fs::MetadataExt;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::FutureExt;
use futures_util::task::AtomicWaker;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use url::Url;

use crate::FirmwareDownloader;
use crate::downloader::{ManagedDownload, ManagedDownloadResource, verify_sha256_file};

const NAMESPACE: &str = "digest-v1";
const MAX_CAPACITY_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_READERS: usize = 32;

/// `ArtifactDigest` is a validated SHA-256 identity. Parsing accepts exactly
/// 64 hexadecimal characters; display always uses lowercase hexadecimal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactDigest([u8; 32]);

impl FromStr for ArtifactDigest {
    type Err = CacheError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        hex::decode_to_slice(value, &mut bytes).map_err(|_| CacheError::InvalidDigest)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

/// `CacheError` distinguishes invalid inputs, exhausted storage, and failures
/// that a caller can report before explicitly retrying with another `open`.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// The digest was not exactly 32 bytes encoded as hexadecimal.
    #[error("firmware digest must contain exactly 64 hexadecimal characters")]
    InvalidDigest,
    /// Managed sources permit anonymous HTTP(S) without query or fragment.
    #[error("firmware source must use anonymous HTTP(S) without query or fragment")]
    InvalidSource,
    /// Capacity must cover one maximum download and must not exceed 1 TiB.
    #[error("firmware cache capacity must cover max_download_bytes and be at most 1 TiB")]
    InvalidCapacity,
    /// Retained files, readers, or downloads prevent the required reservation.
    #[error("firmware cache capacity is exhausted")]
    Capacity,
    /// Releasing the caller's retention lock failed. The lock may already be
    /// released; a successful physical open is discarded before any transfer begins.
    #[error("firmware retention lock release failed")]
    RetentionLockRelease,
    /// Local filesystem or blocking-worker failure.
    #[error("firmware cache I/O failed: {0}")]
    Io(#[from] io::Error),
    /// An admitted transfer failed. A later open explicitly retries it.
    #[error("firmware download failed: {0}")]
    Download(String),
    /// The cache's owning lifecycle has ended.
    #[error("firmware cache cancelled")]
    Cancelled,
}

/// `CacheLookup` either supplies a verified leased reader or asks the caller
/// to poll again. Pending requests do not accumulate queued tasks or waiters.
#[derive(Debug)]
pub enum CacheLookup {
    /// Verified bytes whose file cannot be evicted while physical reads remain.
    Ready(CachedArtifact),
    /// Download/check capacity, reader capacity, or an active transfer is busy.
    Pending,
}

/// `FirmwareArtifactCache` accounts files only in `digest-v1` beneath the
/// supplied root. Clones share one owner; create only one owner per namespace.
/// Legacy URL directories and unknown or nonregular entries are preserved.
#[derive(Clone)]
pub struct FirmwareArtifactCache {
    inner: Arc<CacheInner>,
}

struct CacheInner {
    directory: PathBuf,
    capacity_bytes: u64,
    downloader: FirmwareDownloader,
    cancel: CancellationToken,
    readers: Arc<Semaphore>,
    state: Mutex<CacheState>,
    work: Arc<CacheWork>,
    #[cfg(test)]
    write_queued: Arc<Notify>,
    #[cfg(test)]
    panic_before_write: Arc<AtomicBool>,
    #[cfg(test)]
    transfer_stopped: Notify,
}

// One mutex excludes eviction, read acquisition, and reservation changes.
// Filesystem work runs on bounded blocking workers while holding it.
#[derive(Default)]
struct CacheState {
    initialized: bool,
    entries: HashMap<ArtifactDigest, Entry>,
    last_use: u64,
}

#[derive(Default)]
struct Entry {
    // During a transfer this is its full reservation, which already includes
    // staging bytes. After the last physical owner drops it is actual bytes.
    charged_bytes: u64,
    downloading: bool,
    readers: Arc<AtomicUsize>,
    // There is only one leased inode at a time. A replacement pathname stays
    // separate in accounting until these physical readers have disappeared.
    leased_file: Option<fs::Metadata>,
    last_use: u64,
    failure: Option<String>,
}

impl Entry {
    // All live readers belong to this inode; a replacement is not a second
    // leased generation until those readers finish.
    fn matches_lease(&self, metadata: Option<&fs::Metadata>) -> bool {
        self.leased_file
            .as_ref()
            .zip(metadata)
            .is_some_and(|(leased, current)| {
                leased.dev() == current.dev() && leased.ino() == current.ino()
            })
    }

    fn observe(&mut self, metadata: Option<&fs::Metadata>, partial_bytes: u64) -> io::Result<()> {
        let path_bytes = metadata.map_or(0, fs::Metadata::len);
        let complete_bytes = if self.readers.load(Ordering::Acquire) == 0 {
            self.leased_file = None;
            path_bytes
        } else {
            let leased = self
                .leased_file
                .as_ref()
                .expect("physical readers have leased metadata");
            if self.matches_lease(metadata) {
                leased.len().max(path_bytes)
            } else {
                leased
                    .len()
                    .checked_add(path_bytes)
                    .ok_or_else(byte_count_overflow)?
            }
        };
        self.charged_bytes = complete_bytes
            .checked_add(partial_bytes)
            .ok_or_else(byte_count_overflow)?;
        Ok(())
    }
}

fn byte_count_overflow() -> io::Error {
    io::Error::other("firmware cache byte count overflow")
}

impl CacheState {
    fn prune(&mut self, retained: &HashSet<ArtifactDigest>) {
        // Current retention owns failure history. Empty inactive entries do not
        // need an expiry clock, but physical owners always keep their entry.
        self.entries.retain(|digest, entry| {
            entry.charged_bytes != 0
                || entry.downloading
                || entry.readers.load(Ordering::Acquire) != 0
                || retained.contains(digest)
        });
    }
}

impl FirmwareArtifactCache {
    /// Validates a positive capacity, at most 1 TiB and at least the downloader's
    /// maximum artifact size. Construction performs no filesystem work.
    /// Registers one owner in `owners`; requires a Tokio runtime. The caller
    /// must cancel and drain that set before removing the cache directory or
    /// exiting. Cancellation fences admission and joins accepted physical work;
    /// a worker panic propagates through the owner even after a request vanishes.
    /// The namespace supports one process owner, not a shared filesystem cache.
    pub fn new(
        root: PathBuf,
        capacity_bytes: u64,
        downloader: FirmwareDownloader,
        cancel: CancellationToken,
        owners: &mut JoinSet<()>,
    ) -> Result<Self, CacheError> {
        Self::validate_capacity(capacity_bytes, downloader.max_download_bytes())?;
        let work = Arc::new(CacheWork {
            state: Mutex::new(WorkState {
                closed: false,
                tasks: JoinSet::new(),
            }),
            wake: AtomicWaker::new(),
            cancel: cancel.clone(),
        });
        owners.spawn(work.clone().run());
        Ok(Self {
            inner: Arc::new(CacheInner {
                directory: root.join(NAMESPACE),
                capacity_bytes,
                downloader,
                cancel,
                readers: Arc::new(Semaphore::new(MAX_READERS)),
                state: Mutex::new(CacheState::default()),
                work,
                #[cfg(test)]
                write_queued: Arc::new(Notify::new()),
                #[cfg(test)]
                panic_before_write: Arc::new(AtomicBool::new(false)),
                #[cfg(test)]
                transfer_stopped: Notify::new(),
            }),
        })
    }

    /// Validates the storage budget without filesystem or runtime work. Capacity
    /// must be positive, cover `max_download_bytes`, and not exceed 1 TiB.
    pub fn validate_capacity(
        capacity_bytes: u64,
        max_download_bytes: u64,
    ) -> Result<(), CacheError> {
        if capacity_bytes == 0
            || capacity_bytes < max_download_bytes
            || capacity_bytes > MAX_CAPACITY_BYTES
        {
            return Err(CacheError::InvalidCapacity);
        }
        Ok(())
    }

    /// Opens verified immutable content, or starts one bounded background
    /// transfer and returns `Pending`. Sources must be anonymous HTTP(S), with
    /// no userinfo, query, fragment, or redirects. No request credentials are used.
    ///
    /// `retained` is the caller's current authoritative set, used under the same
    /// exclusion as read acquisition and eviction. Reads, active transfers, and
    /// these digests cannot be evicted. A failure is reported once; the next open
    /// retries. Include the requested digest in `retained`: an idle entry with
    /// no bytes or retention may lose its failure before any caller sees it.
    /// Storage is reserved before source access. Reader admission is
    /// separately bounded at 32, without a queue, so slow readers do not retain
    /// the shared downloader's worker slots. Verified adopted files may be read
    /// even when a reduced budget is already exceeded; only new reservations
    /// require free capacity. Redirect rejection is a source security boundary,
    /// so an origin cannot substitute a different destination.
    /// This wrapper does not lock the retained set. Callers whose authoritative
    /// retained set can grow must use [`Self::open_with_retention_lock`].
    pub async fn open(
        &self,
        source: &Url,
        digest: ArtifactDigest,
        retained: &HashSet<ArtifactDigest>,
    ) -> Result<CacheLookup, CacheError> {
        self.open_with_retention_lock(source, digest, retained, async { Ok(()) })
            .await
    }

    /// Open while the caller holds its retention lock. `release` owns
    /// that lock until polled after physical eviction/reservation or reader
    /// acquisition. Registered work retains it after request cancellation and
    /// releases it before any source transfer or response. Release failure drops
    /// a reader or fails a reservation and starts no download.
    ///
    /// Source, retention, capacity and reader rules are shared with [`Self::open`].
    /// Use this method when authoritative membership can grow concurrently.
    /// `release` must also clean up on drop, since rejection or panic can drop it
    /// without polling. The caller must bound its completion; the cache supplies
    /// no timeout, retains downloader capacity, and drains registered work during
    /// shutdown. Early returns still poll
    /// `release`, whose error takes precedence there. After physical work, an
    /// existing open error takes precedence over a simultaneous release error.
    pub async fn open_with_retention_lock(
        &self,
        source: &Url,
        digest: ArtifactDigest,
        retained: &HashSet<ArtifactDigest>,
        release: impl Future<Output = Result<(), CacheError>> + Send + 'static,
    ) -> Result<CacheLookup, CacheError> {
        if !matches!(source.scheme(), "http" | "https")
            || source.host_str().is_none()
            || !source.username().is_empty()
            || source.password().is_some()
            || source.query().is_some()
            || source.fragment().is_some()
        {
            release.await?;
            return Err(CacheError::InvalidSource);
        }
        if self.inner.cancel.is_cancelled() {
            release.await?;
            return Err(CacheError::Cancelled);
        }
        let Some(capacity) = self.inner.downloader.try_capacity() else {
            release.await?;
            return Ok(CacheLookup::Pending);
        };
        let inner = self.inner.clone();
        let retained = retained.clone();
        let source = source.to_string();
        let capacity = Arc::new(capacity);
        let (response, received) = oneshot::channel();
        let span = tracing::Span::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        self.inner.work.register(
            async move {
                let context = inner.clone();
                let mut opened = blocking(move || context.open(digest, &retained)).await;
                if let Err(error) = release.await {
                    if matches!(&opened, Ok(OpenResult::Download)) {
                        let context = inner.clone();
                        // No transfer started, so do not save a source failure
                        // for the next caller's explicit retry.
                        blocking(move || context.settle(digest, None)).await;
                    }
                    if opened.is_ok() {
                        opened = Err(error);
                    }
                }
                match opened {
                    Ok(OpenResult::Download) => {
                        // The request may disappear after Pending. This registered
                        // parent still owns transport, physical work and settlement.
                        response.send(Ok(CacheLookup::Pending)).ok();
                        let completion = Arc::new(PhysicalCompletion::default());
                        let resource = Arc::new(CacheReservation {
                            completion: completion.clone(),
                            #[cfg(test)]
                            write_queued: inner.write_queued.clone(),
                            #[cfg(test)]
                            panic_before_write: inner.panic_before_write.clone(),
                        });
                        let transferred =
                            AssertUnwindSafe(inner.downloader.managed(ManagedDownload {
                                filename: inner.path(digest),
                                source_url: source,
                                checksum: digest.to_string(),
                                capacity: capacity.clone(),
                                resource,
                                cancel: inner.cancel.clone(),
                            }))
                            .catch_unwind()
                            .await;
                        #[cfg(test)]
                        inner.transfer_stopped.notify_one();
                        completion.wait().await;
                        let error = match &transferred {
                            Ok(result) => result.as_ref().err().cloned(),
                            Err(_) => Some("firmware worker panicked".to_owned()),
                        };
                        blocking(move || inner.settle(digest, error)).await;
                        if let Err(panic) = transferred {
                            resume_unwind(panic);
                        }
                        assert!(
                            !completion.panicked.load(Ordering::Acquire),
                            "managed firmware physical worker panicked"
                        );
                        WorkResult::Downloaded {
                            _capacity: capacity,
                        }
                    }
                    result => WorkResult::Opened {
                        response,
                        result: result.map(|opened| match opened {
                            OpenResult::Lookup(lookup) => lookup,
                            OpenResult::Download => unreachable!(),
                        }),
                        _capacity: capacity,
                    },
                }
            }
            .instrument(span)
            .with_subscriber(dispatch),
        )?;
        received
            .await
            .map_err(|_| CacheError::Io(io::Error::other("firmware cache owner failed")))?
    }
}

enum OpenResult {
    Lookup(CacheLookup),
    Download,
}

// The owner reaps before delivering completed reads/opens. Their admission
// permits bound unreaped parents as well as active work; request disappearance
// does not detach the physical work or hide its panic from Core's join set.
struct CacheWork {
    state: Mutex<WorkState>,
    wake: AtomicWaker,
    cancel: CancellationToken,
}

struct WorkState {
    closed: bool,
    tasks: JoinSet<WorkResult>,
}

enum WorkResult {
    Opened {
        response: oneshot::Sender<Result<CacheLookup, CacheError>>,
        result: Result<CacheLookup, CacheError>,
        _capacity: Arc<OwnedSemaphorePermit>,
    },
    Downloaded {
        _capacity: Arc<OwnedSemaphorePermit>,
    },
    Read {
        response: oneshot::Sender<io::Result<Vec<u8>>>,
        result: io::Result<Vec<u8>>,
        _lease: Arc<ReaderLease>,
    },
}

impl CacheWork {
    fn register(
        &self,
        task: impl Future<Output = WorkResult> + Send + 'static,
    ) -> Result<(), CacheError> {
        let mut state = self
            .state
            .lock()
            .expect("firmware work registration poisoned");
        if state.closed || self.cancel.is_cancelled() {
            return Err(CacheError::Cancelled);
        }
        let cancel = self.cancel.clone();
        state.tasks.spawn(async move {
            match AssertUnwindSafe(task).catch_unwind().await {
                Ok(result) => result,
                Err(panic) => {
                    // Stop admission at the failing child as well as the
                    // owner; request loss must not hide a poisoned worker.
                    cancel.cancel();
                    resume_unwind(panic)
                }
            }
        });
        self.wake.wake();
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        let mut closed = false;
        let mut panic = None;
        loop {
            let next = poll_fn(|context| {
                self.wake.register(context.waker());
                let mut state = self
                    .state
                    .lock()
                    .expect("firmware work registration poisoned");
                match state.tasks.poll_join_next(context) {
                    Poll::Ready(None) if !state.closed => Poll::Pending,
                    result => result,
                }
            });
            tokio::select! {
                biased;
                _ = self.cancel.cancelled(), if !closed => {
                    self.state.lock().expect("firmware work registration poisoned").closed = true;
                    closed = true;
                }
                result = next => match result {
                    None => break,
                    Some(Ok(WorkResult::Opened { response, result, .. })) => {
                        response.send(result).ok();
                    }
                    Some(Ok(WorkResult::Read { response, result, .. })) => {
                        response.send(result).ok();
                    }
                    Some(Ok(WorkResult::Downloaded { .. })) => {}
                    Some(Err(error)) => {
                        self.cancel.cancel();
                        if error.is_panic() && panic.is_none() { panic = Some(error.into_panic()); }
                    }
                },
            }
        }
        if let Some(panic) = panic {
            resume_unwind(panic);
        }
    }
}

async fn blocking<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let span = tracing::Span::current();
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    match tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&dispatch, || span.in_scope(operation))
    })
    .await
    {
        Ok(result) => result,
        Err(error) if error.is_panic() => resume_unwind(error.into_panic()),
        Err(error) => panic!("firmware blocking worker failed: {error}"),
    }
}

impl CacheInner {
    fn path(&self, digest: ArtifactDigest) -> PathBuf {
        self.directory.join(digest.to_string())
    }

    fn open(
        self: Arc<Self>,
        digest: ArtifactDigest,
        retained: &HashSet<ArtifactDigest>,
    ) -> Result<OpenResult, CacheError> {
        let mut state = self
            .state
            .lock()
            .expect("firmware cache state mutex poisoned");
        if self.cancel.is_cancelled() {
            return Err(CacheError::Cancelled);
        }
        if !state.initialized {
            self.reconcile(&mut state)?;
        }
        if let Some(entry) = state.entries.get_mut(&digest) {
            if entry.downloading {
                return Ok(OpenResult::Lookup(CacheLookup::Pending));
            }
            if let Some(error) = entry.failure.take() {
                state.prune(retained);
                return Err(CacheError::Download(error));
            }
        }
        let path = self.path(digest);
        let partial = partial_path(&path);
        let metadata = regular_metadata(&path)?;
        let partial_length = regular_bytes(&partial)?;
        state.prune(retained);
        let entry = state.entries.entry(digest).or_default();
        entry.observe(metadata.as_ref(), partial_length.unwrap_or(0))?;
        if entry.readers.load(Ordering::Acquire) != 0 && !entry.matches_lease(metadata.as_ref()) {
            // The pathname may be absent or already replaced. Keep the old
            // inode and replacement charged, without leasing two generations.
            return Ok(OpenResult::Lookup(CacheLookup::Pending));
        }
        if metadata.is_some() {
            let mut file = File::open(&path)?;
            match verify_sha256_file(&mut file, &digest.to_string()) {
                Ok(()) => {
                    let Ok(reader_permit) = self.readers.clone().try_acquire_owned() else {
                        return Ok(OpenResult::Lookup(CacheLookup::Pending));
                    };
                    let verified_metadata = file.metadata()?;
                    let entry = state.entries.get(&digest).expect("opened artifact exists");
                    if entry.readers.load(Ordering::Acquire) != 0
                        && !entry.matches_lease(Some(&verified_metadata))
                    {
                        self.refresh_entry(
                            digest,
                            state
                                .entries
                                .get_mut(&digest)
                                .expect("opened artifact exists"),
                        )?;
                        return Ok(OpenResult::Lookup(CacheLookup::Pending));
                    }
                    file.rewind()?;
                    state.last_use = state.last_use.saturating_add(1);
                    let last_use = state.last_use;
                    let entry = state.entries.entry(digest).or_default();
                    entry.last_use = last_use;
                    if entry.readers.load(Ordering::Acquire) == 0 {
                        entry.leased_file = Some(verified_metadata.clone());
                    }
                    entry.readers.fetch_add(1, Ordering::AcqRel);
                    let lease = Arc::new(ReaderLease {
                        cache: self.clone(),
                        readers: entry.readers.clone(),
                        _capacity: reader_permit,
                    });
                    self.refresh_entry(digest, entry)?;
                    return Ok(OpenResult::Lookup(CacheLookup::Ready(CachedArtifact {
                        content_length: verified_metadata.len(),
                        file: Arc::new(Mutex::new(file)),
                        lease,
                        read: None,
                        buffered: Vec::new(),
                        offset: 0,
                        eof: false,
                    })));
                }
                Err(error) => {
                    if state
                        .entries
                        .get(&digest)
                        .is_some_and(|entry| entry.readers.load(Ordering::Acquire) != 0)
                    {
                        return Err(CacheError::Io(io::Error::other(error.to_string())));
                    }
                    fs::remove_file(&path)?;
                }
            }
        }
        if partial_length.is_some() {
            fs::remove_file(&partial)?;
        }
        let reservation = self.downloader.max_download_bytes();
        self.ensure_space(&mut state, reservation, retained, digest)?;
        state.last_use = state.last_use.saturating_add(1);
        let last_use = state.last_use;
        state.entries.insert(
            digest,
            Entry {
                charged_bytes: reservation,
                downloading: true,
                last_use,
                ..Entry::default()
            },
        );
        Ok(OpenResult::Download)
    }

    fn settle(&self, digest: ArtifactDigest, error: Option<String>) {
        let mut state = self
            .state
            .lock()
            .expect("firmware cache state mutex poisoned");
        let entry = state
            .entries
            .get_mut(&digest)
            .expect("download reservation exists");
        let path = self.path(digest);
        let observed = (|| -> io::Result<u64> {
            let partial = partial_path(&path);
            if regular_bytes(&partial)?.is_some() {
                fs::remove_file(&partial)?;
            }
            Ok(regular_bytes(&path)?.unwrap_or(0))
        })();
        entry.downloading = false;
        entry.failure = error;
        match observed {
            Ok(bytes) => entry.charged_bytes = bytes,
            Err(error) => {
                let cleanup = format!("unable to settle firmware cache reservation: {error}");
                entry.failure = Some(match entry.failure.take() {
                    Some(original) => format!("{original}; {cleanup}"),
                    None => cleanup,
                });
            }
        }
    }

    /// Adopts only owned regular files after restart. An orphan partial stays
    /// charged if its removal fails; unknown entries are never repaired here.
    fn reconcile(&self, state: &mut CacheState) -> Result<(), CacheError> {
        fs::create_dir_all(&self.directory)?;
        if !fs::symlink_metadata(&self.directory)?.file_type().is_dir() {
            return Err(CacheError::Io(io::Error::other(
                "firmware cache namespace is not a directory",
            )));
        }
        let mut entries: HashMap<ArtifactDigest, Entry> = HashMap::new();
        for file in fs::read_dir(&self.directory)? {
            let file = file?;
            let metadata = fs::symlink_metadata(file.path())?;
            if !metadata.file_type().is_file() {
                continue;
            }
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let (name, partial) = name
                .strip_suffix(".download")
                .map_or((name, false), |name| (name, true));
            let Ok(digest) = name.parse::<ArtifactDigest>() else {
                continue;
            };
            // Only our lowercase filenames belong to this namespace. Preserve
            // differently spelled and unknown files rather than adopting them.
            if name != digest.to_string() {
                continue;
            }
            if partial && fs::remove_file(file.path()).is_ok() {
                continue;
            }
            let entry = entries.entry(digest).or_default();
            entry.charged_bytes =
                entry
                    .charged_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| {
                        CacheError::Io(io::Error::other("firmware cache byte count overflow"))
                    })?;
        }
        state.entries = entries;
        state.initialized = true;
        Ok(())
    }

    fn refresh_entry(&self, digest: ArtifactDigest, entry: &mut Entry) -> io::Result<()> {
        if entry.downloading {
            return Ok(());
        }
        let path = self.path(digest);
        let metadata = regular_metadata(&path);
        let partial_bytes = regular_bytes(&partial_path(&path));
        let uncertain = metadata.is_err() || partial_bytes.is_err();
        let previous = entry.charged_bytes;
        entry.observe(
            metadata.ok().flatten().as_ref(),
            partial_bytes.ok().flatten().unwrap_or(0),
        )?;
        if uncertain {
            // A failed observation cannot free any remembered bytes. Known
            // growth or a replacement can still raise this conservative charge.
            entry.charged_bytes = entry.charged_bytes.max(previous);
        }
        Ok(())
    }

    fn ensure_space(
        &self,
        state: &mut CacheState,
        needed: u64,
        retained: &HashSet<ArtifactDigest>,
        protected: ArtifactDigest,
    ) -> Result<(), CacheError> {
        // Retention protects bytes from eviction, not their old size from
        // reconciliation. Refresh every observable entry before admission.
        for (digest, entry) in &mut state.entries {
            self.refresh_entry(*digest, entry)?;
        }
        let mut obstructed = HashSet::new();
        loop {
            let used = state
                .entries
                .values()
                .try_fold(0_u64, |sum, entry| sum.checked_add(entry.charged_bytes))
                .ok_or_else(|| {
                    CacheError::Io(io::Error::other("firmware cache byte count overflow"))
                })?;
            if needed <= self.capacity_bytes && used <= self.capacity_bytes - needed {
                return Ok(());
            }
            let candidate = state
                .entries
                .iter()
                .filter(|(digest, entry)| {
                    **digest != protected
                        && !retained.contains(digest)
                        && !entry.downloading
                        && entry.readers.load(Ordering::Acquire) == 0
                        && !obstructed.contains(*digest)
                        && entry.charged_bytes != 0
                })
                .min_by_key(|(digest, entry)| (entry.last_use, **digest))
                .map(|(digest, _)| *digest)
                .ok_or(CacheError::Capacity)?;
            let path = self.path(candidate);
            // Keep the charge until each unlink physically succeeds. A failed
            // unlink must not turn occupied disk into apparently free capacity.
            let removed = (|| -> io::Result<()> {
                for path in [path.clone(), partial_path(&path)] {
                    if regular_bytes(&path)?.is_some() {
                        fs::remove_file(&path)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = removed {
                let entry = state
                    .entries
                    .get_mut(&candidate)
                    .expect("eviction candidate exists");
                self.refresh_entry(candidate, entry)?;
                tracing::warn!(artifact_digest = %candidate, %error, "Unable to evict firmware artifact");
                obstructed.insert(candidate);
                continue;
            }
            state.entries.remove(&candidate);
        }
    }
}

fn partial_path(path: &Path) -> PathBuf {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".download");
    partial.into()
}

fn regular_bytes(path: &Path) -> io::Result<Option<u64>> {
    regular_metadata(path).map(|metadata| metadata.map(|metadata| metadata.len()))
}

fn regular_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(metadata)),
        Ok(_) => Err(io::Error::other(format!(
            "firmware cache entry is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Default)]
struct PhysicalCompletion {
    done: AtomicBool,
    panicked: AtomicBool,
    notify: Notify,
}

impl PhysicalCompletion {
    async fn wait(&self) {
        while !self.done.load(Ordering::Acquire) {
            self.notify.notified().await;
        }
    }
}

struct CacheReservation {
    completion: Arc<PhysicalCompletion>,
    #[cfg(test)]
    write_queued: Arc<Notify>,
    #[cfg(test)]
    panic_before_write: Arc<AtomicBool>,
}

impl ManagedDownloadResource for CacheReservation {
    fn panicked(&self) {
        self.completion.panicked.store(true, Ordering::Release);
    }
    #[cfg(test)]
    fn write_queued(&self) {
        self.write_queued.notify_one();
    }
    #[cfg(test)]
    fn before_write(&self) {
        assert!(
            !self.panic_before_write.swap(false, Ordering::AcqRel),
            "injected physical managed-write panic"
        );
    }
}

impl Drop for CacheReservation {
    fn drop(&mut self) {
        self.completion.done.store(true, Ordering::Release);
        self.completion.notify.notify_one();
    }
}

struct ReaderLease {
    cache: Arc<CacheInner>,
    readers: Arc<AtomicUsize>,
    _capacity: OwnedSemaphorePermit,
}

impl Drop for ReaderLease {
    fn drop(&mut self) {
        self.readers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            })
            .expect("reader lease was counted");
    }
}

/// `CachedArtifact` streams the same file descriptor that passed SHA-256
/// verification. Its reader lease and admission slot survive queued or running
/// physical reads after the response or its read future is dropped.
pub struct CachedArtifact {
    content_length: u64,
    file: Arc<Mutex<File>>,
    lease: Arc<ReaderLease>,
    read: Option<oneshot::Receiver<io::Result<Vec<u8>>>>,
    buffered: Vec<u8>,
    offset: usize,
    eof: bool,
}

impl fmt::Debug for CachedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedArtifact")
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

impl CachedArtifact {
    /// Returns the verified file length, in bytes.
    pub fn content_length(&self) -> u64 {
        self.content_length
    }
}

impl AsyncRead for CachedArtifact {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let reader = self.get_mut();
        if reader.lease.cache.cancel.is_cancelled() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "firmware cache cancelled",
            )));
        }
        if output.remaining() == 0 || reader.eof {
            return Poll::Ready(Ok(()));
        }
        if reader.offset == reader.buffered.len() {
            if reader.read.is_none() {
                let file = reader.file.clone();
                let lease = reader.lease.clone();
                let (response, received) = oneshot::channel();
                let span = tracing::Span::current();
                let dispatch = tracing::dispatcher::get_default(Clone::clone);
                let worker_lease = lease.clone();
                let work = async move {
                    let result = blocking(move || {
                        let _lease = worker_lease;
                        let read_file = file;
                        let mut contents = vec![0; 64 * 1024];
                        let length = read_file
                            .lock()
                            .expect("firmware reader mutex poisoned")
                            .read(&mut contents)?;
                        contents.truncate(length);
                        Ok(contents)
                    })
                    .await;
                    WorkResult::Read {
                        response,
                        result,
                        _lease: lease,
                    }
                }
                .instrument(span)
                .with_subscriber(dispatch);
                reader.lease.cache.work.register(work).map_err(|_| {
                    io::Error::new(io::ErrorKind::ConnectionAborted, "firmware cache cancelled")
                })?;
                reader.read = Some(received);
            }
            let read = reader.read.as_mut().expect("read worker exists");
            let result = match Pin::new(read).poll(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result,
            };
            reader.read = None;
            let contents = result.map_err(io::Error::other)??;
            reader.offset = 0;
            reader.buffered = contents;
            if reader.buffered.is_empty() {
                reader.eof = true;
                return Poll::Ready(Ok(()));
            }
        }
        let length = output
            .remaining()
            .min(reader.buffered.len() - reader.offset);
        output.put_slice(&reader.buffered[reader.offset..reader.offset + length]);
        reader.offset += length;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests;
