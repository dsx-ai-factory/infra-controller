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

//! Latest-wins dedup queue.
//!
//! Generic queue keyed by `K` that replaces the value when the same key
//! is pushed again. A bounded queue evicts the oldest distinct key when a new
//! key arrives at capacity. Used by health report sinks (keyed by machine/rack
//! + report source) and OtlpSink (keyed by event type identity string).

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

struct QueueState<K: Eq + Hash, V> {
    values: HashMap<K, V>,
    ready: VecDeque<K>,
}

/// Result of saving a value by its deduplication key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SaveOutcome {
    /// A new key was added without evicting another entry.
    Inserted,

    /// The value for an existing key was replaced in its current queue position.
    Replaced,

    /// A new key was added after evicting the oldest distinct key.
    DroppedOldest,
}

pub(crate) struct DedupQueue<K: Eq + Hash + Clone, V> {
    state: Mutex<QueueState<K, V>>,
    capacity: Option<NonZeroUsize>,
    bounded_len: AtomicUsize,
    notify: Notify,
}

impl<K: Eq + Hash + Clone, V> DedupQueue<K, V> {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                values: HashMap::new(),
                ready: VecDeque::new(),
            }),
            capacity: None,
            bounded_len: AtomicUsize::new(0),
            notify: Notify::new(),
        }
    }

    /// Creates a queue limited to `capacity` distinct keys.
    pub(super) fn bounded(capacity: NonZeroUsize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                values: HashMap::new(),
                ready: VecDeque::new(),
            }),
            capacity: Some(capacity),
            bounded_len: AtomicUsize::new(0),
            notify: Notify::new(),
        }
    }

    /// Saves the latest value for `key` and wakes a waiting consumer.
    ///
    /// Replacing an existing key preserves its queue position. When a bounded
    /// queue is full, inserting a new key evicts the oldest distinct key.
    pub(super) fn save_latest(&self, key: K, value: V) -> SaveOutcome {
        let outcome;

        {
            let mut state = self.state.lock().expect("dedup queue mutex poisoned");
            if state.values.contains_key(&key) {
                state.values.insert(key, value);

                outcome = SaveOutcome::Replaced;
            } else {
                outcome = if self
                    .capacity
                    .is_some_and(|capacity| capacity.get() == state.values.len())
                {
                    if let Some(oldest) = state.ready.pop_front() {
                        state.values.remove(&oldest);
                        SaveOutcome::DroppedOldest
                    } else {
                        SaveOutcome::Inserted
                    }
                } else {
                    SaveOutcome::Inserted
                };

                state.values.insert(key.clone(), value);
                state.ready.push_back(key);
            }

            if outcome == SaveOutcome::Inserted && self.capacity.is_some() {
                self.bounded_len
                    .store(state.values.len(), Ordering::Release);
            }
        }

        self.notify.notify_one();
        outcome
    }

    pub(super) async fn next(&self) -> (K, V) {
        loop {
            if let Some(pair) = self.pop() {
                return pair;
            }
            self.notify.notified().await;
        }
    }

    pub(crate) fn pop(&self) -> Option<(K, V)> {
        let mut state = self.state.lock().expect("dedup queue mutex poisoned");
        while let Some(key) = state.ready.pop_front() {
            if let Some(value) = state.values.remove(&key) {
                if self.capacity.is_some() {
                    self.bounded_len
                        .store(state.values.len(), Ordering::Release);
                }

                return Some((key, value));
            }
        }
        None
    }

    pub(crate) async fn notified(&self) {
        self.notify.notified().await;
    }

    /// Returns a lock-free snapshot of the number of entries in a bounded queue.
    ///
    /// Unbounded queues do not track their depth and return zero.
    pub(super) fn len(&self) -> usize {
        self.bounded_len.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_by_key() {
        let queue = DedupQueue::<String, i32>::new();

        queue.save_latest("a".into(), 1);
        queue.save_latest("a".into(), 2);
        queue.save_latest("b".into(), 3);

        let mut count = 0;
        while queue.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn different_keys_are_separate() {
        let queue = DedupQueue::<String, i32>::new();

        queue.save_latest("a".into(), 1);
        queue.save_latest("b".into(), 2);

        let mut count = 0;
        while queue.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn preserves_fifo_order() {
        let queue = DedupQueue::<String, i32>::new();

        queue.save_latest("first".into(), 1);
        queue.save_latest("second".into(), 2);

        assert_eq!(queue.pop().unwrap().0, "first");
        assert_eq!(queue.pop().unwrap().0, "second");
        assert!(queue.pop().is_none());
    }

    #[test]
    fn update_replaces_value_but_keeps_position() {
        let queue = DedupQueue::<String, i32>::new();

        queue.save_latest("a".into(), 1);
        queue.save_latest("b".into(), 2);
        queue.save_latest("a".into(), 99);

        let (key_a, val_a) = queue.pop().unwrap();
        assert_eq!(key_a, "a");
        assert_eq!(val_a, 99);
        assert_eq!(queue.pop().unwrap().0, "b");
    }

    #[test]
    fn bounded_queue_drops_oldest_distinct_key_and_tracks_depth() {
        let queue = DedupQueue::<String, i32>::bounded(NonZeroUsize::new(2).unwrap());

        assert_eq!(queue.save_latest("a".into(), 1), SaveOutcome::Inserted);
        assert_eq!(queue.save_latest("b".into(), 2), SaveOutcome::Inserted);
        assert_eq!(queue.save_latest("c".into(), 3), SaveOutcome::DroppedOldest);

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(("b".to_string(), 2)));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop(), Some(("c".to_string(), 3)));
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn bounded_queue_replacement_does_not_evict() {
        let queue = DedupQueue::<String, i32>::bounded(NonZeroUsize::new(2).unwrap());

        queue.save_latest("a".into(), 1);
        queue.save_latest("b".into(), 2);

        assert_eq!(queue.save_latest("a".into(), 3), SaveOutcome::Replaced);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(("a".to_string(), 3)));
        assert_eq!(queue.pop(), Some(("b".to_string(), 2)));
    }
}
