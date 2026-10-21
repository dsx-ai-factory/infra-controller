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
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use model::machine::{MachineValidationContext, MachineValidationFilter};
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn validation_run_pages_display_optional_end_time(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let host = env.create_ready_managed_host(1).await.0;
    let machine_id = host.host.id;
    let mut txn = env.test_harness.db_txn().await;
    let run = db::machine_validation::create_new_run(
        txn.as_mut(),
        &machine_id.into(),
        MachineValidationContext::OnDemand,
        MachineValidationFilter::default(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    for (scenario, end_time, expected) in [
        ("unfinished", None, "N/A"),
        (
            "completed",
            Some("2026-10-09T13:00:00Z".parse::<DateTime<Utc>>().unwrap()),
            "2026-10-09T13:00:00Z",
        ),
        (
            "explicit epoch",
            Some(DateTime::UNIX_EPOCH),
            "1970-01-01T00:00:00Z",
        ),
    ] {
        let mut txn = env.test_harness.db_txn().await;
        sqlx::query("UPDATE machine_validation SET end_time = $1, state = $2 WHERE id = $3")
            .bind(end_time)
            .bind(if end_time.is_some() {
                "Success"
            } else {
                "Started"
            })
            .bind(run.id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        txn.commit().await.unwrap();

        for route in [
            "/admin/machinevalidation".to_string(),
            format!("/admin/machine/{machine_id}"),
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
            assert_eq!(response.status(), StatusCode::OK, "{scenario}: {route}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let html = std::str::from_utf8(&body).unwrap();
            let row = html
                .split("<tr>")
                .find(|row| row.contains(&run.id.to_string()))
                .expect("validation run row")
                .split("</tr>")
                .next()
                .unwrap();
            assert!(
                row.contains(&format!("<td>{expected}</td>")),
                "{scenario}: {route}: {row}"
            );
        }
    }
}
