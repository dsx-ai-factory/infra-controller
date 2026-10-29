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

use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::http::{HeaderMap, Request};
use carbide_site_health_probe_api::{
    ExpectedFields, FirmwareRequest, HostFields, Initiator, RecordType, RestResource, SiteMatch,
    Stage, VersionSource,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::config::ReaderConfig;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::CallGate;
use crate::nico::fake::{self, FakeConnector, FakeNico, FakeSite};
use crate::readers::devices::DevicesReader;
use crate::snapshot::{FamilySnapshot, Reader, SnapshotSink, SweepMeta, UnitsData};
use crate::state_metrics::StateCollector;

struct Harness {
    store: Arc<SnapshotStore>,
    router: Router,
    registry: prometheus::Registry,
}

fn state_config() -> StateConfig {
    StateConfig::default()
}

fn harness() -> Harness {
    let cfg = state_config();
    let store = Arc::new(SnapshotStore::new(&cfg.paging));
    let registry = prometheus::Registry::new();
    registry
        .register(Box::new(StateCollector::new(
            Arc::clone(&store),
            SystemTime::now(),
            cfg.nico_api.host_read,
        )))
        .expect("register");
    Harness {
        router: router(ApiState::new(Arc::clone(&store), &cfg)),
        store,
        registry,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    fn field(&self) -> &str {
        self.body["error"]["field"].as_str().unwrap_or_default()
    }

    fn count(&self) -> u64 {
        assert_eq!(self.status, StatusCode::OK, "{}", self.body);
        self.body["count"].as_u64().expect("count")
    }

    fn units(&self) -> UnitsResponse {
        assert_eq!(self.status, StatusCode::OK, "{}", self.body);
        serde_json::from_value(self.body.clone()).expect("units reply")
    }
}

impl Harness {
    async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.expect("served");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
            .await
            .expect("body");
        Reply {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn get(&self, uri: &str) -> Reply {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    async fn post(&self, uri: &str, body: Value) -> Reply {
        self.send(
            Request::post(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    fn publish<T: Send + Sync + 'static>(&self, family: Family, seq: u64, data: T) {
        self.store.publish(
            family,
            Arc::new(FamilySnapshot {
                meta: SweepMeta {
                    process: 1,
                    seq,
                    reader: Reader::Devices,
                    start: UNIX_EPOCH + Duration::from_secs(1_000 + seq * 100),
                    end: UNIX_EPOCH + Duration::from_secs(1_050 + seq * 100),
                },
                data,
            }),
        );
    }

    fn publish_units(&self, family: Family, seq: u64, units: Vec<Unit>) {
        self.publish(family, seq, UnitsData::new(units, 0));
    }

    /// The sum of the published gauge's samples whose labels include all
    /// of `matchers`.
    fn metric_sum(&self, metric: &str, matchers: &[(&str, String)]) -> f64 {
        use prometheus::Encoder;
        let mut buffer = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&self.registry.gather(), &mut buffer)
            .expect("encode");
        String::from_utf8(buffer)
            .expect("utf-8")
            .lines()
            .filter(|line| {
                line.split(['{', ' ']).next()
                    == Some(&format!("carbide_site_health_probe_{metric}"))
            })
            .filter(|line| {
                matchers
                    .iter()
                    .all(|(k, v)| line.contains(&format!("{k}=\"{v}\"")))
            })
            .filter_map(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
            .sum()
    }
}

fn plain(unit_type: UnitType, id: &str, state: &str, stage: Stage) -> Unit {
    Unit {
        id: id.to_string(),
        unit_type,
        source: Source::NicoApi,
        stage,
        done: stage == Stage::Ready,
        state: state.to_string(),
        reason: (stage != Stage::Ready).then(|| state.to_string()),
        bmc_ip: None,
        host: None,
        dpu: None,
        instance: None,
        expected: None,
    }
}

fn host(i: usize, state: &str, model: &str, bmc: &str) -> Unit {
    let stage = match state {
        "Ready" => Stage::Ready,
        "Failed" => Stage::Error,
        _ => Stage::Registered,
    };
    Unit {
        host: Some(HostFields {
            vendor: Some("NVIDIA".to_string()),
            model: Some(model.to_string()),
            instance_type: i.is_multiple_of(2).then(|| "it-1".to_string()),
            bmc_version: Some(bmc.to_string()),
            uefi_version: Some("u1".to_string()),
            version_source: VersionSource::MachineRecord,
            report_at: None,
            report_error: None,
            allocatable: None,
            blocking_reason: None,
            firmware_autoupdate: None,
            update_eligible: None,
            update_blocking_reason: None,
            update_complete: i.is_multiple_of(3),
            open_firmware_request: None,
            nvlink_domain: None,
            rack_id: None,
            dpu_ids: vec![],
        }),
        ..plain(UnitType::Host, &format!("fm100h{i:04}"), state, stage)
    }
}

fn expected(i: usize, record_type: RecordType, linked: bool) -> Unit {
    Unit {
        expected: Some(ExpectedFields {
            record_type,
            linked,
            device_id: None,
            bmc_mac: None,
            explored_address: None,
        }),
        done: linked,
        reason: (!linked).then(|| "no_device".to_string()),
        ..plain(
            UnitType::ExpectedRecord,
            &format!("e-{i:03}"),
            if linked { "linked" } else { "unlinked" },
            Stage::Expected,
        )
    }
}

/// Twelve hosts over three states and two models, eight DPUs, and expected
/// records of three types.
fn small_fleet(h: &Harness) {
    let states = ["Ready", "HostInitializing", "Failed"];
    let hosts = (0..12)
        .map(|i| {
            host(
                i,
                states[i % 3],
                ["gb200", "r750"][i % 2],
                ["b1", "b2"][i % 2],
            )
        })
        .collect();
    h.publish_units(Family::Host, 1, hosts);
    let dpus = (0..8)
        .map(|i| {
            let (state, stage) = if i < 5 {
                ("Ready", Stage::Ready)
            } else {
                ("Failed", Stage::Error)
            };
            plain(UnitType::Dpu, &format!("fm100d{i:04}"), state, stage)
        })
        .collect();
    h.publish_units(Family::Dpu, 1, dpus);
    let records = (0..9)
        .map(|i| {
            let record_type = [RecordType::Host, RecordType::Switch, RecordType::Rack][i % 3];
            expected(i, record_type, i % 2 == 0)
        })
        .collect();
    h.publish_units(Family::Expected, 1, records);
}

const COUNT_FILTERS: [&str; 10] = [
    "type=host",
    "type=host&stage=ready",
    "type=host&stage=error&state=Failed",
    "type=host&model=gb200",
    "type=host&model=r750&stage=ready",
    "type=host&instanceType=it-1&updateComplete=true",
    "type=dpu&stage=ready",
    "type=dpu&stage=error",
    "type=expected_record&recordType=switch&linked=true",
    "type=expected_record&recordType=rack",
];

#[tokio::test]
async fn count_equals_published_metric_sum() {
    let h = harness();
    small_fleet(&h);
    let caps = ApiState::new(Arc::clone(&h.store), &state_config()).caps;
    for query in COUNT_FILTERS {
        let reply = h.get(&format!("/v1/count?{query}")).await;
        let filter: Filter = serde_urlencoded::from_str(query).unwrap();
        let spec = series_spec(&filter, &caps).unwrap();
        let metric = h.metric_sum(spec.metric, &spec.matchers);
        assert_eq!(reply.count() as f64, metric, "{query}");
    }
    assert_eq!(h.get("/v1/count?type=host").await.count(), 12);
    assert_eq!(h.get("/v1/count?type=host&stage=ready").await.count(), 4);
    assert_eq!(h.get("/v1/count?type=dpu&stage=ready").await.count(), 5);
    let reply = h.get("/v1/count?type=host").await;
    assert_eq!(reply.body["family"], "host");
    assert_eq!(reply.body["snapshotId"], "1-devices-1");
    assert_eq!(reply.body["sweepIntervalSeconds"], 120.0);
    assert_eq!(reply.body["readAt"], "1970-01-01T00:19:10Z");
}

/// Pages `/v1/units?{query}` and checks its unit count equals `/v1/count`.
async fn assert_count_and_units_agree(h: &Harness, query: &str) -> u64 {
    let count = h.get(&format!("/v1/count?{query}")).await;
    let mut ids = BTreeSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut uri = format!("/v1/units?{query}&limit=3");
        if let Some(cursor) = &cursor {
            uri.push_str(&format!("&cursor={cursor}"));
        }
        let page = h.get(&uri).await.units();
        assert_eq!(page.sweep.snapshot_id, count.body["snapshotId"], "{query}");
        for unit in page.units {
            assert!(ids.insert(unit.id), "{query}: a unit twice");
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(ids.len() as u64, count.count(), "{query}");
    count.count()
}

#[tokio::test]
async fn count_and_units_agree_on_one_snapshot() {
    let h = harness();
    small_fleet(&h);
    for query in COUNT_FILTERS {
        assert_count_and_units_agree(&h, query).await;
    }
    count_and_units_agree_on_missing_model_and_instance_type().await;
}

/// A host without a model or an instance type is the metric's `unknown` and
/// `none`: the count and the listing both match those values.
async fn count_and_units_agree_on_missing_model_and_instance_type() {
    let h = harness();
    // Hosts 0 and 2 have an instance type, 1 and 3 do not. Host 4 has an
    // instance type but no model, host 5 has neither.
    let mut hosts: Vec<Unit> = (0..6).map(|i| host(i, "Ready", "gb200", "b1")).collect();
    for unit in &mut hosts[4..] {
        unit.host.as_mut().expect("host").model = None;
    }
    h.publish_units(Family::Host, 1, hosts);
    let request = || FirmwareRequest {
        initiator: Initiator::Manual,
        requested_at: None,
        initiated_at: None,
    };
    let mut modeled = host(0, "Ready", "gb200", "b1");
    let mut unmodeled = host(1, "Ready", "gb200", "b1");
    unmodeled.host.as_mut().expect("host").model = None;
    for unit in [&mut modeled, &mut unmodeled] {
        unit.unit_type = UnitType::HostFirmwareRequest;
        unit.host.as_mut().expect("host").open_firmware_request = Some(request());
    }
    h.publish_units(Family::HostFirmwareRequests, 1, vec![modeled, unmodeled]);
    for (query, expected) in [
        ("type=host&model=unknown", 2),
        ("type=host&model=gb200", 4),
        ("type=host&instanceType=none", 3),
        ("type=host&instanceType=it-1", 3),
        ("type=host&model=unknown&instanceType=none", 1),
        ("type=host&model=unknown&instanceType=it-1", 1),
        ("type=host_firmware_request&model=unknown", 1),
        ("type=host_firmware_request&model=gb200", 1),
    ] {
        assert_eq!(
            assert_count_and_units_agree(&h, query).await,
            expected,
            "{query}"
        );
    }
}

#[tokio::test]
async fn unsupported_combination_is_422() {
    let h = harness();
    small_fleet(&h);
    // Each row: status, error code, field ("-" when the error names none)
    // and the count query.
    for row in [
        "422 unsupported_filter model type=dpu&model=gb200",
        "422 unsupported_filter stage type=dpu&stage=expected",
        "422 unsupported_filter resource type=rest_total",
        "422 unsupported_filter done type=host&done=false",
        "422 unsupported_filter source type=switch&source=rest",
        "422 field_unavailable source type=instance&source=rest",
        "422 field_unavailable bmcVersion type=host&bmcVersion=v1",
        "422 unsupported_filter recordType type=expected_record",
        "422 unsupported_filter recordType type=identity_mismatch&recordType=rack",
        "422 field_unavailable updateEligible type=host&updateEligible=true&state=Ready",
        "400 invalid_param bogus type=host&bogus=1",
        "400 invalid_param stage type=host&stage=done",
        "400 invalid_param query type=host&stage=done&state=done",
        "400 invalid_param type type=host&type=dpu",
        "400 invalid_param type stage=ready",
    ] {
        let [status, code, field, query] = row.split(' ').collect::<Vec<_>>()[..] else {
            panic!("{row}");
        };
        let reply = h.get(&format!("/v1/count?{query}")).await;
        assert_eq!(reply.status.as_str(), status, "{query}: {}", reply.body);
        assert_eq!(reply.code(), code, "{query}");
        if field != "-" {
            assert_eq!(reply.field(), field, "{query}");
        }
    }
    let reply = h.get("/v1/units?type=rest_total&resource=rack").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    let reply = h.get("/v1/units?type=host&name=x").await;
    assert_eq!((reply.status.as_u16(), reply.field()), (422, "name"));
    let reply = h.get("/v1/units?type=instance&name=(").await;
    assert_eq!((reply.status.as_u16(), reply.field()), (400, "name"));
    let reply = h.get("/v1/units?type=host&limit=1001").await;
    assert_eq!((reply.status.as_u16(), reply.field()), (400, "limit"));
    let reply = h.get("/v1/nothing").await;
    assert_eq!((reply.status.as_u16(), reply.code()), (404, "not_found"));
}

#[tokio::test]
async fn provider_instance_total_is_422_others_count() {
    let h = harness();
    h.publish(
        Family::RestTotals,
        1,
        crate::readers::rest_inventory::RestTotalsData {
            totals: [
                (RestResource::Machine, Some(10)),
                (RestResource::Instance, None),
                (RestResource::Rack, None),
                (RestResource::Tray, Some(8)),
            ]
            .into_iter()
            .collect(),
        },
    );
    let reply = h.get("/v1/count?type=rest_total&resource=instance").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        (reply.code(), reply.field()),
        ("field_unavailable", "resource")
    );
    let message = reply.body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("provider-scoped"), "{}", reply.body);
    let reply = h.get("/v1/count?type=rest_total&resource=rack").await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = reply.body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("NICo Flow"), "{}", reply.body);
    for (resource, total) in [("machine", 10), ("tray", 8)] {
        let reply = h
            .get(&format!("/v1/count?type=rest_total&resource={resource}"))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{resource}: {}", reply.body);
        assert_eq!(reply.count(), total, "{resource}");
    }
}

#[tokio::test]
async fn unread_and_deferred_fields_are_field_unavailable() {
    let h = harness();
    small_fleet(&h);
    for uri in [
        "/v1/count?type=host&allocatable=true",
        "/v1/units?type=host&allocatable=true",
        "/v1/count?type=host&updateEligible=false",
        "/v1/units?type=host&updateEligible=false",
        "/v1/units?type=host&uefiVersion=u1",
        "/v1/units?type=instance&source=rest",
    ] {
        let reply = h.get(uri).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        assert_eq!(reply.code(), "field_unavailable", "{uri}");
    }
}

#[tokio::test]
async fn no_complete_sweep_is_503() {
    let h = harness();
    for uri in ["/v1/count?type=host", "/v1/units?type=switch", "/v1/config"] {
        let reply = h.get(uri).await;
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(reply.code(), "no_complete_sweep", "{uri}");
        assert_eq!(reply.headers[header::RETRY_AFTER], "10");
    }
}

/// A device sweep of a fake site into the harness store.
async fn sweep_fake_site(h: &Harness, site: FakeSite) {
    let reader = DevicesReader::new(
        ReaderConfig {
            enabled: true,
            interval: Duration::from_secs(120),
            timeout: Duration::from_secs(600),
        },
        100,
        Arc::new(FakeConnector(FakeNico::new(site))),
        Arc::new(CallGate::new(
            "nico-api",
            1000.0,
            Duration::from_secs(5),
            Duration::from_secs(30),
        )),
        Arc::clone(&h.store) as Arc<dyn SnapshotSink>,
        SystemTime::now(),
    );
    reader
        .run(&ObservationRecorder::default())
        .await
        .expect("complete sweep");
}

fn mixed_site() -> FakeSite {
    let mut site = FakeSite::default();
    site.add_host(fake::host(1, "Ready", &[1]));
    site.add_host(fake::host(2, "HostInitializing/WaitingForDiscovery", &[2]));
    site.add_host(fake::host(3, "Failed/MachineValidation", &[3]));
    let orphan = fake::dpu_id(9);
    site.dpus.insert(orphan.to_string(), orphan);
    site.add_switch(fake::switch(1, "ready"));
    site.add_switch(fake::switch(2, "initializing"));
    site.add_shelf(fake::shelf(1, "error"));
    site.add_rack(fake::rack(1, "created"));
    site.add_instance(fake::instance(1, 1, rpc::forge::TenantState::Provisioning));
    site.expected_machines
        .push(fake::expected_machine(1, Some(1)));
    site.expected_switches
        .push(rpc::forge::LinkedExpectedSwitch {
            explored_endpoint_address: Some("10.9.9.9".to_string()),
            ..fake::expected_switch(3, None)
        });
    site.explored.insert(
        "10.9.9.9".to_string(),
        rpc::site_explorer::ExploredEndpoint {
            address: "10.9.9.9".to_string(),
            report: Some(rpc::site_explorer::EndpointExplorationReport {
                last_exploration_error: Some("Unauthorized".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    site
}

#[tokio::test]
async fn units_not_done_carry_reason() {
    let h = harness();
    sweep_fake_site(&h, mixed_site()).await;
    let mut checked = 0;
    for query in [
        "type=host",
        "type=dpu",
        "type=switch",
        "type=power_shelf",
        "type=rack",
        "type=instance",
        "type=expected_record&recordType=switch",
        "type=identity_mismatch",
    ] {
        let page = h
            .get(&format!("/v1/units?{query}&done=false"))
            .await
            .units();
        assert!(!page.units.is_empty(), "{query} has stragglers");
        for unit in page.units {
            assert!(!unit.done);
            assert!(
                unit.reason.as_deref().is_some_and(|r| !r.is_empty()),
                "{query}: {} has no reason",
                unit.id
            );
            checked += 1;
        }
    }
    assert!(checked >= 9);
    let done = h.get("/v1/units?type=host&done=true").await.units();
    assert_eq!(done.units.len(), 1);
}

#[tokio::test]
async fn manager_request_counts_as_automatic() {
    let h = harness();
    let mut site = mixed_site();
    site.firmware_requests
        .push(fake::firmware_request(1, "Automated"));
    site.firmware_requests
        .push(fake::firmware_request(2, "AdminCli"));
    sweep_fake_site(&h, site).await;
    for (initiator, count) in [("automatic", 1), ("manual", 1), ("other", 0)] {
        let query = format!("type=host_firmware_request&initiator={initiator}");
        assert_eq!(
            h.get(&format!("/v1/count?{query}")).await.count(),
            count,
            "{query}"
        );
        assert_eq!(
            h.metric_sum(
                "host_firmware_requests",
                &[("initiator", initiator.to_string())]
            ),
            count as f64,
            "{query} metric"
        );
    }
    let page = h
        .post(
            "/v1/units/query",
            json!({"type": "host", "ids": [fake::host_id(1).to_string()]}),
        )
        .await
        .units();
    let request = page.units[0]
        .host
        .as_ref()
        .and_then(|host| host.open_firmware_request.as_ref())
        .expect("open request");
    assert_eq!(
        request.initiator,
        carbide_site_health_probe_api::Initiator::Automatic
    );
}

#[tokio::test]
async fn unlinked_expected_reason_has_explorer_error() {
    let h = harness();
    sweep_fake_site(&h, mixed_site()).await;
    let page = h
        .get("/v1/units?type=expected_record&recordType=switch&linked=false")
        .await
        .units();
    assert_eq!(page.units.len(), 1);
    let unit = &page.units[0];
    assert_eq!(unit.reason.as_deref(), Some("no_device: Unauthorized"));
    assert_eq!(unit.bmc_ip.as_deref(), Some("10.9.9.9"));
    let count = h
        .get("/v1/count?type=expected_record&recordType=switch&linked=false")
        .await
        .count();
    assert_eq!(count, 1);
}

fn hosts(range: std::ops::Range<usize>) -> Vec<Unit> {
    range.map(|i| host(i, "Ready", "gb200", "b1")).collect()
}

#[tokio::test(start_paused = true)]
async fn paging_across_swap_finishes_on_kept_snapshot() {
    let h = harness();
    h.publish_units(Family::Host, 1, hosts(0..5));
    let first = h.get("/v1/units?type=host&limit=2").await.units();
    assert_eq!(first.sweep.snapshot_id, "1-devices-1");
    // Two newer sweeps land while the client pages.
    h.publish_units(Family::Host, 2, hosts(3..9));
    tokio::time::sleep(Duration::from_secs(120)).await;
    h.publish_units(Family::Host, 3, hosts(4..20));
    let mut ids: Vec<String> = first.units.into_iter().map(|u| u.id).collect();
    let mut cursor = first.next_cursor;
    while let Some(next) = cursor {
        let page = h
            .get(&format!("/v1/units?type=host&limit=2&cursor={next}"))
            .await
            .units();
        assert_eq!(
            page.sweep.snapshot_id, "1-devices-1",
            "pages stay on their snapshot"
        );
        ids.extend(page.units.into_iter().map(|u| u.id));
        cursor = page.next_cursor;
    }
    let want: Vec<String> = hosts(0..5).into_iter().map(|u| u.id).collect();
    assert_eq!(ids, want);
}

#[tokio::test(start_paused = true)]
async fn continuation_never_keeps_an_expired_snapshot() {
    let h = harness();
    h.publish_units(Family::Host, 1, hosts(0..300));
    let first = h.get("/v1/units?type=host&limit=100").await.units();
    h.publish_units(Family::Host, 2, hosts(0..300));
    assert_eq!(h.store.kept(Family::Host), 1, "the first page kept it");
    // The next page looks the snapshot up, and it expires on another
    // worker before the page hands out its own cursor.
    *h.store.after_lookup.lock().unwrap() = Some(Box::new({
        let store = Arc::downgrade(&h.store);
        move || store.upgrade().expect("store").drop_kept(Family::Host)
    }));
    let second = h
        .get(&format!(
            "/v1/units?type=host&limit=100&cursor={}",
            first.next_cursor.expect("more pages")
        ))
        .await
        .units();
    *h.store.after_lookup.lock().unwrap() = None;
    assert_eq!(second.units.len(), 100, "the looked-up page is served");
    assert_eq!(h.store.kept(Family::Host), 0, "not kept again");
    let third = h
        .get(&format!(
            "/v1/units?type=host&limit=100&cursor={}",
            second.next_cursor.expect("more pages")
        ))
        .await;
    assert_eq!(third.status, StatusCode::CONFLICT);
    assert_eq!(third.code(), "snapshot_expired");
}

#[tokio::test(start_paused = true)]
async fn paging_after_expiry_is_409() {
    let h = harness();
    h.publish_units(Family::Host, 1, hosts(0..5));
    let first = h.get("/v1/units?type=host&limit=2").await.units();
    h.publish_units(Family::Host, 2, hosts(0..5));
    tokio::time::sleep(Duration::from_secs(300)).await;
    let reply = h
        .get(&format!(
            "/v1/units?type=host&limit=2&cursor={}",
            first.next_cursor.expect("more pages")
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "snapshot_expired");
    let reply = h.get("/v1/units?type=switch&cursor=bm90IGEgY3Vyc29y").await;
    assert_eq!((reply.status.as_u16(), reply.field()), (400, "cursor"));
}

#[tokio::test]
async fn units_query_missing_ids_are_only_absent_ids() {
    let h = harness();
    let mut units = hosts(0..4);
    units[2] = host(2, "Failed", "gb200", "b1");
    let ids: Vec<String> = units.iter().map(|u| u.id.clone()).collect();
    h.publish_units(Family::Host, 1, units);
    let body = json!({
        "type": "host",
        "stage": "ready",
        // ids[2] exists but is Failed; ids[3] matches on a later page.
        "ids": [ids[0], ids[2], ids[3], "fm100hgone", ids[0]],
        "limit": 1,
    });
    let first = h.post("/v1/units/query", body.clone()).await.units();
    assert_eq!(first.units.len(), 1);
    assert_eq!(first.units[0].id, ids[0]);
    assert_eq!(first.missing_ids, vec!["fm100hgone".to_string()]);
    let mut next = body;
    next["cursor"] = json!(first.next_cursor.expect("a later page"));
    let second = h.post("/v1/units/query", next).await.units();
    assert_eq!(second.units[0].id, ids[3]);
    assert_eq!(
        second.missing_ids,
        vec!["fm100hgone".to_string()],
        "on every page"
    );
    assert_eq!(second.next_cursor, None);

    let reply = h
        .post("/v1/units/query", json!({"type": "host", "bogus": true}))
        .await;
    assert_eq!((reply.status.as_u16(), reply.field()), (400, "bogus"));
    // An unknown variant names the parameter that holds it, not the value.
    let reply = h
        .post("/v1/units/query", json!({"type": "host", "stage": "x"}))
        .await;
    assert_eq!((reply.status.as_u16(), reply.field()), (400, "stage"));
}

fn site_match(h: &Harness, seq: u64, result: SiteMatch) {
    h.publish(Family::RestSiteMatch, seq, SiteMatchData { result });
}

fn config_reply(reply: Reply) -> ConfigResponse {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    serde_json::from_value(reply.body).expect("config reply")
}

#[tokio::test]
async fn config_serves_rest_site_alone_with_each_result() {
    let h = harness();
    let reply = h.get("/v1/config").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.code(), "no_complete_sweep");

    // Only the inventory reader has read: no allocation, no nico-api config.
    for (seq, result) in [
        (1, SiteMatch::Match),
        (2, SiteMatch::Mismatch),
        (3, SiteMatch::Unknown),
    ] {
        site_match(&h, seq, result);
        let reply = h.get("/v1/config").await;
        assert_eq!(reply.body["nicoApi"], Value::Null);
        assert_eq!(reply.body["rest"], Value::Null);
        let config = config_reply(reply);
        let site = config.rest_site.expect("restSite");
        assert_eq!(site.result, result);
        assert_eq!(
            site.read.read_at,
            DateTime::<Utc>::from(UNIX_EPOCH + Duration::from_secs(1_050 + seq * 100)),
            "the site check's own read time"
        );
        assert_eq!(site.read.read_interval_seconds, 120.0);
        assert_eq!(site.read.last_attempt_error, None);
        for other in [SiteMatch::Match, SiteMatch::Mismatch, SiteMatch::Unknown] {
            assert_eq!(
                h.metric_sum("rest_site_match", &[("result", spec::wire(other))]),
                f64::from(u8::from(other == result)),
                "{other:?} after {result:?}"
            );
        }
    }
}

/// A harness whose API sees the REST inventory reader on or off.
fn rest_reader(enabled: bool) -> Harness {
    let mut h = harness();
    let mut cfg = state_config();
    cfg.readers.rest_inventory.enabled = enabled;
    h.router = router(ApiState::new(Arc::clone(&h.store), &cfg));
    h
}

#[tokio::test]
async fn config_without_rest_reader_is_200_with_nulls() {
    let h = rest_reader(false);
    for uri in [
        "/v1/config",
        "/v1/config?vendor=Other&model=x&component=bmc&instanceType=it-9",
    ] {
        let reply = h.get(uri).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}: {}", reply.body);
        // Explicit nulls: no site check at all, not an unknown one.
        assert_eq!(
            reply.body,
            json!({"nicoApi": null, "rest": null, "restSite": null}),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn config_with_rest_reader_is_503_before_the_first_site_check() {
    let h = rest_reader(true);
    let reply = h.get("/v1/config").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.code(), "no_complete_sweep");
    assert_eq!(reply.headers[header::RETRY_AFTER], "10");
    site_match(&h, 1, SiteMatch::Match);
    let config = config_reply(h.get("/v1/config").await);
    assert_eq!(config.rest_site.expect("restSite").result, SiteMatch::Match);
}

#[tokio::test]
async fn config_bad_params_are_400_with_the_rest_reader_on_or_off() {
    for enabled in [false, true] {
        let h = rest_reader(enabled);
        for (uri, field) in [
            ("/v1/config?bogus=1", "bogus"),
            ("/v1/config?component=nope", "component"),
            ("/v1/config?vendor=a&vendor=b", "vendor"),
        ] {
            let reply = h.get(uri).await;
            assert_eq!(
                (reply.status.as_u16(), reply.code(), reply.field()),
                (400, "invalid_param", field),
                "{uri} with the reader on: {enabled}"
            );
        }
    }
}

#[tokio::test]
async fn config_rest_site_keeps_last_result_after_a_failed_read() {
    let h = harness();
    site_match(&h, 1, SiteMatch::Match);
    let good = config_reply(h.get("/v1/config").await)
        .rest_site
        .expect("restSite");
    h.store.failed(
        Reader::RestInventory,
        Family::RestSiteMatch,
        crate::snapshot::FailureKind::Rpc,
        "machine page: HTTP 500",
    );
    let site = config_reply(h.get("/v1/config").await)
        .rest_site
        .expect("the last result stays");
    assert_eq!(site.result, SiteMatch::Match);
    assert_eq!(site.read.read_at, good.read.read_at, "old times");
    assert_eq!(site.read.read_start, good.read.read_start);
    assert_eq!(
        site.read.last_attempt_error.as_deref(),
        Some("machine page: HTTP 500")
    );
}

#[tokio::test]
async fn config_filters_leave_rest_site_alone() {
    let h = harness();
    site_match(&h, 1, SiteMatch::Mismatch);
    let config = config_reply(
        h.get("/v1/config?vendor=Other&model=x&component=bmc&instanceType=it-9")
            .await,
    );
    // Firmware targets and the tenant allocation wait for their tests.
    assert_eq!((config.nico_api, config.rest), (None, None));
    assert_eq!(
        config.rest_site.expect("restSite").result,
        SiteMatch::Mismatch
    );
}

#[tokio::test]
async fn rest_site_from_the_inventory_reader_without_upstream_text() {
    use crate::readers::rest_inventory::{Inventory, RestInventoryReader};

    let h = harness();
    let machines = ["fm100a", "fm100b"];
    h.publish_units(
        Family::Host,
        1,
        machines
            .iter()
            .map(|id| plain(UnitType::Host, id, "Ready", Stage::Ready))
            .collect(),
    );
    h.publish_units(Family::Dpu, 1, vec![]);
    let rest = crate::rest::fake::FakeRest::start().await;
    rest.set_pages(
        "machine",
        vec![
            machines
                .iter()
                .map(|id| json!({"controllerMachineId": id}))
                .collect(),
        ],
    );
    let reader = RestInventoryReader::new(
        ReaderConfig {
            enabled: true,
            interval: Duration::from_secs(120),
            timeout: Duration::from_secs(600),
        },
        rest.client(),
        Inventory {
            provider_auth: rest.auth(),
            provider_org: "provider-org".to_string(),
            site_id: "site-1".to_string(),
        },
        Arc::clone(&h.store) as Arc<dyn crate::snapshot::CensusView>,
        Arc::clone(&h.store) as Arc<dyn SnapshotSink>,
        SystemTime::now(),
    );
    reader
        .run(&ObservationRecorder::default())
        .await
        .expect("sweep");
    let site = config_reply(h.get("/v1/config").await)
        .rest_site
        .expect("restSite");
    assert_eq!(site.result, SiteMatch::Match);
    assert_eq!(site.read.last_attempt_error, None);

    // A reply that does not decode keeps the last result, and neither the
    // reply nor the logs carry the upstream text.
    rest.set_object("machine", json!({"error": SENTINEL}));
    let logs = CapturedLogs::default();
    let guard = logs.capture();
    assert!(reader.run(&ObservationRecorder::default()).await.is_err());
    drop(guard);
    let reply = h.get("/v1/config").await;
    assert!(!reply.body.to_string().contains(SENTINEL));
    let site = config_reply(reply).rest_site.expect("restSite");
    assert_eq!(site.result, SiteMatch::Match);
    assert_eq!(
        site.read.last_attempt_error.as_deref(),
        Some("machine page: undecodable reply")
    );
    assert!(!logs.text().is_empty(), "the failure was logged");
    assert!(!logs.text().contains(SENTINEL), "{}", logs.text());
}

/// Log lines written while the guard lives, at every level.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CapturedLogs {
    fn capture(&self) -> tracing::subscriber::DefaultGuard {
        let writer = self.clone();
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish(),
        )
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

const SENTINEL: &str = "SENTINEL-c0ffee";
