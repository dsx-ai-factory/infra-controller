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

//! Tests for the on_error callback.

use std::sync::Arc;

use super::helpers::{Collector, WatcherMock, make_dpu};
use crate::crds::dpus_generated::*;
use crate::types::*;
use crate::watcher::DpuWatcherBuilder;

const TEST_NS: &str = "watcher-error-ns";

#[tokio::test]
async fn test_error_fires_on_error_phase() {
    let m = Arc::new(WatcherMock::new());
    let c = Arc::new(Collector::<DpuErrorEvent>::default());
    let cc = c.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_error(move |e| {
            let cc = cc.clone();
            async move {
                cc.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Error));
    c.wait_for(1).await;
    assert_eq!(c.get(0).unwrap().dpu_name, "d1");
    assert_eq!(c.get(0).unwrap().device_name, "dev1");
    assert_eq!(c.get(0).unwrap().node_name, "n1");
}

#[tokio::test]
async fn test_error_does_not_fire_on_ready() {
    let m = Arc::new(WatcherMock::new());
    let error_events = Arc::new(Collector::<DpuErrorEvent>::default());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());

    let ec = error_events.clone();
    let dc = dpu_events.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .on_error(move |e| {
            let ec = ec.clone();
            async move {
                ec.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Ready));
    dpu_events.wait_for(1).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(error_events.len(), 0, "on_error should not fire on Ready");
}

#[tokio::test]
async fn test_error_and_dpu_event_both_fire() {
    let m = Arc::new(WatcherMock::new());
    let error_events = Arc::new(Collector::<DpuErrorEvent>::default());
    let dpu_events = Arc::new(Collector::<DpuEvent>::default());

    let ec = error_events.clone();
    let dc = dpu_events.clone();
    let _h = DpuWatcherBuilder::new(m.clone(), TEST_NS)
        .on_dpu_event(move |e| {
            let dc = dc.clone();
            async move {
                dc.push(e);
                Ok(())
            }
        })
        .on_error(move |e| {
            let ec = ec.clone();
            async move {
                ec.push(e);
                Ok(())
            }
        })
        .start()
        .unwrap();
    m.wait_for_watchers(1).await;
    m.emit_dpu(make_dpu(TEST_NS, "d1", "dev1", "n1", DpuStatusPhase::Error));
    dpu_events.wait_for(1).await;
    error_events.wait_for(1).await;
    assert_eq!(dpu_events.get(0).unwrap().phase, DpuPhase::Error);
    assert_eq!(error_events.get(0).unwrap().dpu_name, "d1");
}
