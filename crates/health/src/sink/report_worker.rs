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

use super::dedup_queue::DedupQueue;

/// Submits each entry taken from `queue` with `submit`, one at a time, for as
/// long as the task runs.
pub(super) async fn run_report_worker<K, V, F, Fut>(queue: Arc<DedupQueue<K, V>>, mut submit: F)
where
    K: Eq + Hash + Clone,
    F: FnMut(K, V) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let (key, value) = queue.next().await;
        submit(key, value).await;
    }
}
