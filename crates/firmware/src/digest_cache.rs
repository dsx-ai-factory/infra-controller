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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use futures_util::task::AtomicWaker;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use url::Url;

use crate::FirmwareDownloader;
use crate::downloader::{ManagedDownload, ManagedDownloadResource, sha256_file};

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
/// to poll again with fresh retention membership.
#[derive(Debug)]
pub enum CacheLookup {
    /// Verified bytes whose file cannot be evicted while physical reads remain.
    Ready(CachedArtifact),
    /// No reader is ready. A worker may still be queued, capacity may be busy,
    /// a transfer may be active, or a corrupt or replaced inode may need a new open.
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
    #[cfg(feature = "test-support")]
    gates: crate::test_support::CacheGates,
    #[cfg(test)]
    write_queued: Arc<Notify>,
    #[cfg(test)]
    panic_before_write: Arc<AtomicBool>,
    #[cfg(test)]
    transfer_stopped: Notify,
    #[cfg(test)]
    corrupt_file_verified: Notify,
}

// One mutex excludes eviction, read acquisition, and reservation changes.
// Once an opened descriptor has a reader lease, SHA-256 and streaming reads
// use that descriptor outside the mutex.
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
    readers: Arc<Mutex<ActiveReaders>>,
    last_use: u64,
    failure: Option<String>,
}

// The accounting descriptor stays open while any reader of this inode exists.
// Registration and final close share this mutex. A metadata observer holds
// the cache-state mutex until it drops its own reference to the descriptor.
#[derive(Default)]
struct ActiveReaders {
    count: usize,
    file: Option<Arc<File>>,
}

impl Entry {
    fn reader_count(&self) -> usize {
        self.readers
            .lock()
            .expect("cache reader mutex poisoned")
            .count
    }

    fn observe(
        &mut self,
        metadata: Option<&fs::Metadata>,
        partial_bytes: u64,
    ) -> io::Result<Option<fs::Metadata>> {
        let path_bytes = metadata.map_or(0, fs::Metadata::len);
        let file = self
            .readers
            .lock()
            .expect("cache reader mutex poisoned")
            .file
            .clone();
        let leased = file.map(|file| file.metadata()).transpose()?;
        let complete_bytes = if let Some(leased) = &leased {
            // A leased inode can grow or be unlinked while verification or a
            // stream is running. Charge its observed size even without a path.
            if same_inode(leased, metadata) {
                leased.len().max(path_bytes)
            } else {
                leased
                    .len()
                    .checked_add(path_bytes)
                    .ok_or_else(byte_count_overflow)?
            }
        } else {
            path_bytes
        };
        self.charged_bytes = complete_bytes
            .checked_add(partial_bytes)
            .ok_or_else(byte_count_overflow)?;
        Ok(leased)
    }
}

fn same_inode(opened: &fs::Metadata, path: Option<&fs::Metadata>) -> bool {
    path.is_some_and(|path| path.dev() == opened.dev() && path.ino() == opened.ino())
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
                || entry.reader_count() != 0
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
                #[cfg(feature = "test-support")]
                gates: Default::default(),
                #[cfg(test)]
                write_queued: Arc::new(Notify::new()),
                #[cfg(test)]
                panic_before_write: Arc::new(AtomicBool::new(false)),
                #[cfg(test)]
                transfer_stopped: Notify::new(),
                #[cfg(test)]
                corrupt_file_verified: Notify::new(),
            }),
        })
    }

    /// Test-only synchronization at preparation, physical open and verification.
    #[cfg(feature = "test-support")]
    pub fn test_gates(&self) -> &crate::test_support::CacheGates {
        &self.inner.gates
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
    /// retries. Removing a corrupt cached file returns `Pending`. Another open
    /// must supply fresh retention before reserving its replacement. Include
    /// the requested digest in `retained` so an idle entry with no bytes keeps
    /// its failure until the caller sees it.
    /// Storage is reserved before source access. Reader admission is
    /// separately bounded at 32, without a queue, so slow readers do not retain
    /// the shared downloader's worker slots. Verified adopted files may be read
    /// even when a reduced budget is already exceeded; only new reservations
    /// require free capacity. Redirect rejection is a source security boundary,
    /// so an origin cannot substitute a different destination.
    /// This wrapper does not lock the retained set. Callers whose authoritative
    /// retained set can grow must protect it with [`Self::open_with_retention_lock`]
    /// or call [`Self::prepare_open`] before acquiring their lock.
    pub async fn open(
        &self,
        source: &Url,
        digest: ArtifactDigest,
        retained: &HashSet<ArtifactDigest>,
    ) -> Result<CacheLookup, CacheError> {
        self.open_with_retention_lock(source, digest, retained, async { Ok(()) })
            .await
    }

    /// Open while the caller holds its retention lock, including time queued
    /// for a blocking worker. `release` owns
    /// that lock until polled after physical eviction/reservation or reader
    /// acquisition. Registered work retains it after request cancellation and
    /// releases it before verifying the leased descriptor, downloading, or
    /// responding. Release failure drops a reader or fails a reservation and
    /// starts no download.
    ///
    /// Source, retention, capacity and reader rules are shared with [`Self::open`].
    /// Use this method when authoritative membership can grow concurrently.
    /// `release` must also clean up on drop, since rejection or panic can drop it
    /// without polling. The caller must bound its completion; the cache supplies
    /// no timeout, retains downloader capacity, and drains registered work during
    /// shutdown. Invalid input, cancellation before registration and a busy
    /// downloader poll `release` and propagate its error. If the owner refuses
    /// registration, the callback is dropped. After physical work, an existing
    /// open error takes precedence over a simultaneous release error.
    pub async fn open_with_retention_lock(
        &self,
        source: &Url,
        digest: ArtifactDigest,
        retained: &HashSet<ArtifactDigest>,
        release: impl Future<Output = Result<(), CacheError>> + Send + 'static,
    ) -> Result<CacheLookup, CacheError> {
        if !valid_source(source) {
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
        let capacity = Arc::new(capacity);
        let worker = self.schedule_open(capacity.clone(), None)?;
        submit_open(worker, source, digest, retained.clone(), release, capacity).await
    }

    /// Start a worker while the caller authorizes the request. Each preparation
    /// requires its own request admission permit, held through handoff or until
    /// the owner reaps an abandoned worker. Downloader capacity is acquired
    /// only for an authorized request with a running worker. A started worker
    /// occupies a blocking thread until handoff, drop, or cache cancellation.
    /// Check out any database connection first if checkout needs a blocking
    /// thread. Then prepare the worker, acquire retention exclusion, read current
    /// membership, and pass the lock's release callback to [`PreparedCacheOpen::open`].
    /// Drop the preparation if authorization fails. Registration after shutdown
    /// returns [`CacheError::Cancelled`].
    pub fn prepare_open(
        &self,
        admission: Arc<OwnedSemaphorePermit>,
    ) -> Result<PreparedCacheOpen, CacheError> {
        let (started, ready) = oneshot::channel();
        let worker = self.schedule_open(admission, Some(started))?;
        Ok(PreparedCacheOpen {
            worker,
            ready,
            cache: self.inner.clone(),
        })
    }

    fn schedule_open(
        &self,
        admission: Arc<OwnedSemaphorePermit>,
        started: Option<oneshot::Sender<()>>,
    ) -> Result<oneshot::Sender<OpenRequest>, CacheError> {
        let inner = self.inner.clone();
        let (send, mut receive) = oneshot::channel::<OpenRequest>();
        let span = tracing::Span::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        self.inner.work.register(
            async move {
                let context = inner.clone();
                let physical = blocking(move || {
                    if let Some(started) = started {
                        // Signal from the running thread before it waits for the snapshot.
                        if started.send(()).is_err() {
                            return Err(admission);
                        }
                        #[cfg(feature = "test-support")]
                        context.gates.prepared.run();
                    }
                    // Only channel handoff and cancellation belong here. Timers
                    // or I/O would make progress depend on the caller's runtime.
                    let request = tokio::runtime::Handle::current().block_on(async {
                        tokio::select! {
                            biased;
                            _ = context.cancel.cancelled() => {
                                // Close before draining. A successful sender transferred
                                // its release callback, even if shutdown just won the race.
                                receive.close();
                                receive.try_recv().ok()
                            }
                            request = &mut receive => request.ok(),
                        }
                    });
                    let Some(request) = request else {
                        return Err(admission);
                    };
                    // The submitted open owns downloader capacity through settlement.
                    drop(admission);
                    let opened = context.open(request.digest, &request.retained);
                    Ok((opened, request))
                })
                .await;
                let (mut opened, request) = match physical {
                    Ok(opened) => opened,
                    Err(admission) => {
                        return WorkResult::Finished {
                            _capacity: admission,
                        };
                    }
                };
                let OpenRequest {
                    digest,
                    source,
                    release,
                    response,
                    capacity,
                    retained,
                } = request;
                drop(retained);
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
                let result = match opened {
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
                        return WorkResult::Finished {
                            _capacity: capacity,
                        };
                    }
                    Ok(OpenResult::Verify(lease)) => {
                        if inner.cancel.is_cancelled() {
                            Err(CacheError::Cancelled)
                        } else {
                            // Shutdown drains even a queued verifier. It keeps its reader
                            // lease and checks cancellation before its first read. The
                            // owner returns downloader capacity when it reaps the result.
                            blocking(move || CachedArtifact::verify(lease, digest)).await
                        }
                    }
                    Ok(OpenResult::Pending) => Ok(CacheLookup::Pending),
                    Err(error) => Err(error),
                };
                WorkResult::Opened {
                    response,
                    result,
                    _capacity: capacity,
                }
            }
            .instrument(span)
            .with_subscriber(dispatch),
        )?;
        Ok(send)
    }
}

fn valid_source(source: &Url) -> bool {
    matches!(source.scheme(), "http" | "https")
        && source.host_str().is_some()
        && source.username().is_empty()
        && source.password().is_none()
        && source.query().is_none()
        && source.fragment().is_none()
}

/// A single open prepared before the caller reads fresh retention membership.
/// A busy cache returns Pending after the caller releases its snapshot.
pub struct PreparedCacheOpen {
    worker: oneshot::Sender<OpenRequest>,
    ready: oneshot::Receiver<()>,
    cache: Arc<CacheInner>,
}

struct OpenRequest {
    digest: ArtifactDigest,
    source: String,
    retained: HashSet<ArtifactDigest>,
    release: BoxFuture<'static, Result<(), CacheError>>,
    response: oneshot::Sender<Result<CacheLookup, CacheError>>,
    capacity: Arc<OwnedSemaphorePermit>,
}

impl PreparedCacheOpen {
    /// Give a running worker ownership of the current retention lock through
    /// physical open or reservation. A queued worker or busy downloader returns
    /// Pending after release if the cache is still running. A later open must
    /// supply fresh membership.
    /// The release future must finish within a caller-enforced deadline and
    /// clean up on drop. Early rejection polls release and propagates its error.
    /// After handoff the cache polls release even if the caller disappears.
    /// A physical open error takes precedence over a simultaneous release error.
    pub async fn open(
        mut self,
        source: &Url,
        digest: ArtifactDigest,
        retained: HashSet<ArtifactDigest>,
        release: impl Future<Output = Result<(), CacheError>> + Send + 'static,
    ) -> Result<CacheLookup, CacheError> {
        if !valid_source(source) {
            drop(self.worker);
            release.await?;
            return Err(CacheError::InvalidSource);
        }
        if self.cache.cancel.is_cancelled() {
            drop(self.worker);
            release.await?;
            return Err(CacheError::Cancelled);
        }
        let capacity = match self.ready.try_recv() {
            Ok(()) => self.cache.downloader.try_capacity(),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => {
                release.await?;
                return Err(CacheError::Cancelled);
            }
        };
        let Some(capacity) = capacity else {
            drop(self.worker);
            release.await?;
            self.cache.check_cancelled()?;
            return Ok(CacheLookup::Pending);
        };
        submit_open(
            self.worker,
            source,
            digest,
            retained,
            release,
            Arc::new(capacity),
        )
        .await
    }
}

async fn submit_open(
    worker: oneshot::Sender<OpenRequest>,
    source: &Url,
    digest: ArtifactDigest,
    retained: HashSet<ArtifactDigest>,
    release: impl Future<Output = Result<(), CacheError>> + Send + 'static,
    capacity: Arc<OwnedSemaphorePermit>,
) -> Result<CacheLookup, CacheError> {
    let (response, received) = oneshot::channel();
    if let Err(request) = worker.send(OpenRequest {
        digest,
        source: source.to_string(),
        retained,
        release: release.boxed(),
        response,
        capacity,
    }) {
        request.release.await?;
        return Err(CacheError::Cancelled);
    }
    received
        .await
        .map_err(|_| CacheError::Io(io::Error::other("firmware cache owner failed")))?
}

enum OpenResult {
    Pending,
    Verify(Arc<ReaderLease>),
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
    Finished {
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
                        // Shutdown can start after verification but before delivery.
                        // Release the reader here instead of starting a new stream.
                        if self.cancel.is_cancelled() && matches!(result, Ok(CacheLookup::Ready(_))) {
                            response.send(Err(CacheError::Cancelled)).ok();
                        } else {
                            response.send(result).ok();
                        }
                    }
                    Some(Ok(WorkResult::Read { response, result, .. })) => {
                        response.send(result).ok();
                    }
                    Some(Ok(WorkResult::Finished { .. })) => {}
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
    fn check_cancelled(&self) -> Result<(), CacheError> {
        if self.cancel.is_cancelled() {
            Err(CacheError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn checksum(&self, file: impl Read) -> Result<[u8; 32], CacheError> {
        sha256_file(file, || self.check_cancelled())
    }

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
        self.check_cancelled()?;
        #[cfg(feature = "test-support")]
        self.gates.open.run();
        if !state.initialized {
            self.reconcile(&mut state)?;
        }
        if let Some(entry) = state.entries.get_mut(&digest) {
            if entry.downloading {
                return Ok(OpenResult::Pending);
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
        let CacheState {
            entries, last_use, ..
        } = &mut *state;
        let entry = entries.entry(digest).or_default();
        let leased = entry.observe(metadata.as_ref(), partial_length.unwrap_or(0))?;
        if leased
            .as_ref()
            .is_some_and(|leased| !same_inode(leased, metadata.as_ref()))
        {
            return Ok(OpenResult::Pending);
        }
        if metadata.is_some() {
            let Ok(reader_permit) = self.readers.clone().try_acquire_owned() else {
                return Ok(OpenResult::Pending);
            };
            let file = Arc::new(File::open(&path)?);
            let opened = file.metadata()?;
            if leased
                .as_ref()
                .is_some_and(|leased| !same_inode(leased, Some(&opened)))
            {
                self.refresh_entry(digest, entry)?;
                return Ok(OpenResult::Pending);
            }
            // Every reader has its own cursor. All leases on this generation
            // keep one descriptor available for byte accounting until they finish.
            {
                let mut readers = entry.readers.lock().expect("cache reader mutex poisoned");
                readers.file.get_or_insert_with(|| file.clone());
                readers.count += 1;
            }
            let lease = Arc::new(ReaderLease {
                file,
                _registration: ReaderRegistration(entry.readers.clone()),
                cache: self.clone(),
                _capacity: reader_permit,
            });
            self.refresh_entry(digest, entry)?;
            *last_use = last_use.saturating_add(1);
            entry.last_use = *last_use;
            return Ok(OpenResult::Verify(lease));
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
                        && entry.reader_count() == 0
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
    // Fields drop in declaration order. Close the stream descriptor before
    // unregistering this reader, including when verification returns an error.
    file: Arc<File>,
    _registration: ReaderRegistration,
    cache: Arc<CacheInner>,
    _capacity: OwnedSemaphorePermit,
}

struct ReaderRegistration(Arc<Mutex<ActiveReaders>>);

impl Drop for ReaderRegistration {
    fn drop(&mut self) {
        let mut readers = self.0.lock().expect("cache reader mutex poisoned");
        readers.count -= 1;
        if readers.count == 0 {
            readers.file = None;
        }
    }
}

/// `CachedArtifact` streams the same file descriptor that passed SHA-256
/// verification. Its reader lease and admission slot survive queued or running
/// physical reads after the response or its read future is dropped.
pub struct CachedArtifact {
    content_length: u64,
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
    fn verify(lease: Arc<ReaderLease>, digest: ArtifactDigest) -> Result<CacheLookup, CacheError> {
        let mut file = lease.file.as_ref();
        let cache = &lease.cache;
        #[cfg(feature = "test-support")]
        cache.gates.verify.run();
        let actual = cache.checksum(file)?;
        cache.check_cancelled()?;
        let opened = file.metadata()?;
        if actual != digest.0 {
            // Other readers keep their descriptors and charges. Only unlink
            // this corrupt generation under the cache owner's filesystem lock.
            #[cfg(test)]
            cache.corrupt_file_verified.notify_one();
            let _state = cache
                .state
                .lock()
                .expect("firmware cache state mutex poisoned");
            cache.check_cancelled()?;
            let path = cache.path(digest);
            let current = regular_metadata(&path);
            cache.check_cancelled()?;
            if same_inode(&opened, current?.as_ref()) {
                fs::remove_file(path)?;
            }
            return Ok(CacheLookup::Pending);
        }
        file.rewind()?;
        Ok(CacheLookup::Ready(Self {
            content_length: opened.len(),
            lease,
            read: None,
            buffered: Vec::new(),
            offset: 0,
            eof: false,
        }))
    }

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
                let lease = reader.lease.clone();
                let (response, received) = oneshot::channel();
                let span = tracing::Span::current();
                let dispatch = tracing::dispatcher::get_default(Clone::clone);
                let worker_lease = lease.clone();
                let work = async move {
                    let result = blocking(move || {
                        let mut contents = vec![0; 64 * 1024];
                        let length = worker_lease.file.as_ref().read(&mut contents)?;
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
