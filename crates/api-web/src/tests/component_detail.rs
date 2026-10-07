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
use carbide_uuid::rack::RackProfileId;
use hyper::http::StatusCode;
use model::test_support::power_shelf_config;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn test_component_detail_pages_return_not_found_for_missing_resources(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    let rack_id = env
        .test_harness
        .create_rack(RackProfileId::new("rack"))
        .await
        .id;
    let switch_id = env.test_harness.create_switch(1, 1).await.id;
    let power_shelf_id = env
        .test_harness
        .create_power_shelf(power_shelf_config("Component Detail Test Power Shelf"))
        .await
        .id;

    for route in [
        format!("/admin/rack/{rack_id}"),
        format!("/admin/rack/{rack_id}.json"),
        format!("/admin/switch/{switch_id}"),
        format!("/admin/switch/{switch_id}.json"),
        format!("/admin/power-shelf/{power_shelf_id}"),
        format!("/admin/power-shelf/{power_shelf_id}.json"),
    ] {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(&route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
    }

    for route in [
        "/admin/rack/missing-rack",
        "/admin/rack/missing-rack.json",
        "/admin/switch/sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
        "/admin/switch/sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0.json",
        "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
        "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0.json",
    ] {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{route}");
    }
}
