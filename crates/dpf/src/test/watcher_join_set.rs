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

//! Tests for watcher JoinSet integration.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;

use super::helpers::{Collector, WatcherMock, make_dpu};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-joinset-ns";

#[tokio::test]
async fn test_with_join_set_spawns_into_provided_set() {
    let m = Arc::new(WatcherMock::new());
    let c = Arc::new(Collector::<DpuEvent>::default());
    let cc = c.clone();

    let mut join_set = JoinSet::new();
    assert_eq!(join_set.len(), 0);

    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .with_join_set(&mut join_set)
        .on_dpu_event(move |e| {
            let cc = cc.clone();
            async move {
                cc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();

    assert_eq!(join_set.len(), 1);

    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Ready));
    c.wait_for(1).await;
    assert_eq!(c.len(), 1);
}

#[tokio::test]
async fn test_with_join_set_task_completes_after_drop() {
    let m = Arc::new(WatcherMock::new());

    let mut join_set = JoinSet::new();
    let h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .with_join_set(&mut join_set)
        .on_dpu_event(|_| async { Ok(()) })
        .start()
        .unwrap();

    m.wait_for_watchers(1).await;
    assert_eq!(join_set.len(), 1);

    drop(h);

    let result = tokio::time::timeout(Duration::from_secs(5), join_set.join_next()).await;
    assert!(
        result.is_ok(),
        "task should complete after watcher is dropped"
    );
    assert_eq!(join_set.len(), 0);
}
