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

//! Counts the `sqlx::query` events a measured block emits, so a test can
//! assert how many database round-trips a code path costs. Used by the
//! query-count regression tests that pin "this used to be N statements and
//! is now 1" claims.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

/// A tracing `Layer` that tallies every `sqlx::query` event. sqlx emits one
/// such event per statement it executes, so the tally is a direct count of
/// database round-trips inside the scope it is attached to.
///
/// sqlx consults the *current* dispatcher when logging a statement, so the
/// scoped `Dispatch` installed by `with_subscriber` sees these events
/// regardless of any global test subscriber's `sqlx=warn` filter.
///
/// `BEGIN` and a dropped transaction's rollback are queued without an
/// executed statement and tally nothing; an explicit `COMMIT` or `ROLLBACK`
/// tallies one event each. Counted futures therefore open their transaction
/// inside the measured block and either drop it there or return it out for
/// verification, keeping the tally exactly the statements under test.
#[derive(Clone, Default)]
pub struct QueryCounter(Arc<AtomicUsize>);

impl QueryCounter {
    /// Number of `sqlx::query` events tallied so far.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for QueryCounter {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target().starts_with("sqlx::query") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Drives `fut` under a scoped subscriber holding a fresh [`QueryCounter`]
/// and returns the future's output together with the number of `sqlx::query`
/// events it emitted.
pub async fn count_queries<F, T>(fut: F) -> (T, usize)
where
    F: std::future::Future<Output = T>,
{
    let counter = QueryCounter::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(counter.clone()));
    let out = fut.with_subscriber(dispatch).await;
    let count = counter.count();
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the counting rule without a database: exactly the events whose
    /// target starts with `sqlx::query` tally, everything else is ignored.
    #[tokio::test]
    async fn counts_only_sqlx_query_targets() {
        let ((), count) = count_queries(async {
            tracing::event!(target: "sqlx::query", tracing::Level::DEBUG, "counted");
            tracing::event!(target: "sqlx::query::slow", tracing::Level::WARN, "counted");
            tracing::event!(target: "sqlx::pool", tracing::Level::DEBUG, "ignored");
            tracing::event!(target: "app", tracing::Level::INFO, "ignored");
        })
        .await;
        assert_eq!(count, 2, "exactly the sqlx::query-prefixed events tally");
    }
}
