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
use sqlx::types::Json;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

async fn response_body(response: Response) -> String {
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("empty response body")
        .to_bytes();

    String::from_utf8(body_bytes.to_vec()).expect("invalid UTF-8 in response body")
}

async fn insert_desired_firmware(
    pool: &sqlx::PgPool,
    vendor: &str,
    model: &str,
    explicit_update_start_needed: bool,
) {
    sqlx::query(
        r#"
            INSERT INTO desired_firmware (
                vendor,
                model,
                versions,
                explicit_update_start_needed
            )
            VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(vendor)
    .bind(model)
    .bind(Json(serde_json::json!({
        "Versions": {
            "bmc": "1.2.3",
            "uefi": "4.5.6"
        }
    })))
    .bind(explicit_update_start_needed)
    .execute(pool)
    .await
    .expect("insert desired firmware row");
}

#[crate::sqlx_test]
async fn firmware_page_shows_desired_firmware_table(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool.clone()).await;
    insert_desired_firmware(&pool, "Acme", "RackServer 9000", true).await;
    let app = make_test_app(&env.test_harness);

    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/firmware")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body(response).await;
    assert!(body.contains("Desired Firmware (1)"));
    assert!(body.contains("Acme"));
    assert!(body.contains("RackServer 9000"));
    assert!(body.contains("<td>true</td>"));
    assert!(body.contains("<b>BMC</b>: 1.2.3"));
    assert!(body.contains("<b>UEFI</b>: 4.5.6"));
}

#[crate::sqlx_test]
async fn firmware_json_returns_complete_records_in_vendor_model_order(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool.clone()).await;
    let expected_records = [
        ("Acme", "RackServer 1000", false),
        ("Acme", "RackServer 9000", true),
        ("Zeta", "RackServer 1000", false),
    ];
    // Reverse insertion distinguishes vendor/model ordering from insertion order.
    for &(vendor, model, explicit_update_start_needed) in expected_records.iter().rev() {
        insert_desired_firmware(&pool, vendor, model, explicit_update_start_needed).await;
    }
    let app = make_test_app(&env.test_harness);

    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/firmware.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&response_body(response).await).expect("valid JSON response");
    let expected = expected_records.map(|(vendor, model, explicit_update_start_needed)| {
        serde_json::json!({
            "vendor": vendor,
            "model": model,
            "versions": {
                "Versions": {
                    "bmc": "1.2.3",
                    "uefi": "4.5.6"
                }
            },
            "explicit_update_start_needed": explicit_update_start_needed,
        })
    });
    assert_eq!(rows, expected);
}
