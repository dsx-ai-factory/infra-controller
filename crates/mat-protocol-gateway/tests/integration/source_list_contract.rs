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

//! The `GET /v1/sources` contract, checked against the controller's own fixture.
//!
//! `dev/k8s/machine-a-tron-controller/pkg/sourcelist/testdata/sources_v1.json` is written by the
//! Go handler test `TestHandler_SourcesGolden` and included here, so there is one copy: a shape
//! change on either side fails the other side's tests. The unit tests in `src/sources.rs` parse
//! the same file; this file checks the wire path (HTTP client, headers, serialisation) and keeps
//! the fakes used by the other wire tests honest.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::routing::get;
use mat_protocol_gateway::{SourceClientConfig, SourceList, SourceListClient};
use serde_json::Value;
use url::Url;

use crate::common::FakeController;

const FIXTURE: &str = include_str!(
    "../../../../dev/k8s/machine-a-tron-controller/pkg/sourcelist/testdata/sources_v1.json"
);

fn fixture_value() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn gateway_types_carry_exactly_the_fixture_fields() {
    let list = SourceList::parse(FIXTURE.as_bytes()).unwrap();
    let fixture = fixture_value();

    // Serialising back names the same fields, top level and per source: nothing dropped, nothing
    // invented. Values are compared field by field because `Url` normalises `base_url` with a
    // trailing slash on output; the gateway never publishes a list, so the shape is the contract.
    let serialized = serde_json::to_value(&list).unwrap();
    assert_eq!(keys(&serialized), keys(&fixture));
    for (serialized, fixture) in serialized["sources"]
        .as_array()
        .unwrap()
        .iter()
        .zip(fixture["sources"].as_array().unwrap())
    {
        assert_eq!(keys(serialized), keys(fixture));
    }

    assert_eq!(serialized["generation"], fixture["generation"]);
    assert_eq!(serialized["ready"], fixture["ready"]);
    let fixture_sources = fixture["sources"].as_array().unwrap();
    assert_eq!(list.sources.len(), fixture_sources.len());
    for (source, fixture) in list.sources.iter().zip(fixture_sources) {
        assert_eq!(source.name, fixture["name"]);
        assert_eq!(source.pod, fixture["pod"]);
        assert_eq!(
            source.base_url,
            Url::parse(fixture["base_url"].as_str().unwrap()).unwrap()
        );
    }
}

#[tokio::test]
async fn client_fetches_the_fixture_as_the_controller_serves_it() {
    // The Go handler's response: the fixture bytes with its headers.
    let address = crate::common::serve(Router::new().route(
        "/v1/sources",
        get(|| async {
            (
                [
                    (CONTENT_TYPE, "application/json"),
                    (CACHE_CONTROL, "no-store"),
                ],
                FIXTURE,
            )
        }),
    ))
    .await;
    let client = SourceListClient::new(
        format!("http://{address}/v1/sources").parse().unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();

    let list = client.fetch().await.unwrap();

    list.validate().unwrap();
    assert_eq!(list, SourceList::parse(FIXTURE.as_bytes()).unwrap());
    assert_eq!(list.generation, 3);
    let inventory = list.inventory_sources(&SourceClientConfig::default());
    assert_eq!(
        inventory
            .iter()
            .map(|source| source.url.as_str())
            .collect::<Vec<_>>(),
        vec![
            "https://nico-machine-a-tron-mat-0-bmc-mock.nico-system.svc.cluster.local:8443/machines/status",
            "https://nico-machine-a-tron-mat-1-bmc-mock.nico-system.svc.cluster.local:8443/machines/status",
            "https://nico-machine-a-tron-single-bmc-mock.nico-system.svc.cluster.local:1266/machines/status",
        ]
    );
}

/// The fake controller the lifecycle tests drive must publish the same JSON shape as the real
/// one, or those tests would pass against a contract the gateway never sees in a pod.
#[tokio::test]
async fn fake_controller_publishes_the_fixture_shape() {
    let controller = FakeController::new(
        vec![("mat-a".to_string(), "127.0.0.1:1".parse().unwrap())],
        3,
    );
    let address = controller.serve().await;

    let body: Value = reqwest::get(format!("http://{address}/v1/sources"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let fixture = fixture_value();

    assert_eq!(keys(&body), keys(&fixture));
    assert_eq!(keys(&body["sources"][0]), keys(&fixture["sources"][0]));
    assert!(body["sources"].is_array());
    assert!(body["ready"].is_boolean());
    assert!(body["generation"].is_u64());
    // And the gateway accepts it like the fixture.
    SourceList::parse(&serde_json::to_vec(&body).unwrap())
        .unwrap()
        .validate()
        .unwrap();
}
