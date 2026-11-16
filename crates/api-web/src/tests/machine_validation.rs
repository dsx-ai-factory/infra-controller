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

#[crate::sqlx_test]
async fn validation_run_pages_show_progress_current_work_and_logs(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let machine_id = env.create_ready_managed_host(1).await.0.host.id;
    let mut txn = env.test_harness.db_txn().await;
    let run = db::machine_validation::create_new_run(
        txn.as_mut(),
        &machine_id.into(),
        MachineValidationContext::OnDemand,
        MachineValidationFilter::default(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE machine_validation SET state = 'InProgress', total = 3, completed = 1 WHERE id = $1")
        .bind(run.id).execute(txn.as_mut()).await.unwrap();
    let mut attempt_ids = Vec::new();
    let result_attempt_id = uuid::Uuid::new_v4();
    let result_start: DateTime<Utc> = "2026-10-09T13:00:00Z".parse().unwrap();
    let result_end: DateTime<Utc> = "2026-10-09T13:01:00Z".parse().unwrap();
    for (index, state) in [(0, "Running"), (1, "Running"), (2, "Pending")] {
        let item_id = uuid::Uuid::new_v4();
        let attempt_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO machine_validation_run_items (id, run_id, test_id, display_name, context, state, order_index, attempt, max_attempts) VALUES ($1, $2, $3, $6, 'OnDemand', $4, $5, 2, 3)")
            .bind(item_id).bind(run.id).bind(format!("live_test_{index}"))
            .bind(state).bind(index).bind(format!("Live test {index}"))
            .execute(txn.as_mut()).await.unwrap();
        sqlx::query("INSERT INTO machine_validation_attempts (id, run_item_id, attempt_number, state) VALUES ($1, $2, 2, $3)")
            .bind(attempt_id).bind(item_id).bind(state).execute(txn.as_mut()).await.unwrap();
        if index != 1 {
            // The matching result predates attempt 2; test 2 has mismatched times.
            let first_attempt_id = if index == 0 {
                result_attempt_id
            } else {
                uuid::Uuid::new_v4()
            };
            sqlx::query("INSERT INTO machine_validation_attempts (id, run_item_id, attempt_number, state, exit_code, started_at, ended_at) VALUES ($1, $2, 1, 'Failed', 1, $3, $4)")
                .bind(first_attempt_id).bind(item_id).bind(result_start)
                .bind(if index == 0 { result_end } else { result_start })
                .execute(txn.as_mut()).await.unwrap();
        }
        db::machine_validation_result::create(
            model::machine_validation::MachineValidationResult {
                validation_id: run.id,
                name: format!("live_test_{index}"),
                description: String::new(),
                stdout: String::new(),
                stderr: "Initial attempt failed".to_string(),
                command: "validation-test".to_string(),
                args: String::new(),
                context: "OnDemand".to_string(),
                exit_code: 1,
                start_time: if index == 2 {
                    result_start + chrono::Duration::microseconds(100)
                } else {
                    result_start
                },
                end_time: result_end,
                test_id: Some(format!("live_test_{index}")),
            },
            txn.as_mut(),
        )
        .await
        .unwrap();
        attempt_ids.push(attempt_id);
    }
    let pending_item_id = uuid::Uuid::new_v4();
    let pending_attempt_id = uuid::Uuid::new_v4();
    let skipped_item_id = uuid::Uuid::new_v4();
    let skipped_attempt_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO machine_validation_run_items (id, run_id, test_id, display_name, context, state, order_index, attempt, max_attempts) VALUES ($1, $2, 'live_pending', 'Planned test', 'OnDemand', 'Pending', 3, 0, 1), ($3, $2, 'live_skipped', 'Skipped test', 'OnDemand', 'Skipped', 4, 1, 1)")
        .bind(pending_item_id).bind(run.id).bind(skipped_item_id)
        .execute(txn.as_mut()).await.unwrap();
    sqlx::query("INSERT INTO machine_validation_attempts (id, run_item_id, attempt_number, state, started_at, ended_at, exit_code) VALUES ($1, $2, 1, 'Pending', NULL, NULL, NULL), ($3, $4, 1, 'Skipped', $5, $6, 0)")
        .bind(pending_attempt_id).bind(pending_item_id).bind(skipped_attempt_id).bind(skipped_item_id)
        .bind(result_start).bind(result_end).execute(txn.as_mut()).await.unwrap();
    db::machine_validation_result::create(
        model::machine_validation::MachineValidationResult {
            validation_id: run.id,
            name: "Skipped test".to_string(),
            description: String::new(),
            stdout: "Skipped: precondition not met".to_string(),
            stderr: String::new(),
            command: "validation-test".to_string(),
            args: String::new(),
            context: "OnDemand".to_string(),
            exit_code: 0,
            start_time: result_start,
            end_time: result_end,
            test_id: Some("live_skipped".to_string()),
        },
        txn.as_mut(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE machine_validation SET total = 5, completed = 1 WHERE id = $1")
        .bind(run.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    sqlx::query("INSERT INTO machine_validation_attempt_logs (attempt_id, sequence, stream, content) SELECT $1, sequence, CASE WHEN sequence = 1 THEN 'stderr' ELSE 'stdout' END, '<script>untrusted output</script>' FROM generate_series(1, 101) AS sequence")
        .bind(attempt_ids[0]).execute(txn.as_mut()).await.unwrap();
    txn.commit().await.unwrap();

    for completed in [false, true] {
        if completed {
            let mut txn = env.test_harness.db_txn().await;
            // A finished run must not present stale Running items as current work.
            sqlx::query("UPDATE machine_validation SET end_time = NOW(), state = 'Success', completed = 5 WHERE id = $1")
                .bind(run.id).execute(txn.as_mut()).await.unwrap();
            txn.commit().await.unwrap();
        }
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(format!("/admin/machinevalidation/runs/{}", run.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains(if completed { "5/5" } else { "1/5" }));
        assert!(!html.contains("Click current attempt to expand relevant logs"));
        assert!(!html.contains("Click View logs to expand the relevant attempt logs"));
        assert!(html.contains("<tr><th>Context</th><td>OnDemand</td></tr>"));
        assert!(html.contains("Basic authentication alone does not grant log access."));
        assert!(html.contains(&format!(
            "<th>Machine ID</th><td><a href=\"/admin/machine/{machine_id}\">{machine_id}</a></td>"
        )));
        assert!(!html.contains("<h2>Current Work</h2>"));
        let results_table = html
            .split("<h2>Tests</h2>")
            .nth(1)
            .unwrap()
            .split("<aside")
            .next()
            .unwrap();
        assert_eq!(
            results_table.matches("In Progress").count(),
            if completed { 0 } else { 2 }
        );
        for (index, attempt_id) in attempt_ids.iter().take(2).enumerate() {
            assert_eq!(
                results_table.contains(&format!("href=\"#attempt-log-{attempt_id}\"")),
                !completed
            );
            assert!(results_table.contains(&format!(
                "href=\"/admin/machinevalidation/tests/live_test_{index}\">live_test_{index}</a>"
            )));
        }
        assert!(results_table.contains("Complete (1)"));
        assert!(results_table.contains("Complete (0)"));
        assert!(!html.contains(&pending_attempt_id.to_string()));
        assert!(!html.contains("<details"));
        assert_eq!(html.matches("<pre hidden><code></code></pre>").count(), 1);
        assert!(html.contains("Select View logs to display one attempt's stdout/stderr here."));
        assert!(results_table.contains("<th>Test Name/ID</th>"));
        assert!(results_table.contains("<th>Attempt</th>"));
        assert!(results_table.contains(&format!("href=\"#attempt-log-{skipped_attempt_id}\"")));
        assert!(!results_table.contains("<th>Context</th>"));
        assert!(!results_table.contains("<th>End Time</th>"));
        assert!(results_table.contains("<th>Started (UTC)</th>"));
        assert!(results_table.contains("<th>Duration</th>"));
        assert!(results_table.contains("data-sort=\"60000\""));
        for offset in [0, 100] {
            assert!(results_table.contains(&format!(
                "data-sort=\"{}\"",
                result_start.timestamp_micros() + offset
            )));
        }
        assert!(results_table.contains("1m 00s"));
        assert!(results_table.contains("title=\"2026-10-09T13:00:00Z\">13:00:00</time>"));
        assert!(results_table.contains("title=\"Ended 2026-10-09T13:01:00Z\""));
        for (index, attempt_id) in attempt_ids.iter().take(3).enumerate() {
            let row = results_table
                .split("<tr>")
                .find(|row| {
                    row.contains(&format!("live_test_{index}")) && !row.contains("In Progress")
                })
                .unwrap()
                .split("</tr>")
                .next()
                .unwrap();
            if index == 0 {
                assert!(row.contains(&format!("href=\"#attempt-log-{result_attempt_id}\"")));
                assert!(row.contains("Attempt 1 ·"));
                assert!(row.contains("View logs"));
            } else {
                assert!(row.contains("Unavailable"));
                assert!(!row.contains("#attempt-log-"));
            }
            assert!(!row.contains(&attempt_id.to_string()));
        }
        assert_eq!(
            html.matches(&format!("href=\"#attempt-log-{result_attempt_id}\""))
                .count(),
            1
        );
        assert!(!html.contains("<script>untrusted output</script>"));

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
            assert_eq!(response.status(), StatusCode::OK, "{route}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.contains("<th>Completed/Total</th>"));
            let row = html
                .split("<tr>")
                .find(|row| row.contains(&run.id.to_string()))
                .unwrap()
                .split("</tr>")
                .next()
                .unwrap();
            assert!(row.contains("<td>OnDemand</td>"), "{route}: {row}");
            assert!(
                row.contains(if completed {
                    "Completed(Success)"
                } else {
                    "InProgress(InProgress)"
                }),
                "{route}: {row}"
            );
            assert!(
                row.contains(if completed {
                    "<td>5/5</td>"
                } else {
                    "<td>1/5</td>"
                }),
                "{route}: {row}"
            );
            if route == "/admin/machinevalidation" {
                assert!(row.contains(&format!("href=\"/admin/machine/{machine_id}\"")));
                assert!(row.contains("class=\"machine_id\""));
            } else {
                assert!(html.contains("<th>Current Work</th>"));
                for (index, attempt_id) in attempt_ids.iter().take(2).enumerate() {
                    assert_eq!(
                        row.contains(&format!("live_test_{index}")),
                        !completed,
                        "{route}: {row}"
                    );
                    assert_eq!(
                        row.contains(&format!("2 ({attempt_id})")),
                        !completed,
                        "{route}: {row}"
                    );
                }
                assert!(!row.contains("live_test_2"));
            }
        }
    }

    for (end_time, sort_key, display) in [
        (result_start, "0", "0ms"),
        (result_start - chrono::Duration::milliseconds(1), "", "N/A"),
    ] {
        let mut txn = env.test_harness.db_txn().await;
        sqlx::query("UPDATE machine_validation_results SET end_time = $1 WHERE machine_validation_id = $2 AND test_id = 'live_test_0'")
            .bind(end_time).bind(run.id).execute(txn.as_mut()).await.unwrap();
        txn.commit().await.unwrap();
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(format!("/admin/machinevalidation/runs/{}", run.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&body).unwrap();
        let row = html
            .split("<tr>")
            .find(|row| row.contains("live_test_0"))
            .unwrap()
            .split("</tr>")
            .next()
            .unwrap();
        assert!(row.contains(&format!(
            "<td data-sort=\"{sort_key}\" title=\"Ended {}\">{display}</td>",
            rpc::Timestamp::from(end_time)
        )));
        assert!(html.contains("validation-results n-last"));
    }

    let logs_url = format!("/admin/machinevalidation/attempts/{}/logs", attempt_ids[0]);
    for (context, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (
            Some(carbide_api_core::AuthContext::default()),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = web_request_builder()
            .uri(&logs_url)
            .body(Body::empty())
            .unwrap();
        if let Some(context) = context {
            request.extensions_mut().insert(context);
        }
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
    }
    let admin = carbide_api_core::AuthContext {
        principals: vec![carbide_authn::middleware::Principal::from_web_cookie(
            "test-admin".into(),
            "admin".into(),
        )],
        authorization: None,
    };
    for (cursor, count, has_more) in [(0, 100, true), (100, 1, false), (101, 0, false)] {
        let mut request = web_request_builder()
            .uri(format!("{logs_url}?after_sequence={cursor}"))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(admin.clone());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let chunks = page["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), count);
        assert_eq!(page["has_more"], has_more);
        if count > 0 {
            assert_eq!(chunks[0]["sequence"], cursor + 1);
            assert_eq!(
                chunks[0]["stream"],
                if cursor == 0 { "stderr" } else { "stdout" }
            );
            assert_eq!(chunks[0]["content"], "<script>untrusted output</script>");
        }
    }
}
