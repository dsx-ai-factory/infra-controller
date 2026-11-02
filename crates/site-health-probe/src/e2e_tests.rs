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

//! The probe end to end on a fake site, on tokio's paused clock: the device
//! reader sweeps a fake nico-api, the state collector publishes, and the /v1
//! API answers from the same snapshots. This stands in for the site check
//! (database counts) until a site run.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use carbide_site_health_probe_api::CountResponse;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::api::{ApiState, router};
use crate::config::{ReaderConfig, StateConfig};
use crate::framework::{self, Probe};
use crate::gate::CallGate;
use crate::nico::fake::{self, FakeConnector, FakeNico, FakeSite};
use crate::readers::devices::{DEVICE_FAMILIES, DevicesReader};
use crate::snapshot::SnapshotSink;
use crate::state::store::SnapshotStore;
use crate::state_metrics::{CountingSink, StateCollector};

const HOSTS: u32 = 18;
/// Host i turns Ready at T0 + i × this.
const READY_STEP: Duration = Duration::from_secs(30);

/// One rack: 18 hosts with two DPUs each, 9 switches, 8 shelves, and an
/// expected record for every device.
fn one_rack() -> FakeSite {
    let mut site = FakeSite::default();
    for i in 1..=HOSTS {
        site.add_host(fake::host(
            i,
            "HostInitializing/WaitingForDiscovery",
            &[2 * i - 1, 2 * i],
        ));
        site.expected_machines
            .push(fake::expected_machine(i, Some(i)));
    }
    for i in 1..=9 {
        site.add_switch(fake::switch(i, "ready"));
        site.expected_switches
            .push(fake::expected_switch(i, Some(i)));
    }
    for i in 1..=8 {
        site.add_shelf(fake::shelf(i, "ready"));
        site.expected_shelves.push(fake::expected_shelf(i, Some(i)));
    }
    site.add_rack(fake::rack(1, "ready"));
    site.expected_racks.push(fake::expected_rack(1));
    site
}

pub(crate) async fn get<T: serde::de::DeserializeOwned>(
    app: &axum::Router,
    uri: &str,
) -> (StatusCode, Option<T>) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .expect("served");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    (status, serde_json::from_slice(&body).ok())
}

async fn count(app: &axum::Router, query: &str) -> u64 {
    let (status, reply) = get::<CountResponse>(app, &format!("/v1/count?{query}")).await;
    assert_eq!(status, StatusCode::OK, "{query}");
    reply.expect("count reply").count
}

#[tokio::test(start_paused = true)]
async fn fake_site_counts_match_the_site() {
    let t0 = crate::clock::now();
    let started = tokio::time::Instant::now();

    let mut cfg = StateConfig::default();
    cfg.readers.devices.interval = Duration::from_secs(20);
    cfg.paging.max_kept_snapshots = 16;
    let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
        .expect("metrics setup");
    let store = Arc::new(SnapshotStore::new(&cfg.paging));
    setup
        .registry
        .register(Box::new(StateCollector::new(
            Arc::clone(&store),
            t0,
            cfg.nico_api.host_read,
        )))
        .expect("register");
    let sink: Arc<dyn SnapshotSink> = Arc::new(CountingSink::new(Arc::clone(&store), &setup.meter));

    let nico = FakeNico::new(one_rack());
    let reader = DevicesReader::new(
        ReaderConfig {
            enabled: true,
            interval: cfg.readers.devices.interval,
            timeout: Duration::from_secs(600),
        },
        cfg.nico_api.batch_size,
        Arc::new(FakeConnector(Arc::clone(&nico))),
        Arc::new(CallGate::new(
            "nico-api",
            cfg.caps.calls_per_second,
            cfg.caps.call_timeout,
            cfg.caps.max_backoff,
        )),
        sink,
        t0,
    );

    let app = router(ApiState::new(Arc::clone(&store), &cfg));

    let cancel = CancellationToken::new();
    let probes: Vec<Arc<dyn Probe>> = vec![Arc::new(reader)];
    let (results, loops) = framework::start(probes, cancel.clone());
    let latency = Arc::new(crate::metrics::Metrics::new(&setup.meter));
    let collector = tokio::spawn(framework::collect(results, latency));

    // Host i turns Ready at T0 + 30 s × i.
    let transitions = tokio::spawn({
        let nico = Arc::clone(&nico);
        async move {
            for i in 1..=HOSTS {
                tokio::time::sleep_until(started + READY_STEP * i).await;
                let mut site = nico.site.lock().unwrap();
                let host = site
                    .hosts
                    .get_mut(&fake::host_id(i).to_string())
                    .expect("host");
                host.state = "Ready".to_string();
            }
        }
    });
    // Not ready, and no answer, before the first complete sweep.
    let mut ready = Box::pin(store.wait_all_ready(&DEVICE_FAMILIES));
    assert!(
        futures_poll_pending(&mut ready),
        "ready before the first sweep"
    );
    let (status, _) = get::<serde_json::Value>(&app, "/v1/count?type=host").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    tokio::time::timeout(Duration::from_secs(60), ready)
        .await
        .expect("ready after the first sweep");
    assert_eq!(count(&app, "type=host&stage=ready").await, 0);

    // Past the last transition and two more sweeps.
    tokio::time::sleep_until(started + READY_STEP * HOSTS + Duration::from_secs(60)).await;
    transitions.await.expect("transitions");

    // Counts equal the fake site's truth.
    for (query, truth) in [
        ("type=host", HOSTS as u64),
        ("type=host&stage=ready", HOSTS as u64),
        ("type=dpu", 2 * HOSTS as u64),
        ("type=dpu&stage=ready", 2 * HOSTS as u64),
        ("type=switch&stage=ready", 9),
        ("type=power_shelf", 8),
        ("type=rack", 1),
        (
            "type=expected_record&recordType=host&linked=true",
            HOSTS as u64,
        ),
        ("type=expected_record&recordType=switch&linked=true", 9),
        ("type=expected_record&recordType=rack&linked=true", 1),
        ("type=identity_mismatch", 0),
    ] {
        assert_eq!(count(&app, query).await, truth, "{query}");
    }

    cancel.cancel();
    framework::join_loops(loops).await.expect("loops end");
    collector.await.expect("collector");
}

/// Whether a pinned future is still pending after one poll.
fn futures_poll_pending<F: std::future::Future>(future: &mut std::pin::Pin<Box<F>>) -> bool {
    future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        .is_pending()
}
