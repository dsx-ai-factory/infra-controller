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

use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use carbide_test_support::Outcome::{Fails, Yields};
use carbide_test_support::{Case, check_cases};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::ArtifactStatus;
use crate::downloader::FirmwareDownloadConfig;

fn digest(contents: &[u8]) -> ArtifactDigest {
    hex::encode(Sha256::digest(contents)).parse().unwrap()
}

struct CacheFixture {
    cache: FirmwareArtifactCache,
    owners: JoinSet<()>,
}

impl std::ops::Deref for CacheFixture {
    type Target = FirmwareArtifactCache;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl CacheFixture {
    async fn finish(mut self) {
        self.inner.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = self.owners.join_next().await {
                result.expect("cache owner failed");
            }
        })
        .await
        .expect("cache owner did not drain");
    }
}

fn cache(root: &Path, capacity: u64, workers: usize) -> CacheFixture {
    let downloader = FirmwareDownloader::with_config(
        FirmwareDownloadConfig {
            max_concurrent_downloads: workers,
            max_download_bytes: 8,
            ..FirmwareDownloadConfig::default()
        },
        carbide_instrument::Instrumentation::from_meter_provider(
            &opentelemetry::metrics::noop::NoopMeterProvider::new(),
        ),
    )
    .unwrap();
    let mut owners = JoinSet::new();
    let cache = FirmwareArtifactCache::new(
        root.to_owned(),
        capacity,
        downloader,
        CancellationToken::new(),
        &mut owners,
    )
    .unwrap();
    CacheFixture { cache, owners }
}

async fn within<T>(phase: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect(phase)
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cache lifecycle did not settle");
}

async fn ready(
    cache: &FirmwareArtifactCache,
    source: &Url,
    digest: ArtifactDigest,
    retained: &HashSet<ArtifactDigest>,
) -> CachedArtifact {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match cache.open(source, digest, retained).await.unwrap() {
                CacheLookup::Ready(reader) => return reader,
                CacheLookup::Pending => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("artifact did not become ready")
}

#[test]
fn digest_parsing_enforces_sha256_identity() {
    check_cases(
        [
            Case {
                scenario: "lowercase identity",
                input: "ab".repeat(32),
                expect: Yields("ab".repeat(32)),
            },
            Case {
                scenario: "uppercase spelling",
                input: "AB".repeat(32),
                expect: Yields("ab".repeat(32)),
            },
            Case {
                scenario: "short digest",
                input: "ab".repeat(31),
                expect: Fails,
            },
            Case {
                scenario: "overlong digest",
                input: "ab".repeat(33),
                expect: Fails,
            },
            Case {
                scenario: "nonhex digest",
                input: "gg".repeat(32),
                expect: Fails,
            },
            Case {
                scenario: "whitespace is not identity",
                input: format!(" {}", "ab".repeat(32)),
                expect: Fails,
            },
        ],
        |input| {
            input
                .parse::<ArtifactDigest>()
                .map(|digest| digest.to_string())
                .map_err(drop)
        },
    );
}

#[tokio::test]
async fn construction_and_invalid_sources_do_not_touch_storage() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("unused");
    for (scenario, capacity) in [
        ("zero capacity", 0),
        ("below maximum artifact size", 7),
        ("above hard capacity ceiling", MAX_CAPACITY_BYTES + 1),
    ] {
        let downloader = FirmwareDownloader::with_config(
            FirmwareDownloadConfig {
                max_download_bytes: 8,
                ..FirmwareDownloadConfig::default()
            },
            carbide_instrument::Instrumentation::from_meter_provider(
                &opentelemetry::metrics::noop::NoopMeterProvider::new(),
            ),
        )
        .unwrap();
        let mut owners = JoinSet::new();
        assert!(
            matches!(
                FirmwareArtifactCache::new(
                    root.clone(),
                    capacity,
                    downloader,
                    CancellationToken::new(),
                    &mut owners
                ),
                Err(CacheError::InvalidCapacity)
            ),
            "{scenario}"
        );
    }
    FirmwareArtifactCache::validate_capacity(MAX_CAPACITY_BYTES, 8)
        .expect("inclusive hard capacity ceiling is accepted");
    let cache = cache(&root, 8, 1);
    for (scenario, source) in [
        ("local file", "file:///firmware"),
        ("user credentials", "https://user@example.test/fw"),
        (
            "password-only credentials",
            "https://:secret@example.test/fw",
        ),
        ("query", "https://example.test/fw?token=secret"),
        ("fragment", "https://example.test/fw#fragment"),
    ] {
        let source = Url::parse(source).unwrap();
        assert!(
            matches!(
                cache.open(&source, digest(b"data"), &HashSet::new()).await,
                Err(CacheError::InvalidSource)
            ),
            "{scenario}"
        );
    }
    assert!(!root.exists());
    cache.finish().await;
}

#[tokio::test]
async fn repeated_reader_errors_do_not_poll_a_completed_worker() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 1);
    let wanted = digest(b"data");
    fs::create_dir_all(&cache.inner.directory).unwrap();
    fs::write(cache.inner.path(wanted), b"data").unwrap();
    let source = Url::parse("http://unused.test/firmware").unwrap();
    let mut reader = ready(&cache, &source, wanted, &HashSet::new()).await;
    // A write-only descriptor deterministically fails reads, including a second
    // call after the completed worker has returned its first I/O error.
    reader.file = Arc::new(Mutex::new(
        File::options()
            .write(true)
            .open(cache.inner.path(wanted))
            .unwrap(),
    ));
    for _ in 0..2 {
        assert!(reader.read(&mut [0; 1]).await.is_err());
    }
    cache.finish().await;
}

// The retention owner is released after eviction or reader acquisition, before any
// download. A failed release must discard either kind of successful open.
#[tokio::test]
async fn retention_release_precedes_transfer_and_failure_releases_cache_ownership() {
    for (scenario, warm, release_succeeds) in [
        ("download starts after release", false, true),
        ("failed release settles reservation", false, false),
        ("failed release drops reader", true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 1);
        let image = digest(b"data");
        fs::create_dir_all(&cache.inner.directory).unwrap();
        if warm {
            fs::write(cache.inner.path(image), b"data").unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}/firmware", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let retained = HashSet::from([image]);
        let (entered_tx, entered) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let mut opening =
            Box::pin(
                cache.open_with_retention_lock(&source, image, &retained, async move {
                    entered_tx.send(()).unwrap();
                    released.await.unwrap();
                    if release_succeeds {
                        Ok(())
                    } else {
                        Err(CacheError::RetentionLockRelease)
                    }
                }),
            );
        assert!(futures_util::poll!(&mut opening).is_pending());
        within("physical open reaches release", entered)
            .await
            .unwrap();
        assert!(futures_util::poll!(&mut opening).is_pending(), "{scenario}");
        assert_eq!(
            cache.inner.state.lock().unwrap().entries[&image]
                .readers
                .load(Ordering::Acquire),
            usize::from(warm),
            "{scenario}"
        );
        {
            let state = cache.inner.state.lock().unwrap();
            assert_eq!(state.entries[&image].downloading, !warm, "{scenario}");
            assert_eq!(
                state.entries[&image].charged_bytes,
                if warm { 4 } else { 8 },
                "{scenario}"
            );
        }
        let mut accepted = Box::pin(listener.accept());
        assert!(
            futures_util::poll!(&mut accepted).is_pending(),
            "no source access before release"
        );

        release.send(()).unwrap();
        let result = within("open settles after release", opening).await;
        if release_succeeds {
            assert!(matches!(result, Ok(CacheLookup::Pending)), "{scenario}");
        } else {
            assert!(
                matches!(result, Err(CacheError::RetentionLockRelease)),
                "{scenario}"
            );
            assert!(
                futures_util::poll!(&mut accepted).is_pending(),
                "failed release starts no transfer"
            );
            {
                let state = cache.inner.state.lock().unwrap();
                assert_eq!(
                    state.entries[&image].readers.load(Ordering::Acquire),
                    0,
                    "{scenario}"
                );
                assert!(!state.entries[&image].downloading, "{scenario}");
                assert!(
                    state.entries[&image].failure.is_none(),
                    "{scenario}: no source transfer failed"
                );
                assert_eq!(
                    state.entries[&image].charged_bytes,
                    if warm { 4 } else { 0 },
                    "{scenario}"
                );
            }
            assert!(
                cache.inner.downloader.try_capacity().is_some(),
                "{scenario}"
            );
            // Actual reuse protects against leaked reader permits or reservations
            // that would not be visible in the flags alone.
            let retried = within(
                "open after failed release",
                cache.open(&source, image, &retained),
            )
            .await
            .unwrap();
            assert!(
                matches!(retried, CacheLookup::Ready(_)) == warm,
                "{scenario}"
            );
            drop(retried);
        }
        if !warm {
            let (mut stream, _) = within("transfer reaches origin", accepted).await.unwrap();
            // Bound the entire fixture exchange, including a stalled peer after
            // connect, and finish transfer before shutting down its owner.
            within("complete origin exchange", async {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata")
                    .await
                    .unwrap();
            })
            .await;
            drop(stream);
            drop(ready(&cache, &source, image, &retained).await);
        }
        cache.finish().await;
    }
}

// Early source rejection polls release, while losing registration to shutdown
// drops the unpolled owner. Neither path may touch storage or retain capacity.
#[tokio::test]
async fn retention_owner_cleans_up_on_early_and_registration_rejection() {
    for (scenario, closed) in [
        ("invalid source polls the release before returning", false),
        ("closed registry drops release without polling", true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 1);
        if closed {
            // Model registration losing the race with the shutdown owner after
            // the caller's earlier cancellation check already passed.
            cache.inner.work.state.lock().unwrap().closed = true;
        }
        let observed = Arc::new(AtomicBool::new(false));
        let owner = observed.clone();
        let source = if closed {
            "http://127.0.0.1:1/unused"
        } else {
            "file:///invalid-source"
        }
        .parse()
        .unwrap();
        let result = cache
            .open_with_retention_lock(&source, digest(b"data"), &HashSet::new(), async move {
                owner.store(true, Ordering::Release);
                Err(CacheError::RetentionLockRelease)
            })
            .await;
        if closed {
            assert!(matches!(result, Err(CacheError::Cancelled)), "{scenario}");
        } else {
            assert!(
                matches!(result, Err(CacheError::RetentionLockRelease)),
                "{scenario}"
            );
        }
        assert_eq!(observed.load(Ordering::Acquire), !closed, "{scenario}");
        assert_eq!(
            Arc::strong_count(&observed),
            1,
            "{scenario}: release owner dropped"
        );
        assert!(!cache.inner.directory.exists(), "{scenario}");
        assert!(
            cache.inner.downloader.try_capacity().is_some(),
            "{scenario}"
        );
        cache.finish().await;
    }
}

// A consumed source failure must still be reported once if releasing exclusion
// also fails. The release callback runs, but cannot replace that prior error.
#[tokio::test]
async fn stored_download_failure_survives_a_simultaneous_release_failure() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 1);
    let image = digest(b"data");
    let retained = HashSet::from([image]);
    assert!(matches!(
        cache.inner.clone().open(image, &retained),
        Ok(OpenResult::Download)
    ));
    cache
        .inner
        .settle(image, Some("prior transfer failed".into()));
    let called = Arc::new(AtomicBool::new(false));
    let observed = called.clone();
    let result = cache
        .open_with_retention_lock(
            &"http://127.0.0.1:1/unused".parse().unwrap(),
            image,
            &retained,
            async move {
                observed.store(true, Ordering::Release);
                Err(CacheError::RetentionLockRelease)
            },
        )
        .await;
    assert!(matches!(result, Err(CacheError::Download(error)) if error == "prior transfer failed"));
    assert!(called.load(Ordering::Acquire));
    assert!(
        cache.inner.state.lock().unwrap().entries[&image]
            .failure
            .is_none()
    );
    cache.finish().await;
}

// Dropping the caller future must not drop the retention owner while physical open
// is queued. The one-thread blocking pool gives an explicit, bounded gate.
#[test]
fn cancelled_open_keeps_retention_until_its_physical_worker_finishes() {
    struct RetentionOwner(Arc<AtomicBool>);
    impl Drop for RetentionOwner {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 1);
        let image = digest(b"data");
        fs::create_dir_all(&cache.inner.directory).unwrap();
        fs::write(cache.inner.path(image), b"data").unwrap();
        let (started_tx, started) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("release blocking gate");
        });
        within("blocking gate starts", started).await.unwrap();

        let dropped = Arc::new(AtomicBool::new(false));
        let owner = RetentionOwner(dropped.clone());
        let (finished_tx, finished) = oneshot::channel();
        let inner = cache.inner.clone();
        let source = "http://127.0.0.1:1/unused".parse().unwrap();
        let retained = HashSet::from([image]);
        let mut opening =
            Box::pin(
                cache.open_with_retention_lock(&source, image, &retained, async move {
                    assert_eq!(
                        inner.state.lock().unwrap().entries[&image]
                            .readers
                            .load(Ordering::Acquire),
                        1,
                        "physical reader acquired before retention ends"
                    );
                    drop(owner);
                    finished_tx.send(()).unwrap();
                    Ok(())
                }),
            );
        assert!(futures_util::poll!(&mut opening).is_pending());
        // Register and poll the cache task while its physical open cannot run.
        tokio::task::yield_now().await;
        drop(opening);
        assert!(!dropped.load(Ordering::Acquire));
        assert!(cache.inner.downloader.try_capacity().is_none());

        release.send(()).unwrap();
        blocker.await.unwrap();
        within("physical open releases retention", finished)
            .await
            .unwrap();
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(fs::read(cache.inner.path(image)).unwrap(), b"data");
        cache.finish().await;
    });
}

// Two URLs naming the same bytes must share one verified file. Later eviction
// must stay inside the managed namespace, even with a legacy cache next to it.
#[tokio::test]
async fn digest_shares_origins_and_eviction_preserves_legacy_files() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory
        .path()
        .join("legacy-url-hash")
        .join("firmware.bin");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, b"legacy").unwrap();
    let mut first = mockito::Server::new_async().await;
    let response = first
        .mock("GET", "/firmware")
        .with_body("data")
        .expect(2)
        .create_async()
        .await;
    let mut second = mockito::Server::new_async().await;
    let unexpected = second
        .mock("GET", "/same-digest")
        .expect(0)
        .create_async()
        .await;
    let replacement = second
        .mock("GET", "/replacement")
        .with_body("next")
        .expect(1)
        .create_async()
        .await;
    let cache = cache(directory.path(), 8, 2);
    let first_digest = digest(b"data");
    let source = Url::parse(&format!("{}/firmware", first.url())).unwrap();
    drop(ready(&cache, &source, first_digest, &HashSet::new()).await);
    let alternate = Url::parse(&format!("{}/same-digest", second.url())).unwrap();
    let mut reader = ready(&cache, &alternate, first_digest, &HashSet::new()).await;
    assert_eq!(reader.content_length(), 4);
    let mut contents = Vec::new();
    reader.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"data");
    drop(reader);
    // Verification must repeat after this same cache instance has served the
    // file; remembering its previous success would expose these damaged bytes.
    fs::write(cache.inner.path(first_digest), b"bad").unwrap();
    let mut repaired = ready(&cache, &source, first_digest, &HashSet::new()).await;
    let mut contents = Vec::new();
    repaired.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"data");
    drop(repaired);
    let source = Url::parse(&format!("{}/replacement", second.url())).unwrap();
    drop(ready(&cache, &source, digest(b"next"), &HashSet::new()).await);
    assert!(!cache.inner.path(first_digest).exists());
    assert_eq!(fs::read(legacy).unwrap(), b"legacy");
    response.assert_async().await;
    unexpected.assert_async().await;
    replacement.assert_async().await;
    cache.finish().await;
}

// Retention and a live reader independently exclude eviction. Releasing both
// must allow another artifact without ever contacting its source while full.
#[tokio::test]
async fn readers_and_current_retention_prevent_eviction_before_source_access() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let first = server
        .mock("GET", "/first")
        .with_body("data")
        .create_async()
        .await;
    let second = server
        .mock("GET", "/second")
        .with_body("next")
        .expect(1)
        .create_async()
        .await;
    let cache = cache(directory.path(), 8, 2);
    let first_digest = digest(b"data");
    let source = Url::parse(&format!("{}/first", server.url())).unwrap();
    let reader = ready(&cache, &source, first_digest, &HashSet::new()).await;
    let next = Url::parse(&format!("{}/second", server.url())).unwrap();
    assert!(matches!(
        cache.open(&next, digest(b"next"), &HashSet::new()).await,
        Err(CacheError::Capacity)
    ));
    assert!(!second.matched_async().await);
    drop(reader);
    let retained = HashSet::from([first_digest]);
    assert!(matches!(
        cache.open(&next, digest(b"next"), &retained).await,
        Err(CacheError::Capacity)
    ));
    assert!(!second.matched_async().await);
    // Retention is supplied for this call, so releasing it permits immediate
    // reclamation without waiting for a background policy snapshot.
    drop(ready(&cache, &next, digest(b"next"), &HashSet::new()).await);
    assert!(!cache.inner.path(first_digest).exists());
    first.assert_async().await;
    second.assert_async().await;
    cache.finish().await;
}

// Hold an HTTP body open after its first bytes. Completion or cancellation must
// settle physical accounting without relying on a subsequent cache request.
#[tokio::test]
async fn physical_completion_and_cancellation_settle_reservations_without_polling() {
    let _metrics = MetricsCapture::start();
    for (scenario, complete) in [("completed transfer", true), ("cancelled transfer", false)] {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 2);
        let image = digest(b"data");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = Url::parse(&format!(
            "http://{}/firmware",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nda")
                .await
                .unwrap();
            headers_tx.send(()).unwrap();
            if finish_rx.await.unwrap() {
                stream.write_all(b"ta").await.unwrap();
            }
        });
        assert!(matches!(
            cache.open(&source, image, &HashSet::new()).await.unwrap(),
            CacheLookup::Pending
        ));
        tokio::time::timeout(Duration::from_secs(5), headers_rx)
            .await
            .expect("origin did not receive the transfer")
            .unwrap();
        let partial = partial_path(&cache.inner.path(image));
        until(|| fs::metadata(&partial).is_ok_and(|metadata| metadata.len() == 2)).await;
        assert_eq!(
            cache.inner.state.lock().unwrap().entries[&image].charged_bytes,
            8
        );
        assert!(matches!(
            cache.open(&source, digest(b"next"), &HashSet::new()).await,
            Err(CacheError::Capacity)
        ));
        if complete {
            finish_tx.send(true).unwrap();
            until(|| !cache.inner.state.lock().unwrap().entries[&image].downloading).await;
        } else {
            // Keep the peer open until cancellation settles, so EOF cannot
            // explain why the physical reservation and partial were released.
            cache.inner.cancel.cancel();
            until(|| !cache.inner.state.lock().unwrap().entries[&image].downloading).await;
            finish_tx.send(false).unwrap();
        }
        server.await.unwrap();
        {
            let state = cache.inner.state.lock().unwrap();
            let entry = &state.entries[&image];
            assert_eq!(
                entry.charged_bytes,
                if complete { 4 } else { 0 },
                "{scenario}"
            );
            assert_eq!(entry.failure.is_none(), complete, "{scenario}");
            assert!(!partial.exists());
            assert_eq!(cache.inner.path(image).exists(), complete);
        }
        cache.finish().await;
    }
}

// Reopen a damaged namespace, then fetch on a second empty replica. Both must
// verify owned files and preserve unrelated files and symlink targets.
#[tokio::test]
async fn restart_removes_owned_partials_refills_corruption_and_preserves_unknown_entries() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let namespace = directory.path().join(NAMESPACE);
    fs::create_dir(&namespace).unwrap();
    let image = digest(b"data");
    fs::write(namespace.join(image.to_string()), b"bad").unwrap();
    let orphan = namespace.join(format!("{}.download", digest(b"orphan")));
    fs::write(&orphan, b"orphan").unwrap();
    let unknown = namespace.join("operator-note");
    fs::write(&unknown, b"keep").unwrap();
    // Hex case is accepted at the API boundary, but only lowercase cache
    // filenames belong to the cache. Preserve both owned-looking spellings.
    let uppercase = digest(b"operator-owned").to_string().to_uppercase();
    let unowned_names = [
        namespace.join(&uppercase),
        namespace.join(format!("{uppercase}.download")),
    ];
    for path in &unowned_names {
        fs::write(path, b"keep-case").unwrap();
    }
    let external = directory.path().join("external");
    fs::write(&external, b"keep-external").unwrap();
    let symlink = namespace.join(digest(b"external").to_string());
    std::os::unix::fs::symlink(&external, &symlink).unwrap();
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/firmware")
        .with_body("data")
        .expect(2)
        .create_async()
        .await;
    let source = Url::parse(&format!("{}/firmware", server.url())).unwrap();
    let first = cache(directory.path(), 8, 2);
    drop(ready(&first, &source, image, &HashSet::new()).await);
    assert!(!orphan.exists());
    first.finish().await;
    let restarted = cache(directory.path(), 8, 2);
    drop(ready(&restarted, &source, image, &HashSet::new()).await);
    assert_eq!(fs::read(&unknown).unwrap(), b"keep");
    for path in &unowned_names {
        assert_eq!(fs::read(path).unwrap(), b"keep-case");
    }
    assert!(
        fs::symlink_metadata(symlink)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(external).unwrap(), b"keep-external");
    let cold_root = tempfile::tempdir().unwrap();
    let cold = cache(cold_root.path(), 8, 2);
    drop(ready(&cold, &source, image, &HashSet::new()).await);
    response.assert_async().await;
    restarted.finish().await;
    cold.finish().await;
}

// Managed origins may not redirect the machine's approved source. The retained
// URL downloader must still follow its existing redirect policy.
#[tokio::test]
async fn managed_redirects_are_rejected_and_legacy_redirects_still_work() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let mut server = mockito::Server::new_async().await;
    let redirect = server
        .mock("GET", "/redirect")
        .with_status(302)
        .with_header("location", "/destination")
        .expect(2)
        .create_async()
        .await;
    let destination = server
        .mock("GET", "/destination")
        .with_body("data")
        .expect(1)
        .create_async()
        .await;
    let retry = server
        .mock("GET", "/retry")
        .with_body("data")
        .expect(1)
        .create_async()
        .await;
    let cache = cache(directory.path(), 8, 2);
    let source = Url::parse(&format!("{}/redirect", server.url())).unwrap();
    let image = digest(b"data");
    assert!(matches!(
        cache.open(&source, image, &HashSet::new()).await.unwrap(),
        CacheLookup::Pending
    ));
    until(|| !cache.inner.state.lock().unwrap().entries[&image].downloading).await;
    assert!(
        matches!(cache.open(&source, image, &HashSet::new()).await, Err(CacheError::Download(message)) if message.contains("unsuccessful HTTP status"))
    );
    assert!(!destination.matched_async().await);
    let retry_source = Url::parse(&format!("{}/retry", server.url())).unwrap();
    drop(ready(&cache, &retry_source, image, &HashSet::new()).await);
    let legacy = directory.path().join("legacy").join("firmware");
    assert_eq!(
        cache
            .inner
            .downloader
            .available(&legacy, source.as_str(), &image.to_string())
            .await,
        ArtifactStatus::Downloading
    );
    until(|| legacy.exists()).await;
    until(|| {
        let permits: Vec<_> = (0..2)
            .filter_map(|_| cache.inner.downloader.try_capacity())
            .collect();
        permits.len() == 2
    })
    .await;
    assert_eq!(fs::read(legacy).unwrap(), b"data");
    redirect.assert_async().await;
    destination.assert_async().await;
    retry.assert_async().await;
    cache.finish().await;
}

#[tokio::test]
async fn metadata_obstruction_keeps_the_reservation_charged() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 2);
    fs::create_dir_all(&cache.inner.directory).unwrap();
    let image = digest(b"data");
    // A nonregular staging path prevents safe metadata/cleanup. Settlement
    // must preserve its charge rather than assume the path contains no bytes.
    let partial = partial_path(&cache.inner.path(image));
    fs::create_dir(&partial).unwrap();
    fs::write(partial.join("keep"), b"keep").unwrap();
    {
        let mut state = cache.inner.state.lock().unwrap();
        state.initialized = true;
        state.entries.insert(
            image,
            Entry {
                charged_bytes: 8,
                downloading: true,
                ..Entry::default()
            },
        );
    }
    let inner = cache.inner.clone();
    blocking(move || inner.settle(image, Some("cancelled".to_owned()))).await;
    let source = Url::parse("http://127.0.0.1:1/unused").unwrap();
    assert!(matches!(
        cache
            .open(&source, digest(b"next"), &HashSet::from([image]))
            .await,
        Err(CacheError::Capacity)
    ));
    {
        let state = cache.inner.state.lock().unwrap();
        assert_eq!(state.entries[&image].charged_bytes, 8);
        assert!(!state.entries[&image].downloading);
        assert!(
            state.entries[&image]
                .failure
                .as_ref()
                .unwrap()
                .contains("not a regular file")
        );
        assert!(
            state.entries[&image]
                .failure
                .as_ref()
                .unwrap()
                .starts_with("cancelled; ")
        );
        assert_eq!(fs::read(partial.join("keep")).unwrap(), b"keep");
    }
    cache.finish().await;
}

// Queue a read behind a gated blocking task, then drop the response. Its file
// and reader admission must remain protected until the queued worker finishes.
#[test]
fn dropping_a_reader_keeps_its_lease_until_a_queued_physical_read_finishes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 1);
        fs::create_dir_all(&cache.inner.directory).unwrap();
        let image = digest(b"data");
        fs::write(cache.inner.path(image), b"data").unwrap();
        let source = Url::parse("http://127.0.0.1:1/unused").unwrap();
        let mut reader = ready(&cache, &source, image, &HashSet::new()).await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).ok();
            release_rx.recv().ok();
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("blocking gate did not start")
            .unwrap();
        let mut contents = [0; 4];
        let mut read = Box::pin(reader.read(&mut contents));
        assert!(futures_util::poll!(&mut read).is_pending());
        drop(read);
        drop(reader);
        {
            let mut state = cache.inner.state.lock().unwrap();
            assert_eq!(state.entries[&image].readers.load(Ordering::Acquire), 1);
            assert_eq!(cache.inner.readers.available_permits(), MAX_READERS - 1);
            assert!(matches!(
                cache
                    .inner
                    .ensure_space(&mut state, 8, &HashSet::new(), digest(b"next")),
                Err(CacheError::Capacity)
            ));
        }
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        until(|| {
            cache.inner.state.lock().unwrap().entries[&image]
                .readers
                .load(Ordering::Acquire)
                == 0
                && cache.inner.readers.available_permits() == MAX_READERS
        })
        .await;
        assert_eq!(cache.inner.readers.available_permits(), MAX_READERS);
        cache.finish().await;
    });
}

// Fill reader admission while the shared downloader has only one worker. A
// slow response must not prevent legacy downloads from using that worker.
#[tokio::test]
async fn ready_readers_do_not_starve_the_legacy_download_pool() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 1);
    fs::create_dir_all(&cache.inner.directory).unwrap();
    let image = digest(b"data");
    fs::write(cache.inner.path(image), b"data").unwrap();
    let unused = Url::parse("http://127.0.0.1:1/unused").unwrap();
    let mut readers = Vec::new();
    for _ in 0..MAX_READERS {
        readers.push(ready(&cache, &unused, image, &HashSet::new()).await);
    }
    assert!(matches!(
        cache.open(&unused, image, &HashSet::new()).await.unwrap(),
        CacheLookup::Pending
    ));
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/legacy")
        .with_body("next")
        .create_async()
        .await;
    let filename = directory.path().join("legacy").join("firmware");
    let source = format!("{}/legacy", server.url());
    assert_eq!(
        cache
            .inner
            .downloader
            .available(&filename, &source, "")
            .await,
        ArtifactStatus::Downloading
    );
    until(|| filename.exists()).await;
    until(|| cache.inner.downloader.try_capacity().is_some()).await;
    assert_eq!(fs::read(filename).unwrap(), b"next");
    let mut reader = readers.pop().unwrap();
    let mut contents = Vec::new();
    reader.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"data");
    drop(reader);
    drop(ready(&cache, &unused, image, &HashSet::new()).await);
    response.assert_async().await;
    cache.finish().await;
}

#[tokio::test]
async fn unlinked_content_stays_charged_until_its_last_reader_drops() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 2);
    let image = digest(b"data");
    fs::create_dir_all(&cache.inner.directory).unwrap();
    fs::write(cache.inner.path(image), b"data").unwrap();
    let mut server = mockito::Server::new_async().await;
    let refill = server
        .mock("GET", "/firmware")
        .with_body("data")
        .expect(1)
        .create_async()
        .await;
    let source = Url::parse(&format!("{}/firmware", server.url())).unwrap();
    let mut reader = ready(&cache, &source, image, &HashSet::new()).await;
    fs::remove_file(cache.inner.path(image)).unwrap();
    assert!(matches!(
        cache.open(&source, image, &HashSet::new()).await.unwrap(),
        CacheLookup::Pending
    ));
    {
        let state = cache.inner.state.lock().unwrap();
        assert_eq!(state.entries[&image].charged_bytes, 4);
        assert_eq!(state.entries[&image].readers.load(Ordering::Acquire), 1);
    }
    assert!(matches!(
        cache.open(&source, digest(b"next"), &HashSet::new()).await,
        Err(CacheError::Capacity)
    ));
    assert!(!refill.matched_async().await);
    let mut contents = Vec::new();
    reader.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"data");
    drop(reader);
    drop(ready(&cache, &source, image, &HashSet::new()).await);
    refill.assert_async().await;
    cache.finish().await;
}

#[tokio::test]
async fn over_budget_restart_content_streams_multiple_chunks_and_accounts_other_digests() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 1);
    fs::create_dir_all(&cache.inner.directory).unwrap();
    let contents: Vec<_> = (0..(2 * 64 * 1024 + 19))
        .map(|offset| (offset % 251) as u8)
        .collect();
    let image = digest(&contents);
    let other = digest(b"retained");
    fs::write(cache.inner.path(image), &contents).unwrap();
    fs::write(cache.inner.path(other), b"retained").unwrap();
    let source = Url::parse("http://unused.test/firmware").unwrap();
    let retained = HashSet::from([image, other]);
    let mut reader = ready(&cache, &source, image, &retained).await;
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, contents);
    assert_eq!(reader.content_length(), contents.len() as u64);
    assert_eq!(
        cache.inner.state.lock().unwrap().entries[&other].charged_bytes,
        8
    );
    assert!(matches!(
        cache.open(&source, digest(b"next"), &retained).await,
        Err(CacheError::Capacity)
    ));
    assert_eq!(fs::read(cache.inner.path(other)).unwrap(), b"retained");
    drop(reader);
    cache.finish().await;
}

#[tokio::test]
async fn eviction_skips_an_obstructed_candidate_and_prunes_stale_empty_entries() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 16, 1);
    fs::create_dir_all(&cache.inner.directory).unwrap();
    let obstructed = digest(b"blocked");
    let evictable = digest(b"evict me");
    let empty = digest(b"stale failure");
    fs::create_dir(cache.inner.path(obstructed)).unwrap();
    fs::write(cache.inner.path(evictable), b"evict me").unwrap();
    {
        let mut state = cache.inner.state.lock().unwrap();
        state.initialized = true;
        state.entries.insert(
            obstructed,
            Entry {
                charged_bytes: 4,
                ..Entry::default()
            },
        );
        state.entries.insert(
            evictable,
            Entry {
                charged_bytes: 8,
                last_use: 1,
                ..Entry::default()
            },
        );
        state.entries.insert(
            empty,
            Entry {
                failure: Some("old failure".to_owned()),
                ..Entry::default()
            },
        );
        cache
            .inner
            .ensure_space(&mut state, 8, &HashSet::new(), digest(b"incoming"))
            .unwrap();
        state.prune(&HashSet::new());
        assert_eq!(state.entries[&obstructed].charged_bytes, 4);
        assert!(!state.entries.contains_key(&evictable));
        assert!(!state.entries.contains_key(&empty));
    }
    assert!(cache.inner.path(obstructed).is_dir());
    assert!(!cache.inner.path(evictable).exists());
    cache.finish().await;
}

#[tokio::test]
async fn cold_concurrent_origins_share_one_physical_digest_download() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = Url::parse(&format!("http://{}/first", listener.local_addr().unwrap())).unwrap();
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        started_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata")
            .await
            .unwrap();
    });
    let mut alternate = mockito::Server::new_async().await;
    let unexpected = alternate
        .mock("GET", "/second")
        .expect(0)
        .create_async()
        .await;
    let alternate_source = Url::parse(&format!("{}/second", alternate.url())).unwrap();
    let image = digest(b"data");
    assert!(matches!(
        cache.open(&source, image, &HashSet::new()).await.unwrap(),
        CacheLookup::Pending
    ));
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        cache
            .open(&alternate_source, image, &HashSet::new())
            .await
            .unwrap(),
        CacheLookup::Pending
    ));
    assert!(!cache.inner.path(image).exists());
    finish_tx.send(()).unwrap();
    within("origin task did not finish", server).await.unwrap();
    drop(ready(&cache, &alternate_source, image, &HashSet::new()).await);
    unexpected.assert_async().await;
    cache.finish().await;
}

#[test]
fn owner_shutdown_joins_a_cancelled_queued_managed_write_before_settlement() {
    let _metrics = MetricsCapture::start();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    for (scenario, panic_write) in [
        ("cancelled write finishes", false),
        ("cancelled write panics after its await stops", true),
    ] {
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let mut cache = cache(directory.path(), 8, 1);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let source = Url::parse(&format!(
                "http://{}/firmware",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let (finish_tx, finish_rx) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nda")
                    .await
                    .unwrap();
                finish_rx.await.unwrap();
                stream.write_all(b"ta").await.unwrap();
            });
            let image = digest(b"data");
            assert!(matches!(
                cache.open(&source, image, &HashSet::new()).await.unwrap(),
                CacheLookup::Pending
            ));
            let partial = partial_path(&cache.inner.path(image));
            until(|| fs::metadata(&partial).is_ok_and(|metadata| metadata.len() == 2)).await;
            within(
                "first write was not queued",
                cache.inner.write_queued.notified(),
            )
            .await;
            let (started_tx, started_rx) = oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started_tx.send(()).ok();
                release_rx.recv().ok();
            });
            within("blocking gate did not start", started_rx)
                .await
                .unwrap();
            finish_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), cache.inner.write_queued.notified())
                .await
                .unwrap();
            cache.inner.cancel.cancel();
            until(|| cache.inner.work.state.lock().unwrap().closed).await;
            within(
                "managed transfer await did not stop",
                cache.inner.transfer_stopped.notified(),
            )
            .await;
            if panic_write {
                cache
                    .inner
                    .panic_before_write
                    .store(true, Ordering::Release);
            }
            assert!(futures_util::poll!(std::pin::pin!(cache.owners.join_next())).is_pending());
            assert!(cache.inner.downloader.try_capacity().is_none());
            {
                let state = cache.inner.state.lock().unwrap();
                assert!(state.entries[&image].downloading);
                assert_eq!(state.entries[&image].charged_bytes, 8);
            }
            assert!(matches!(
                cache.open(&source, image, &HashSet::new()).await,
                Err(CacheError::Cancelled)
            ));
            release_tx.send(()).unwrap();
            within("blocking gate did not finish", blocker)
                .await
                .unwrap();
            within("origin task did not finish", server).await.unwrap();
            let inner = cache.inner.clone();
            let outcome = within("cache owner did not drain", cache.owners.join_next())
                .await
                .expect("registered cache owner");
            match (panic_write, outcome) {
                (false, Ok(())) => {}
                (true, Err(error))
                    if error.is_panic()
                        && error
                            .to_string()
                            .contains("managed firmware physical worker panicked") => {}
                (_, outcome) => panic!("{scenario}: unexpected owner result {outcome:?}"),
            }
            assert!(cache.owners.is_empty(), "{scenario}");
            cache.finish().await;
            assert_eq!(inner.state.lock().unwrap().entries[&image].charged_bytes, 0);
            assert!(!partial.exists());
            assert!(inner.downloader.try_capacity().is_some());
        });
    }
}

#[test]
fn a_lost_request_does_not_hide_its_blocking_worker_panic_from_the_owner() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = cache(directory.path(), 8, 1);
        // Poisoning this lock supplies a deterministic panic in the public
        // open worker, after its request has abandoned the reply channel.
        let inner = cache.inner.clone();
        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _state = inner.state.lock().unwrap();
                panic!("injected cache state failure");
            }))
            .is_err()
        );
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).ok();
            release_rx.recv().ok();
        });
        within("blocking gate did not start", started_rx)
            .await
            .unwrap();
        let source = Url::parse("http://unused.test/firmware").unwrap();
        let retained = HashSet::new();
        let mut opening = Box::pin(cache.open(&source, digest(b"data"), &retained));
        assert!(futures_util::poll!(&mut opening).is_pending());
        drop(opening);
        release_tx.send(()).unwrap();
        within("blocking gate did not finish", blocker)
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), cache.owners.join_next())
            .await
            .unwrap();
        assert!(matches!(outcome, Some(Err(error)) if error.is_panic()));
        assert!(cache.owners.is_empty());
        assert!(cache.inner.cancel.is_cancelled());
        assert!(matches!(
            cache.open(&source, digest(b"data"), &retained).await,
            Err(CacheError::Cancelled)
        ));
    });
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);
impl io::Write for CapturedLogs {
    fn write(&mut self, contents: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(contents);
        Ok(contents.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn managed_failures_keep_the_request_span_without_recording_source_path_secrets() {
    let _metrics = MetricsCapture::start();
    carbide_instrument::testing::keep_callsites_enabled();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 8, 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = Url::parse(&format!(
        "http://{}/signed-secret-token/firmware",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        // Close before headers so middleware must record a transport failure.
    });
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(move || writer.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let image = digest(b"data");
    async {
        async {
            assert!(matches!(
                cache
                    .open(&source, image, &HashSet::from([image]))
                    .await
                    .unwrap(),
                CacheLookup::Pending
            ));
        }
        .instrument(tracing::info_span!("cache_request"))
        .await;
    }
    .with_subscriber(dispatch)
    .await;
    within("origin task did not finish", server).await.unwrap();
    until(|| !cache.inner.state.lock().unwrap().entries[&image].downloading).await;
    let diagnostic = cache.inner.state.lock().unwrap().entries[&image]
        .failure
        .clone()
        .unwrap();
    assert_eq!(diagnostic, "firmware source connection failed");
    cache.finish().await;
    let recorded = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        recorded
            .lines()
            .any(|line| line.contains("cache_request")
                && line.contains("firmware_download_finished")),
        "{recorded}"
    );
    assert!(
        recorded.contains("managed firmware source request failed"),
        "{recorded}"
    );
    assert!(!recorded.contains("signed-secret-token"), "{recorded}");
}

// Both mutations happen after adoption. An unrelated reservation must use
// observable bytes even when that digest is never opened again.
#[tokio::test]
async fn unrelated_reservations_refresh_grown_and_deleted_retained_files() {
    let _metrics = MetricsCapture::start();
    enum Change {
        GrowInactive,
        DeleteRetained,
    }
    for (scenario, change) in [
        ("grown inactive candidate", Change::GrowInactive),
        (
            "deleted retained file after its last reader",
            Change::DeleteRetained,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache(directory.path(), 8, 2);
        fs::create_dir_all(&cache.inner.directory).unwrap();
        let previous = digest(b"data");
        fs::write(cache.inner.path(previous), b"data").unwrap();
        let mut server = mockito::Server::new_async().await;
        let response = server
            .mock("GET", "/incoming")
            .with_body("next")
            .expect(1)
            .create_async()
            .await;
        let source = Url::parse(&format!("{}/incoming", server.url())).unwrap();
        let reader = ready(&cache, &source, previous, &HashSet::new()).await;
        let retained = match change {
            Change::GrowInactive => {
                drop(reader);
                fs::write(cache.inner.path(previous), b"grows").unwrap();
                HashSet::new()
            }
            Change::DeleteRetained => {
                fs::remove_file(cache.inner.path(previous)).unwrap();
                drop(reader);
                HashSet::from([previous])
            }
        };
        let mut incoming = ready(&cache, &source, digest(b"next"), &retained).await;
        let mut bytes = Vec::new();
        incoming.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"next", "{scenario}");
        assert!(!cache.inner.path(previous).exists(), "{scenario}");
        assert!(!cache.inner.cancel.is_cancelled(), "{scenario}");
        drop(incoming);
        response.assert_async().await;
        cache.finish().await;
    }
}

#[tokio::test]
async fn recreated_path_is_charged_with_its_live_old_reader_before_unrelated_reservation() {
    let _metrics = MetricsCapture::start();
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(directory.path(), 12, 2);
    fs::create_dir_all(&cache.inner.directory).unwrap();
    let previous = digest(b"data");
    fs::write(cache.inner.path(previous), b"data").unwrap();
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("GET", "/incoming")
        .with_body("incoming")
        .expect(1)
        .create_async()
        .await;
    let source = Url::parse(&format!("{}/incoming", server.url())).unwrap();
    let mut old_reader = ready(&cache, &source, previous, &HashSet::new()).await;
    fs::remove_file(cache.inner.path(previous)).unwrap();
    fs::write(cache.inner.path(previous), b"data").unwrap();
    let retained = HashSet::from([previous]);
    // Do not reopen `previous` first: admission itself must discover the
    // replacement in addition to the old reader's four occupied bytes.
    assert!(matches!(
        cache.open(&source, digest(b"incoming"), &retained).await,
        Err(CacheError::Capacity)
    ));
    assert!(!response.matched_async().await);
    assert_eq!(
        cache.inner.state.lock().unwrap().entries[&previous].charged_bytes,
        8
    );
    assert!(matches!(
        cache.open(&source, previous, &retained).await.unwrap(),
        CacheLookup::Pending
    ));
    let mut bytes = Vec::new();
    old_reader.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"data");
    drop(old_reader);
    let mut incoming = ready(&cache, &source, digest(b"incoming"), &retained).await;
    bytes.clear();
    incoming.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"incoming");
    drop(incoming);
    response.assert_async().await;
    cache.finish().await;
}
