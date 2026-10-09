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

//! Isolated PostgreSQL databases for Core tests.
//!
//! `DATABASE_URL` selects the PostgreSQL connection used to create test databases.
//! With `NICO_TEST_TEMPLATE_DB` set, tests clone that already migrated database
//! without dropping it or applying migrations. Names must be nonempty and fit
//! PostgreSQL's `max_identifier_length` (normally 63 bytes). A missing template
//! is an error. Without the variable, each process prepares its own template as
//! before. `cargo make test-release-container-services` prepares a shared template
//! once before running the suite; direct `cargo test` retains the default mode.

use std::ops::Deref;
use std::str::FromStr;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::testing::{TestArgs, TestContext, TestTermination};
use sqlx::{ConnectOptions, Connection, Executor, Postgres};
use tokio::sync::OnceCell;

static POOL: OnceCell<PgPool> = OnceCell::const_new();
static DB_NUMBER: AtomicUsize = AtomicUsize::new(0);
static TEMPLATE_DB: &str = "sqlx_test_template_db";
static PREPARED_TEMPLATE_DB: LazyLock<Option<String>> =
    LazyLock::new(|| match std::env::var("NICO_TEST_TEMPLATE_DB") {
        Ok(name) => {
            assert!(!name.is_empty(), "NICO_TEST_TEMPLATE_DB must not be empty");
            Some(name)
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("cannot read NICO_TEST_TEMPLATE_DB: {error}"),
    });

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn drop_database_query(db_name: &str) -> String {
    format!("drop database if exists {}", quote_identifier(db_name))
}

fn create_database_query(db_name: &str) -> String {
    format!("create database {}", quote_identifier(db_name))
}

fn create_database_from_template_query(db_name: &str, template_db: &str) -> String {
    format!(
        "create database {} template {}",
        quote_identifier(db_name),
        quote_identifier(template_db)
    )
}

async fn validate_template_name(root_pool: &PgPool, template_db: &str) -> Result<(), sqlx::Error> {
    if template_db.is_empty() {
        return Err(sqlx::Error::Protocol(
            "NICO_TEST_TEMPLATE_DB must not be empty".to_string(),
        ));
    }
    let max_length: i32 =
        sqlx::query_scalar("SELECT current_setting('max_identifier_length')::integer")
            .fetch_one(root_pool)
            .await?;
    if template_db.len() > max_length as usize {
        return Err(sqlx::Error::Protocol(format!(
            "NICO_TEST_TEMPLATE_DB must not exceed {max_length} bytes"
        )));
    }
    Ok(())
}

pub trait TestFn {
    type Output;

    fn run_test(self, args: TestArgs) -> Self::Output;
}

impl<Fut> TestFn for fn(PgPool) -> Fut
where
    Fut: Future,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        run_test_with_pool(args, self)
    }
}

impl<Fut> TestFn for fn(PgPoolOptions, PgConnectOptions) -> Fut
where
    Fut: Future,
    Fut::Output: TestTermination,
{
    type Output = Fut::Output;

    fn run_test(self, args: TestArgs) -> Self::Output {
        run_test(args, self)
    }
}

pub fn run_test_with_pool<F, Fut>(args: TestArgs, test_fn: F) -> Fut::Output
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future,
    Fut::Output: TestTermination,
{
    let test_path = args.test_path;
    run_test(args, |pool_opts, connect_opts| async move {
        let pool = pool_opts
            .connect_with(connect_opts)
            .await
            .expect("failed to connect test pool");

        let res = test_fn(pool.clone()).await;

        let close_timed_out = sqlx_core::rt::timeout(Duration::from_secs(10), pool.close())
            .await
            .is_err();

        if close_timed_out {
            eprintln!("test {test_path} held onto Pool after exiting");
        }

        res
    })
}

fn run_test<F, Fut>(args: TestArgs, test_fn: F) -> Fut::Output
where
    F: FnOnce(PgPoolOptions, PgConnectOptions) -> Fut,
    Fut: Future,
    Fut::Output: TestTermination,
{
    sqlx_core::rt::test_block_on(async move {
        let test_context = test_context(&args)
            .await
            .expect("failed to connect to setup test database");

        setup_test_db(&test_context.connect_opts, &args).await;

        let res = test_fn(test_context.pool_opts, test_context.connect_opts).await;
        if res.is_success()
            && let Err(e) = cleanup_test(&test_context.db_name).await
        {
            eprintln!(
                "failed to cleanup database {:?}: {}",
                test_context.db_name, e
            );
        }
        res
    })
}

async fn cleanup_test(db_name: &str) -> Result<(), sqlx::Error> {
    let query = drop_database_query(db_name);
    POOL.get()
        .unwrap()
        .acquire()
        .await?
        .execute(sqlx::AssertSqlSafe(query))
        .await
        .map(|_| ())
}

async fn setup_test_db(copts: &PgConnectOptions, args: &TestArgs) {
    let mut conn = copts
        .connect()
        .await
        .expect("failed to connect to test database");

    for fixture in args.fixtures {
        (&mut conn)
            .execute(fixture.contents)
            .await
            .unwrap_or_else(|e| panic!("failed to apply test fixture {:?}: {:?}", fixture.path, e));
    }

    conn.close()
        .await
        .expect("failed to close setup connection");
}

async fn init_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set.");
    let opts = PgConnectOptions::from_str(&url).expect("failed to parse DATABASE_URL");
    let root_pool = PoolOptions::new()
        .max_connections(50)
        .after_release(|_conn, _| Box::pin(async move { Ok(false) }))
        .connect_lazy_with(opts);

    if let Some(template_db) = PREPARED_TEMPLATE_DB.as_deref() {
        validate_template_name(&root_pool, template_db)
            .await
            .expect("invalid prepared test template database name");
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(template_db)
                .fetch_one(&root_pool)
                .await
                .expect("cannot check prepared test template database");
        assert!(
            exists,
            "NICO_TEST_TEMPLATE_DB specifies missing database {template_db:?}; prepare it before running tests"
        );
    } else {
        prepare_template_database(&root_pool, TEMPLATE_DB)
            .await
            .expect("cannot prepare test template database");
    }
    root_pool
}

/// Replace a test template database and apply the current Core migrations.
///
/// Call this once before launching tests with `NICO_TEST_TEMPLATE_DB` set to
/// `template_db`. The template's connections are closed before this returns.
/// The caller must own the template exclusively while preparing it.
/// The name must be nonempty and fit the server's `max_identifier_length`
/// (normally 63 bytes). Failed migrations drop the new template; cleanup failures
/// are reported to stderr while preserving the original migration error.
pub async fn prepare_template_database(
    root_pool: &PgPool,
    template_db: &str,
) -> Result<(), sqlx::migrate::MigrateError> {
    validate_template_name(root_pool, template_db).await?;
    let drop_template_query = drop_database_query(template_db);
    root_pool
        .execute(sqlx::AssertSqlSafe(drop_template_query))
        .await?;

    let create_template_query = create_database_query(template_db);
    root_pool
        .execute(sqlx::AssertSqlSafe(create_template_query))
        .await?;
    let root_opts: std::sync::Arc<PgConnectOptions> = root_pool.connect_options();
    let template_opts = root_opts.deref().clone().database(template_db);
    let template_pool = PoolOptions::new().connect_lazy_with(template_opts);
    let result = db::migrations::migrate(&template_pool).await;
    template_pool.close().await;
    if result.is_err()
        && let Err(error) = root_pool
            .execute(sqlx::AssertSqlSafe(drop_database_query(template_db)))
            .await
    {
        eprintln!(
            "failed to cleanup template database {template_db:?} after migration failure: {error}"
        );
    }
    result
}

async fn test_context(args: &TestArgs) -> Result<TestContext<Postgres>, sqlx::Error> {
    let pool = POOL.get_or_init(init_pool).await;

    let new_db_name = format!(
        "db{}_{}",
        DB_NUMBER.fetch_add(1, Ordering::SeqCst),
        args.test_path.replace(":", "_"),
    );

    let drop_test_query = drop_database_query(&new_db_name);
    pool.acquire()
        .await?
        .execute(sqlx::AssertSqlSafe(drop_test_query))
        .await?;

    let template_db = PREPARED_TEMPLATE_DB.as_deref().unwrap_or(TEMPLATE_DB);
    let create_test_query = create_database_from_template_query(&new_db_name, template_db);
    pool.acquire()
        .await?
        .execute(sqlx::AssertSqlSafe(create_test_query))
        .await?;

    Ok(TestContext {
        pool_opts: PoolOptions::new()
            .max_connections(5)
            .idle_timeout(Some(Duration::from_secs(1)))
            .parent(pool.clone()),
        connect_opts: pool
            .connect_options()
            .deref()
            .clone()
            .database(&new_db_name),
        db_name: new_db_name,
    })
}
