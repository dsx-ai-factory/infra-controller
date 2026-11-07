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
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn overview_shows_host_inband_with_formatted_state(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let segment = env
        .test_harness
        .network_controller()
        .create_host_inband_segment(env.domain())
        .await;
    let app = make_test_app(&env.test_harness);

    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/network-segment")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("empty response body")
        .to_bytes();
    let body = String::from_utf8(body.to_vec()).expect("invalid UTF-8 in response body");
    let host_inband = body
        .split_once("<h2>Host Inband</h2>")
        .expect("missing Host Inband section")
        .1
        .split_once("<h2>Underlay</h2>")
        .expect("missing Underlay section after Host Inband")
        .0;

    assert!(host_inband.contains(&format!(
        "href=\"/admin/network-segment/{}\">HOST_INBAND</a>",
        segment.id
    )));
    assert!(host_inband.contains("<span class=\"bubble success\">Ready</span>"));
    assert!(!host_inband.contains(r#"{&quot;state&quot;:&quot;ready&quot;}"#));
}
