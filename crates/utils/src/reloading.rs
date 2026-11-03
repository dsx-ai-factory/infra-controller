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

//! A value built from files that change while a service runs, such as TLS
//! material cert-manager renews in a mounted Secret.
//!
//! [`Reloading::current`] answers with the value it holds and, when a check
//! is due, checks the files in the background: the value is rebuilt only
//! when their contents changed, and the rebuilt value serves the calls after
//! it. A check that fails keeps the value held. Its outcome goes to the
//! holder's `on_check`, which reports a failure as the holder's own metric.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

/// When a [`Reloading`] value's files are checked again.
#[derive(Clone, Copy, Debug)]
pub struct ReloadSchedule {
    /// After the value was built, or a check found nothing to rebuild.
    pub every: Duration,
    /// After a check failed.
    pub retry: Duration,
}

/// Why a check left a [`Reloading`] value as it was, or why one could not be
/// built at all.
#[derive(Debug, thiserror::Error)]
pub enum ReloadError {
    /// A file could not be read.
    #[error("{name} {path:?}: {source}")]
    Read {
        /// What the file is, as the configuration key giving its path names it.
        name: String,
        /// Where the file was read from.
        path: PathBuf,
        /// Why the read failed.
        #[source]
        source: std::io::Error,
    },
    /// The value could not be built from the files' contents.
    #[error(transparent)]
    Build(Box<dyn std::error::Error + Send + Sync>),
    /// The check stopped before it finished: the rebuild panicked, or the
    /// runtime shut down under it.
    #[error("the check stopped before it finished: {0}")]
    Interrupted(#[source] tokio::task::JoinError),
}

/// A value built from the contents of files, and rebuilt when they change.
pub struct Reloading<T> {
    inner: Arc<Inner<T>>,
}

type Build<T> =
    dyn Fn(&[Vec<u8>]) -> Result<T, Box<dyn std::error::Error + Send + Sync>> + Send + Sync;

type OnCheck<T> = dyn Fn(&T, Option<&ReloadError>) + Send + Sync;

struct Inner<T> {
    files: Vec<(String, PathBuf)>,
    schedule: ReloadSchedule,
    build: Box<Build<T>>,
    on_check: Box<OnCheck<T>>,
    state: Mutex<State<T>>,
}

struct State<T> {
    value: T,
    /// The files' contents `value` was built from.
    contents: Vec<Vec<u8>>,
    /// When the next check is due.
    next_check: Instant,
    /// The check last started, until a test waits for it.
    check: Option<JoinHandle<()>>,
}

impl<T: Clone + Send + 'static> Reloading<T> {
    /// Reads `files`, each a name for errors and a path, on the calling
    /// thread, and builds the value from their contents, in that order, with
    /// `build`. `on_check` sees the value built, and then each check's
    /// outcome: the value held afterwards, and why the check left it as it
    /// was, when it failed. The files are checked again as `schedule` says.
    pub fn new<const N: usize, E>(
        files: [(String, PathBuf); N],
        schedule: ReloadSchedule,
        build: impl Fn(&[Vec<u8>; N]) -> Result<T, E> + Send + Sync + 'static,
        on_check: impl Fn(&T, Option<&ReloadError>) + Send + Sync + 'static,
    ) -> Result<Self, ReloadError>
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let build = move |contents: &[Vec<u8>]| {
            let contents = contents
                .try_into()
                .expect("a check reads every file the value is built from");
            build(contents).map_err(Into::into)
        };
        let files = Vec::from(files);
        let contents = read(&files)?;
        let value = build(&contents).map_err(ReloadError::Build)?;
        on_check(&value, None);
        Ok(Self {
            inner: Arc::new(Inner {
                files,
                schedule,
                build: Box::new(build),
                on_check: Box::new(on_check),
                state: Mutex::new(State {
                    value,
                    contents,
                    next_check: Instant::now() + schedule.every,
                    check: None,
                }),
            }),
        })
    }

    /// The value held. When a check is due and none is running, this starts
    /// one in the background, so it must be called within a Tokio runtime.
    pub fn current(&self) -> T {
        let mut state = lock(&self.inner.state);
        let now = Instant::now();
        let idle = state.check.as_ref().is_none_or(JoinHandle::is_finished);
        if idle && now >= state.next_check {
            // Claimed until the check reschedules itself, so a check that
            // never finishes is not restarted on every call.
            state.next_check = now + self.inner.schedule.every;
            let inner = Arc::clone(&self.inner);
            state.check = Some(tokio::spawn(async move {
                let checking = Arc::clone(&inner);
                let outcome = tokio::task::spawn_blocking(move || checking.check())
                    .await
                    .unwrap_or_else(|interrupted| Err(ReloadError::Interrupted(interrupted)));
                inner.finish(outcome);
            }));
        }
        state.value.clone()
    }

    /// Waits for the check last started, if one was.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn settled(&self) {
        let check = lock(&self.inner.state).check.take();
        if let Some(check) = check {
            check.await.expect("a check does not panic");
        }
    }
}

impl<T: Clone> Inner<T> {
    /// Reads the files and rebuilds the value from them when their contents
    /// differ from those it was built from. Blocks on the reads.
    fn check(&self) -> Result<(), ReloadError> {
        let contents = read(&self.files)?;
        let unchanged = contents == lock(&self.state).contents;
        if unchanged {
            return Ok(());
        }
        let value = (self.build)(&contents).map_err(ReloadError::Build)?;
        let mut state = lock(&self.state);
        state.value = value;
        state.contents = contents;
        Ok(())
    }

    /// Schedules the next check by `outcome`, and hands it to `on_check`.
    fn finish(&self, outcome: Result<(), ReloadError>) {
        let value = {
            let mut state = lock(&self.state);
            let wait = match outcome {
                Ok(()) => self.schedule.every,
                Err(_) => self.schedule.retry,
            };
            state.next_check = Instant::now() + wait;
            state.value.clone()
        };
        (self.on_check)(&value, outcome.as_ref().err());
    }
}

fn read(files: &[(String, PathBuf)]) -> Result<Vec<Vec<u8>>, ReloadError> {
    files
        .iter()
        .map(|(name, path)| {
            std::fs::read(path).map_err(|source| ReloadError::Read {
                name: name.clone(),
                path: path.clone(),
                source,
            })
        })
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("a reloading value's mutex must not be poisoned")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{ReloadError, ReloadSchedule, Reloading};

    const SCHEDULE: ReloadSchedule = ReloadSchedule {
        every: Duration::from_secs(60),
        retry: Duration::from_secs(10),
    };

    /// The contents a test value is built from: a number, or `bad`, which
    /// fails the build, or `panic`, which panics it.
    fn build([contents]: &[Vec<u8>; 1]) -> Result<u32, String> {
        let contents = String::from_utf8_lossy(contents);
        match contents.as_ref() {
            "panic" => panic!("the test build panics"),
            number => number
                .parse()
                .map_err(|_| format!("not a number: {number}")),
        }
    }

    /// Every check's outcome as `on_check` saw it: the value held, and the
    /// failure, if the check failed.
    type Checks = Arc<Mutex<Vec<(u32, Option<String>)>>>;

    fn reloading(file: &Path, builds: &Arc<Mutex<u32>>) -> (Reloading<u32>, Checks) {
        let checks = Checks::default();
        let builds = Arc::clone(builds);
        let reloading = Reloading::new(
            [("value".to_string(), file.to_path_buf())],
            SCHEDULE,
            move |contents: &[Vec<u8>; 1]| {
                *builds.lock().unwrap() += 1;
                build(contents)
            },
            {
                let checks = Arc::clone(&checks);
                move |value: &u32, failure: Option<&ReloadError>| {
                    checks
                        .lock()
                        .unwrap()
                        .push((*value, failure.map(ToString::to_string)));
                }
            },
        )
        .expect("the first build succeeds");
        (reloading, checks)
    }

    /// The value serves until its check is due, then while the check runs;
    /// the value rebuilt from changed files serves after it, and files that
    /// did not change are not rebuilt from.
    #[tokio::test(start_paused = true)]
    async fn a_value_is_rebuilt_once_its_files_change() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("value");
        std::fs::write(&file, "1").unwrap();
        let builds = Arc::new(Mutex::new(0));
        let (reloading, checks) = reloading(&file, &builds);

        std::fs::write(&file, "2").unwrap();
        tokio::time::advance(SCHEDULE.every - Duration::from_secs(1)).await;
        assert_eq!(reloading.current(), 1, "no check before it is due");
        reloading.settled().await;
        assert_eq!(*builds.lock().unwrap(), 1);

        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(reloading.current(), 1, "served while the check runs");
        reloading.settled().await;
        assert_eq!(reloading.current(), 2, "rebuilt from the changed file");

        tokio::time::advance(SCHEDULE.every).await;
        reloading.current();
        reloading.settled().await;
        assert_eq!(
            *builds.lock().unwrap(),
            2,
            "an unchanged file is not rebuilt from"
        );
        assert_eq!(
            *checks.lock().unwrap(),
            [(1, None), (2, None), (2, None)],
            "the first build and every check are reported"
        );
    }

    /// A check whose files cannot be read, or whose rebuild fails or panics,
    /// keeps the value, reports why, and is retried after `retry` rather
    /// than `every`.
    #[tokio::test(start_paused = true)]
    async fn a_failed_check_keeps_the_value_and_is_retried_sooner() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("value");
        std::fs::write(&file, "1").unwrap();
        let (reloading, checks) = reloading(&file, &Arc::new(Mutex::new(0)));
        let check = || async {
            reloading.current();
            reloading.settled().await;
            let checks = checks.lock().unwrap();
            let (value, failure) = checks.last().expect("a check was reported").clone();
            (checks.len(), value, failure)
        };

        std::fs::write(&file, "bad").unwrap();
        tokio::time::advance(SCHEDULE.every).await;
        assert_eq!(check().await, (2, 1, Some("not a number: bad".to_string())));

        std::fs::remove_file(&file).unwrap();
        tokio::time::advance(SCHEDULE.retry - Duration::from_secs(1)).await;
        assert_eq!(check().await.0, 2, "not retried before `retry`");
        tokio::time::advance(Duration::from_secs(1)).await;
        let (_, value, failure) = check().await;
        assert_eq!(value, 1);
        assert!(
            failure.is_some_and(|failure| failure.starts_with("value ")),
            "an unreadable file is named"
        );

        std::fs::write(&file, "panic").unwrap();
        tokio::time::advance(SCHEDULE.retry).await;
        let (_, value, failure) = check().await;
        assert_eq!(value, 1);
        assert!(
            failure.is_some_and(|failure| failure.contains("stopped before it finished")),
            "a panicking rebuild is reported"
        );

        std::fs::write(&file, "3").unwrap();
        tokio::time::advance(SCHEDULE.retry).await;
        assert_eq!(check().await, (5, 3, None), "checks go on after a panic");
    }

    /// While a check runs, a call that finds another due starts none: the
    /// files changed again meanwhile are rebuilt from by a later check, not
    /// a second one beside it.
    #[tokio::test(start_paused = true)]
    async fn a_running_check_is_not_started_again() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("value");
        std::fs::write(&file, "1").unwrap();
        let (started, mut rebuilding) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(released);
        let built = Arc::new(Mutex::new(Vec::<String>::new()));
        let reloading = Reloading::new(
            [("value".to_string(), file.clone())],
            SCHEDULE,
            {
                let built = Arc::clone(&built);
                move |contents: &[Vec<u8>; 1]| {
                    let [value] = contents;
                    built
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(value).into_owned());
                    // The rebuild from 2 holds its check until the test lets it go.
                    if value == b"2" {
                        started.send(()).unwrap();
                        released
                            .lock()
                            .unwrap()
                            .recv()
                            .expect("the test releases the rebuild");
                    }
                    build(contents)
                }
            },
            |_: &u32, _: Option<&ReloadError>| {},
        )
        .expect("the first build succeeds");

        std::fs::write(&file, "2").unwrap();
        tokio::time::advance(SCHEDULE.every).await;
        reloading.current();
        rebuilding.recv().await.expect("the check rebuilds from 2");

        std::fs::write(&file, "3").unwrap();
        tokio::time::advance(SCHEDULE.every).await;
        assert_eq!(
            reloading.current(),
            1,
            "the running check still serves the old value"
        );
        release.send(()).unwrap();
        reloading.settled().await;
        assert_eq!(reloading.current(), 2);
        assert_eq!(
            *built.lock().unwrap(),
            ["1", "2"],
            "no check started beside it"
        );
    }
}
