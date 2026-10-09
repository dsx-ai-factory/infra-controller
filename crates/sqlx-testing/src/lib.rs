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

use std::ops::Deref;
use std::str::FromStr;
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

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn drop_database_query(db_name: &str) -> String {
    format!("drop database if exists {}", quote_identifier(db_name))
}

fn create_database_query(db_name: &str) -> String {
    format!("create database {}", quote_identifier(db_name))
}

fn create_database_from_template_query(db_name: &str) -> String {
    format!(
        "create database {} template {}",
        quote_identifier(db_name),
        quote_identifier(TEMPLATE_DB)
    )
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

    let drop_template_query = drop_database_query(TEMPLATE_DB);
    root_pool
        .execute(sqlx::AssertSqlSafe(drop_template_query))
        .await
        .expect("cannot cleanup template database");

    let create_template_query = create_database_query(TEMPLATE_DB);
    root_pool
        .execute(sqlx::AssertSqlSafe(create_template_query))
        .await
        .expect("cannot create template database");
    let root_opts: std::sync::Arc<PgConnectOptions> = root_pool.connect_options();
    let template_opts = root_opts.deref().clone().database(TEMPLATE_DB);
    let template_pool = PoolOptions::new().connect_lazy_with(template_opts);
    db::migrations::migrate(&template_pool)
        .await
        .expect("cannot migrate DB used as template");
    template_pool.close().await;
    root_pool
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

    let create_test_query = create_database_from_template_query(&new_db_name);
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
