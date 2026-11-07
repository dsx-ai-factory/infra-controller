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

//! Tests for the on_dpu_ready callback.

use std::sync::Arc;

use super::helpers::{Collector, WatcherMock, make_dpu};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-ready-ns";

#[tokio::test]
async fn test_ready_invoked() {
    let m = Arc::new(WatcherMock::new());
    let c = Arc::new(Collector::<DpuReadyEvent>::default());
    let cc = c.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_ready(move |e| {
            let cc = cc.clone();
            async move {
                cc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev", "n1", DpuStatusPhase::Ready));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().dpu_name, "d1");
}

#[tokio::test]
async fn test_ready_not_invoked_non_ready() {
    let m = Arc::new(WatcherMock::new());
    let ready = Arc::new(Collector::<DpuReadyEvent>::default());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());
    let rc = ready.clone();
    let dc = dpu_events.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_ready(move |e| {
            let rc = rc.clone();
            async move {
                rc.push(e);
                Ok(())
            }
        })
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(
        TEST_NS,
        "d1",
        "dev",
        "n1",
        DpuStatusPhase::Initializing,
    ));
    m.emit_dpu(make_dpu(TEST_NS, "d2", "dev", "n1", DpuStatusPhase::Error));
    dpu_events.wait_for(2).await;
    assert_eq!(ready.len(), 0);
}
