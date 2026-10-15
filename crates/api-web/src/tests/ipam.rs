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
use carbide_test_harness::TestHarness;
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use tower::ServiceExt;

use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn dual_stack_pages_show_all_prefixes_and_count_segments(pool: sqlx::PgPool) {
    struct SegmentCase {
        route: &'static str,
        segment_type: forge::NetworkSegmentType,
        prefixes: [&'static str; 2],
    }

    struct PageCase {
        uri: String,
        expected: Vec<&'static str>,
    }

    let harness = TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_site_fabric_prefixes(vec![
                "2001:db8:2::/64".parse().unwrap(),
                "198.51.100.0/24".parse().unwrap(),
            ])
        })
        .build()
        .await;
    let domain = harness.test_domain().await;
    harness
        .network_controller()
        .create_underlay_segment(&domain)
        .await;
    let app = make_test_app(&harness);
    let mut pages = Vec::new();
    for case in [
        SegmentCase {
            route: "underlay",
            segment_type: forge::NetworkSegmentType::Underlay,
            prefixes: ["2001:db8:1::/64", "198.18.0.0/24"],
        },
        SegmentCase {
            route: "overlay",
            segment_type: forge::NetworkSegmentType::Tenant,
            prefixes: ["2001:db8:2::/64", "198.51.100.0/24"],
        },
    ] {
        let segment = harness
            .api()
            .create_network_segment(tonic::Request::new(forge::NetworkSegmentCreationRequest {
                name: format!("dual-stack-{}", case.route),
                segment_type: case.segment_type as i32,
                prefixes: case
                    .prefixes
                    .iter()
                    .map(|prefix| forge::NetworkPrefix {
                        prefix: (*prefix).to_string(),
                        reserve_first: 1,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }))
            .await
            .expect("create dual-stack segment")
            .into_inner();
        let segment_id = segment.id.expect("created segment has an ID");
        pages.push(PageCase {
            uri: format!("/admin/ipam/{}/segment/{segment_id}", case.route),
            expected: vec!["<td>Prefixes</td>", case.prefixes[0], case.prefixes[1]],
        });
    }

    // Count the single-stack underlay once and the dual-stack underlay once.
    pages.push(PageCase {
        uri: "/admin/ipam/underlay".to_string(),
        expected: vec![
            "<p>2 segment(s). Each prefix is shown in a separate row.</p>",
            ">192.0.1.0/24</a>",
            ">2001:db8:1::/64</a>",
            ">198.18.0.0/24</a>",
        ],
    });

    for case in pages {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(&case.uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{}", case.uri);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("read response body")
            .to_bytes();
        let body = String::from_utf8(body.to_vec()).expect("response is UTF-8");
        for expected in case.expected {
            assert!(
                body.contains(expected),
                "{} is missing {expected}",
                case.uri
            );
        }
    }
}
