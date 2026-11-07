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

//! Submission loop shared by the health-report sinks.

use std::hash::Hash;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::dedup_queue::DedupQueue;
use super::shutdown::DeliveryCounts;

/// Submits each entry taken from `queue` with `submit`, one at a time.
///
/// `submit` returns whether the entry was delivered; `counts` records each
/// entry as one record. Once `flush_token` is cancelled, the worker finishes
/// its current submission, submits every entry still queued, and returns.
pub(super) async fn run_report_worker<K, V, F, Fut>(
    queue: Arc<DedupQueue<K, V>>,
    counts: DeliveryCounts,
    flush_token: CancellationToken,
    mut submit: F,
) where
    K: Eq + Hash + Clone,
    F: FnMut(K, V) -> Fut,
    Fut: Future<Output = bool>,
{
    while let Some((key, value)) = flush_token.run_until_cancelled(queue.next()).await {
        submit_counted(&counts, &mut submit, key, value).await;
    }

    while let Some((key, value)) = queue.pop() {
        submit_counted(&counts, &mut submit, key, value).await;
    }
}

async fn submit_counted<K, V, F, Fut>(counts: &DeliveryCounts, submit: &mut F, key: K, value: V)
where
    F: FnMut(K, V) -> Fut,
    Fut: Future<Output = bool>,
{
    counts.start(1);
    let delivered = usize::from(submit(key, value).await);
    counts.finish(1, delivered, 1 - delivered);
}
