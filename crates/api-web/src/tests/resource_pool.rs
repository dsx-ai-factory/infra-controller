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

use axum::body::Body;
use axum::response::Response;
use carbide_test_harness::TestHarness;
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain, ValueType,
};
use tower::ServiceExt;

use crate::tests::{make_test_app, web_request_builder};

/// Collect the public response so the test checks the same HTML and JSON an
/// operator receives through the router.
async fn response_body(response: Response) -> String {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Find one pool's cells so a value in another pool cannot satisfy its checks.
fn pool_cells<'a>(body: &'a str, name: &str) -> Vec<&'a str> {
    let row = body
        .split("<tr>")
        .find(|row| row.contains(&format!("<td>{name}</td>")))
        .expect("pool is present in the table");
    row.split("<td>")
        .skip(1)
        .map(|cell| cell.split("</td>").next().unwrap())
        .collect()
}

/// The page keeps empty and unavailable pools distinct. Existing JSON clients
/// retain their numeric fields and receive an error for unsupported backends.
#[crate::sqlx_test]
async fn resource_pool_pages_show_counts_and_unavailable_backends(pool: sqlx::PgPool) {
    let harness = TestHarness::builder(pool.clone()).build().await;
    let mut txn = pool.begin().await.unwrap();

    // Allocate one of two fixture VNIs to exercise a populated percentage.
    db::resource_pool::allocate(
        &ResourcePool::<u32>::new("vpc-vni".to_string(), ValueType::Integer),
        &mut txn,
        OwnerType::Vpc,
        "web-inspection-vpc",
        None,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Existing JSON consumers retain the flat numeric fields for local pools.
    let app = make_test_app(&harness);
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri("/admin/resource-pool.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let pools: Vec<serde_json::Value> =
        serde_json::from_str(&response_body(response).await).unwrap();
    assert!(
        pools
            .windows(2)
            .all(|pair| pair[0]["name"].as_str() < pair[1]["name"].as_str())
    );
    let vpc = pools.iter().find(|pool| pool["name"] == "vpc-vni").unwrap();
    assert_eq!(
        vpc,
        &serde_json::json!({
            "name": "vpc-vni",
            "min": "20001",
            "max": "60001",
            "total": 2,
            "allocated": 1,
        })
    );

    let mut txn = pool.begin().await.unwrap();
    for selection in [
        // An empty Integrated pool has a known size of zero.
        ResourcePoolSelection {
            pool_name: "empty-pool".to_string(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: None,
            backend_kind: ResourcePoolBackendKind::Integrated,
            authority_id: "local-db".to_string(),
            remote_pool: "empty-pool".to_string(),
        },
        // A remote binding without an accepted definition has no observed counts.
        ResourcePoolSelection {
            pool_name: "remote-pool".to_string(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("dc-network".to_string()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "dc-network-allocator".to_string(),
            remote_pool: "tenant-vnis".to_string(),
        },
    ] {
        db::resource_pool::binding::bind_or_verify(&mut txn, &selection)
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri("/admin/resource-pool")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body(response).await;
    let header = body
        .split("<thead>")
        .nth(1)
        .unwrap()
        .split("</thead>")
        .next()
        .unwrap();
    let columns: Vec<_> = header
        .split("<th>")
        .skip(1)
        .map(|column| column.split("</th>").next().unwrap())
        .collect();
    assert_eq!(
        columns,
        [
            "Name",
            "Backend",
            "Remote Pool",
            "Min",
            "Max",
            "Size",
            "Used",
            "Status"
        ]
    );
    for (name, expected, status) in [
        (
            "vpc-vni",
            [
                "vpc-vni",
                "Integrated",
                "vpc-vni",
                "20001",
                "60001",
                "2",
                "1 (50%)",
            ],
            "Known",
        ),
        (
            "empty-pool",
            ["empty-pool", "Integrated", "empty-pool", "—", "—", "0", "0"],
            "Known",
        ),
        (
            "remote-pool",
            [
                "remote-pool",
                "gRPC (dc-network)",
                "tenant-vnis",
                "—",
                "—",
                "Unavailable",
                "Unavailable",
            ],
            "Unavailable",
        ),
    ] {
        let cells = pool_cells(&body, name);
        assert_eq!(&cells[..7], &expected, "{name}");
        assert!(cells[7].starts_with(status), "{name}: {}", cells[7]);
    }
    assert!(!body.contains("NaN"));

    // The numeric JSON schema rejects remote pools instead of inventing counts.
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/resource-pool.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        response_body(response).await,
        "remote resource pools require AdminInspectResourcePools"
    );
}
