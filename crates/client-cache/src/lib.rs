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

//! A cache of built protocol clients that rebuilds a client once the inputs it
//! was built from change.
//!
//! Fabric-manager adapters, such as the UFM REST client and the NMX-C gRPC
//! channel pool, build an expensive client from an endpoint, credentials, and
//! TLS material on disk, then share it across calls. [`ClientCache`] owns the
//! lifecycle they have in common:
//!
//! - It reuses a cached client while the caller's fingerprint of its by-value
//!   inputs, such as an endpoint and credentials, matches the one the client
//!   was built from, and no TLS material file is newer than the client.
//! - It evicts and rebuilds the client once the fingerprint differs or a TLS
//!   material file was modified after the client was built (certificate
//!   rotation).
//! - It keeps the cached client when a TLS material file's modification time
//!   cannot be read, so a file missing during an incomplete rotation does not
//!   tear down a working client.
//!
//! Adapters keep everything protocol-specific: the cache key, what the
//! fingerprint covers, client construction and its errors, and log wording,
//! which they derive from the [`TlsMaterialEvent`]s the cache reports.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;
use std::{fmt, io};

/// Built clients by key, each checked against the fingerprint and creation
/// time it was built with before it is reused.
///
/// `K` is the cache key, such as a fabric name or endpoint URI. `F` is the
/// fingerprint of the by-value inputs a client is built from, or `()` when the
/// key determines them. `C` is the client, cloned out to every caller.
pub struct ClientCache<K, F, C> {
    entries: Mutex<HashMap<K, Entry<F, C>>>,
    on_tls_material_event: fn(TlsMaterialEvent<'_>),
}

/// A built client, the fingerprint it was built from, and when its build
/// started.
struct Entry<F, C> {
    fingerprint: F,
    created: SystemTime,
    client: C,
}

/// A TLS material observation that [`ClientCache::get_or_build`] reports to
/// the cache's owner, which logs it in protocol-specific terms.
#[derive(Debug)]
pub enum TlsMaterialEvent<'a> {
    /// The file's modification time could not be read, for example because
    /// the file is missing mid-rotation, so it does not make the cached client
    /// stale.
    Unreadable {
        /// The TLS material file that was checked.
        path: &'a Path,
        /// Why its modification time could not be read.
        error: &'a io::Error,
    },
    /// The file was modified after the cached client was built, so the client
    /// is evicted and rebuilt.
    Newer {
        /// The TLS material file that was checked.
        path: &'a Path,
    },
}

impl<K, F, C> ClientCache<K, F, C> {
    /// Creates an empty cache that reports TLS material observations to
    /// `on_tls_material_event`.
    pub fn new(on_tls_material_event: fn(TlsMaterialEvent<'_>)) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            on_tls_material_event,
        }
    }

    /// Locks the entries. Each critical section is a single lookup, removal,
    /// or insertion that leaves the map consistent if it panics, so a poisoned
    /// lock is recovered instead of failing every later caller.
    fn lock(&self) -> MutexGuard<'_, HashMap<K, Entry<F, C>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<K: Eq + Hash, F: PartialEq, C: Clone> ClientCache<K, F, C> {
    /// Returns the client cached under `key`, or, when none is cached or the
    /// cached one is stale, builds one with `build`, caches it, and returns it.
    ///
    /// A cached client is stale when it was built with a different
    /// `fingerprint`, or when a file in `tls_material`, the on-disk material
    /// `build` loads, was modified after the client was built. Files are
    /// checked in order until one is newer. A file whose modification time
    /// cannot be read does not make the client stale. A future-dated
    /// modification time, such as from writer clock skew, makes every call
    /// rebuild until the clock passes it. Unreadable files and the newer file
    /// are reported to the handler passed to [`ClientCache::new`].
    ///
    /// A stale client is evicted before `build` runs, so when `build` fails,
    /// its error is returned and nothing is cached under `key`. The creation
    /// time is taken before `build` runs, so material rewritten while building
    /// counts as newer than the new client and triggers one more rebuild
    /// instead of being missed.
    ///
    /// The cache lock is held only to look up, evict, and insert, never across
    /// the modification-time reads or `build`. Concurrent calls that miss the
    /// same key may each build; the last insert wins.
    pub async fn get_or_build<Q, P, E, Fut>(
        &self,
        key: &Q,
        fingerprint: F,
        tls_material: &[P],
        build: impl FnOnce() -> Fut,
    ) -> Result<C, E>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ToOwned<Owned = K> + ?Sized,
        P: AsRef<Path>,
        Fut: Future<Output = Result<C, E>>,
    {
        let cached = self
            .lock()
            .get(key)
            .filter(|entry| entry.fingerprint == fingerprint)
            .map(|entry| (entry.client.clone(), entry.created));
        if let Some((client, created)) = cached
            && !self.tls_material_newer_than(tls_material, created).await
        {
            return Ok(client);
        }

        self.lock().remove(key);
        // Before building: the build reads the material that later calls
        // compare against this time.
        let created = SystemTime::now();
        let client = build().await?;
        self.lock().insert(
            key.to_owned(),
            Entry {
                fingerprint,
                created,
                client: client.clone(),
            },
        );
        Ok(client)
    }

    /// True when a file in `tls_material` was modified after `created`.
    /// Reports each unreadable file and the newer file.
    async fn tls_material_newer_than(
        &self,
        tls_material: &[impl AsRef<Path>],
        created: SystemTime,
    ) -> bool {
        for path in tls_material {
            let path = path.as_ref();
            match tokio::fs::metadata(path)
                .await
                .and_then(|metadata| metadata.modified())
            {
                Ok(modified) if modified > created => {
                    (self.on_tls_material_event)(TlsMaterialEvent::Newer { path });
                    return true;
                }
                Ok(_) => {}
                Err(error) => (self.on_tls_material_event)(TlsMaterialEvent::Unreadable {
                    path,
                    error: &error,
                }),
            }
        }
        false
    }
}

impl<K, F, C> fmt::Debug for ClientCache<K, F, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Entries hold fingerprints of the credentials their clients were
        // built from; keep them out of debug output.
        f.debug_struct("ClientCache").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;

    const KEY: &str = "endpoint";

    /// The TLS material files a [`Harness`] writes, in the order it lists them.
    const MATERIAL: [&str; 3] = ["ca.crt", "tls.crt", "tls.key"];

    #[derive(Debug, PartialEq)]
    struct BuildFailed;

    thread_local! {
        /// TLS material events reported on this thread, as (kind, file).
        static EVENTS: RefCell<Vec<(&'static str, &'static str)>> = const { RefCell::new(Vec::new()) };
    }

    fn record_event(event: TlsMaterialEvent<'_>) {
        let (kind, path) = match event {
            TlsMaterialEvent::Unreadable { path, .. } => ("unreadable", path),
            TlsMaterialEvent::Newer { path } => ("newer", path),
        };
        let file = MATERIAL
            .into_iter()
            .find(|file| path.ends_with(file))
            .expect("a harness material file");
        EVENTS.with_borrow_mut(|events| events.push((kind, file)));
    }

    fn set_modified(path: &Path, modified: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open TLS material")
            .set_modified(modified)
            .expect("set TLS material mtime");
    }

    fn assert_send<T: Send>(_: &T) {}

    /// A cache over three TLS material files whose clients are build sequence
    /// numbers, so a test can tell a reused client from a rebuilt one.
    struct Harness {
        cache: ClientCache<String, u64, u32>,
        builds: AtomicU32,
        material_dir: tempfile::TempDir,
    }

    impl Harness {
        fn new() -> Self {
            let material_dir = tempfile::tempdir().expect("temp dir");
            for file in MATERIAL {
                std::fs::write(material_dir.path().join(file), file).expect("write TLS material");
            }
            Self {
                cache: ClientCache::new(record_event),
                builds: AtomicU32::new(0),
                material_dir,
            }
        }

        fn path(&self, file: &str) -> PathBuf {
            self.material_dir.path().join(file)
        }

        fn material(&self) -> Vec<PathBuf> {
            MATERIAL.into_iter().map(|file| self.path(file)).collect()
        }

        async fn acquire(&self, fingerprint: u64, build_fails: bool) -> Result<u32, BuildFailed> {
            self.cache
                .get_or_build(KEY, fingerprint, &self.material(), || async {
                    if build_fails {
                        return Err(BuildFailed);
                    }
                    Ok(self.builds.fetch_add(1, Ordering::SeqCst) + 1)
                })
                .await
        }

        fn cached(&self) -> Option<u32> {
            self.cache.lock().get(KEY).map(|entry| entry.client)
        }
    }

    /// Inputs that change between a first acquisition and a second one.
    #[derive(Default)]
    struct Change {
        fingerprint_changed: bool,
        removed: Option<&'static str>,
        rotated: Option<&'static str>,
        build_fails: bool,
    }

    /// What the second acquisition returned, what the cache holds afterwards,
    /// and the TLS material events reported along the way.
    #[derive(Debug, PartialEq)]
    struct Observed {
        acquired: Result<u32, BuildFailed>,
        cached: Option<u32>,
        events: Vec<(&'static str, &'static str)>,
    }

    #[tokio::test]
    async fn caches_clients_until_their_inputs_change() {
        struct Row {
            scenario: &'static str,
            change: Change,
            expect: Observed,
        }

        let rows = [
            Row {
                scenario: "unchanged inputs reuse the cached client",
                change: Change::default(),
                expect: Observed {
                    acquired: Ok(1),
                    cached: Some(1),
                    events: vec![],
                },
            },
            Row {
                scenario: "a changed fingerprint rebuilds the client",
                change: Change {
                    fingerprint_changed: true,
                    ..Change::default()
                },
                expect: Observed {
                    acquired: Ok(2),
                    cached: Some(2),
                    events: vec![],
                },
            },
            Row {
                scenario: "a newer file after unchanged ones rebuilds the client",
                change: Change {
                    rotated: Some("tls.key"),
                    ..Change::default()
                },
                expect: Observed {
                    acquired: Ok(2),
                    cached: Some(2),
                    events: vec![("newer", "tls.key")],
                },
            },
            Row {
                scenario: "an unreadable file keeps the cached client",
                change: Change {
                    removed: Some("ca.crt"),
                    ..Change::default()
                },
                expect: Observed {
                    acquired: Ok(1),
                    cached: Some(1),
                    events: vec![("unreadable", "ca.crt")],
                },
            },
            Row {
                scenario: "an unreadable file does not hide a newer one",
                change: Change {
                    removed: Some("ca.crt"),
                    rotated: Some("tls.key"),
                    ..Change::default()
                },
                expect: Observed {
                    acquired: Ok(2),
                    cached: Some(2),
                    events: vec![("unreadable", "ca.crt"), ("newer", "tls.key")],
                },
            },
            Row {
                scenario: "a failed rebuild evicts the stale client",
                change: Change {
                    fingerprint_changed: true,
                    build_fails: true,
                    ..Change::default()
                },
                expect: Observed {
                    acquired: Err(BuildFailed),
                    cached: None,
                    events: vec![],
                },
            },
        ];

        for Row {
            scenario,
            change,
            expect,
        } in rows
        {
            let harness = Harness::new();
            assert_eq!(
                harness.acquire(1, false).await,
                Ok(1),
                "{scenario}: first build"
            );
            if let Some(file) = change.removed {
                std::fs::remove_file(harness.path(file)).expect("remove TLS material");
            }
            if let Some(file) = change.rotated {
                // An explicit future timestamp avoids depending on the
                // filesystem's modification-time resolution.
                set_modified(
                    &harness.path(file),
                    SystemTime::now() + Duration::from_secs(60),
                );
            }

            let fingerprint = if change.fingerprint_changed { 2 } else { 1 };
            let observed = Observed {
                acquired: harness.acquire(fingerprint, change.build_fails).await,
                cached: harness.cached(),
                events: EVENTS.take(),
            };
            assert_eq!(observed, expect, "{scenario}");
        }
    }

    #[tokio::test]
    async fn material_rewritten_while_building_is_newer_than_the_built_client() {
        let harness = Harness::new();
        let rewritten = harness.path("tls.key");
        let first = harness
            .cache
            .get_or_build(KEY, 1, &harness.material(), || async {
                // The sleeps keep the rewrite strictly between the start and
                // the end of the build, so the next call rebuilds only if the
                // creation time was taken before building.
                tokio::time::sleep(Duration::from_millis(10)).await;
                set_modified(&rewritten, SystemTime::now());
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<_, BuildFailed>(0)
            })
            .await;
        assert_eq!(first, Ok(0));

        assert_eq!(
            harness.acquire(1, false).await,
            Ok(1),
            "material rewritten while building triggers a rebuild"
        );
    }

    #[tokio::test]
    async fn builds_without_holding_the_cache_lock() {
        let cache = ClientCache::<String, (), u32>::new(record_event);
        let no_material: [&Path; 0] = [];
        let acquire = cache.get_or_build(KEY, (), &no_material, || async {
            assert!(
                cache.entries.try_lock().is_ok(),
                "the cache lock is free while building"
            );
            Ok::<_, BuildFailed>(1)
        });
        // A std `MutexGuard` held across an await point would make the
        // acquisition `!Send`.
        assert_send(&acquire);
        assert_eq!(acquire.await, Ok(1));
    }

    #[tokio::test]
    async fn debug_output_omits_fingerprints_and_clients() {
        let harness = Harness::new();
        assert_eq!(harness.acquire(0x5ec2e7, false).await, Ok(1));

        assert_eq!(format!("{:?}", harness.cache), "ClientCache { .. }");
    }

    #[tokio::test]
    async fn a_poisoned_lock_keeps_serving_cached_clients() {
        let harness = Harness::new();
        assert_eq!(harness.acquire(1, false).await, Ok(1));

        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = harness.cache.entries.lock().expect("an unpoisoned lock");
                    panic!("poison the cache lock");
                })
                .join()
                .expect_err("the poisoning thread panics");
        });
        assert!(harness.cache.entries.is_poisoned());

        assert_eq!(
            harness.acquire(1, false).await,
            Ok(1),
            "the recovered lock still serves the cached client"
        );
    }
}
