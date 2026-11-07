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
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

/// Collects a web response body into a UTF-8 string.
async fn response_body(response: Response) -> String {
    // Collect the full response body before converting it to text.
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("empty response body")
        .to_bytes();

    String::from_utf8(body_bytes.to_vec()).expect("invalid UTF-8 in response body")
}

#[crate::sqlx_test]
async fn vpc_pages_show_status_vni(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    // Create a VPC with an auto-allocated VNI stored in status.
    let network_controller = env.test_harness.network_controller();
    let vpc_id = network_controller.create_vpc("test vpc 1").await;
    network_controller
        .create_tenant_segment(env.domain(), vpc_id)
        .await;
    let mut vpcs = env
        .api()
        .find_vpcs_by_ids(tonic::Request::new(forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await
        .unwrap()
        .into_inner()
        .vpcs;
    let vpc = vpcs.pop().expect("expected fixture VPC");
    let vni = vpc
        .status
        .as_ref()
        .and_then(|status| status.vni)
        .expect("expected status VNI")
        .to_string();

    // Add a VPC prefix so the IPAM prefix detail page can render parent VPC data.
    let vpc_prefix = env
        .api()
        .create_vpc_prefix(tonic::Request::new(forge::VpcPrefixCreationRequest {
            id: None,
            prefix: String::new(),
            vpc_id: Some(vpc_id),
            site_prefix_id: None,
            config: Some(forge::VpcPrefixConfig {
                prefix: "192.0.2.0/25".to_string(),
            }),
            metadata: Some(forge::Metadata {
                name: "Test VPC prefix".to_string(),
                description: String::new(),
                labels: Vec::new(),
            }),
        }))
        .await
        .unwrap()
        .into_inner();
    let vpc_prefix_id = vpc_prefix.id.expect("expected VPC prefix ID");

    // Verify the VPC overview renders status.vni.
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri("/admin/vpc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_body(response)
            .await
            .contains(&format!("<td>{vni}</td>"))
    );

    // Verify the VPC detail page renders status.vni.
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/vpc/{vpc_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_body(response)
            .await
            .contains(&format!("<tr><th>VNI</th><td>{vni}</td></tr>"))
    );

    // Verify the IPAM overlay overview renders status.vni.
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri("/admin/ipam/overlay")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_body(response)
            .await
            .contains(&format!("<td>{vni}</td>"))
    );

    // Verify the IPAM overlay prefix detail page renders status.vni.
    let response = app
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/ipam/overlay/prefix/{vpc_prefix_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response_body(response)
            .await
            .contains(&format!("<tr><td>VNI</td><td>{vni}</td></tr>"))
    );
}
