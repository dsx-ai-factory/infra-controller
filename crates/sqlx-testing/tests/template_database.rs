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

use std::process::{Command, Output};

use sqlx::postgres::PgPoolOptions;
use sqlx::testing::TestArgs;

fn probe(template: Option<&str>) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "template_probe", "--ignored", "--nocapture"]);
    command.env_remove("NICO_TEST_TEMPLATE_DB");
    command.env_remove("NICO_TEST_EXPECT_PREPARED");
    if let Some(template) = template {
        command.env("NICO_TEST_TEMPLATE_DB", template);
        command.env("NICO_TEST_EXPECT_PREPARED", "true");
    }
    command.output().unwrap()
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("failed to cleanup database"),
        "{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn template_database_modes() {
    let template = format!("test template \"{}", std::process::id());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let root_pool = runtime.block_on(async {
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap()
    });

    assert_success(
        Command::new(env!("CARGO_BIN_EXE_prepare-test-template"))
            .env("NICO_TEST_TEMPLATE_DB", &template)
            .output()
            .unwrap(),
    );
    runtime.block_on(async {
        let opts = root_pool
            .connect_options()
            .as_ref()
            .clone()
            .database(&template);
        let pool = PgPoolOptions::new().connect_with(opts).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE template_marker (id integer); INSERT INTO template_marker VALUES (1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    });

    // Separate processes must reuse the same template without replacing it.
    assert_success(probe(Some(&template)));
    assert_success(probe(Some(&template)));
    assert_success(probe(None));

    for (name, message) in [
        (
            format!("missing_template_{}", std::process::id()),
            "specifies missing database",
        ),
        (String::new(), "NICO_TEST_TEMPLATE_DB must not be empty"),
    ] {
        let output = probe(Some(&name));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        let exists: bool = runtime
            .block_on(
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                    .bind(&name)
                    .fetch_one(&root_pool),
            )
            .unwrap();
        assert!(!exists, "invalid prepared templates must not be created");
    }

    runtime.block_on(async {
        let max_length: i32 =
            sqlx::query_scalar("SELECT current_setting('max_identifier_length')::integer")
                .fetch_one(&root_pool)
                .await
                .unwrap();
        let prefix = format!("template_name_limit_{}_", std::process::id());
        let prefix = format!("{prefix}{}", "x".repeat(max_length as usize - prefix.len()));
        sqlx_testing::prepare_template_database(&root_pool, &prefix)
            .await
            .unwrap();
        let oid: i64 = sqlx::query_scalar("SELECT oid::bigint FROM pg_database WHERE datname = $1")
            .bind(&prefix)
            .fetch_one(&root_pool)
            .await
            .unwrap();
        for name in [
            format!("{prefix}x"),
            format!("{}я", &prefix[..prefix.len() - 1]),
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_prepare-test-template"))
                .env("NICO_TEST_TEMPLATE_DB", &name)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("must not exceed"));
            let output = probe(Some(&name));
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("must not exceed"));
            let retained_oid: i64 =
                sqlx::query_scalar("SELECT oid::bigint FROM pg_database WHERE datname = $1")
                    .bind(&prefix)
                    .fetch_one(&root_pool)
                    .await
                    .unwrap();
            assert_eq!(retained_oid, oid, "validation must precede DROP DATABASE");
        }
        let drop = format!("DROP DATABASE \"{prefix}\"");
        sqlx::raw_sql(sqlx::AssertSqlSafe(drop))
            .execute(&root_pool)
            .await
            .unwrap();

        // Only new connections used by migrations are read-only; the existing
        // administrative connection can still create and clean up the database.
        let failing_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let options = failing_pool
            .connect_options()
            .as_ref()
            .clone()
            .options([("default_transaction_read_only", "on")]);
        failing_pool.set_connect_options(options);
        let failed_template = format!("failed_template_{}", std::process::id());
        let error = sqlx_testing::prepare_template_database(&failing_pool, &failed_template)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&failed_template)
                .fetch_one(&root_pool)
                .await
                .unwrap();
        assert!(!exists, "failed migrations must remove the template");
        failing_pool.close().await;

        let query = format!("DROP DATABASE \"{}\"", template.replace('"', "\"\""));
        sqlx::raw_sql(sqlx::AssertSqlSafe(query))
            .execute(&root_pool)
            .await
            .unwrap();
        root_pool.close().await;
    });
}

#[test]
#[ignore = "subprocess probe invoked by template_database_modes"]
fn template_probe() {
    let prepared = std::env::var("NICO_TEST_EXPECT_PREPARED").is_ok();
    for test_path in ["template_probe_first", "template_probe_second"] {
        sqlx_testing::run_test_with_pool(
            TestArgs {
                test_path,
                migrator: None,
                fixtures: &[],
            },
            |pool| async move {
                let migrations: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                assert!(migrations > 0);
                if prepared {
                    let markers: i64 = sqlx::query_scalar("SELECT count(*) FROM template_marker")
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                    assert_eq!(markers, 1, "each test must get an untouched clone");
                    sqlx::query("INSERT INTO template_marker VALUES (2)")
                        .execute(&pool)
                        .await
                        .unwrap();
                } else {
                    let marker: Option<String> =
                        sqlx::query_scalar("SELECT to_regclass('template_marker')::text")
                            .fetch_one(&pool)
                            .await
                            .unwrap();
                    assert_eq!(marker, None);
                    sqlx::query("CREATE TABLE template_marker (id integer)")
                        .execute(&pool)
                        .await
                        .unwrap();
                }
            },
        );
    }
}
