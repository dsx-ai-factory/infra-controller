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

use std::time::Duration;

use carbide_api_core::test_support::dep_log_filter;
use carbide_test_harness::prelude::*;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::EnvFilter;

#[sqlx_test]
async fn test_dynamic_log_filter(db_pool: PgPool) -> eyre::Result<()> {
    let env = TestHarness::builder(db_pool).build().await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    // Real env does this in api/lib.rs::run
    env.api().start_dynamic_settings_reset_task(
        &mut join_set,
        Duration::from_millis(300),
        cancel_token.clone(),
    );

    // 1. It's correct when we start
    // This is actually set in TestEnv so not especially useful check, but we need it later
    let local = EnvFilter::builder()
        .parse(std::env::var("RUST_LOG").unwrap_or("trace".to_string()))
        .unwrap()
        .to_string();
    let base = env.api().log_filter_string();
    assert_eq!(local, base, "Startup log filter does not match RUST_LOG");

    // 2. Apply default dep log levels (as in fn setup_logging(...))
    let local = dep_log_filter(local.clone().into());

    // 3. set_log_filter changes it correctly
    let req = rpc::forge::SetDynamicConfigRequest {
        setting: rpc::forge::ConfigSetting::LogFilter.into(),
        value: "debug".to_string(),
        expiry: Some("500ms".to_string()),
    };
    env.api()
        .set_dynamic_config(tonic::Request::new(req))
        .await?;
    let current = env.api().log_filter_string();
    // it should be something like: "...,debug until 2024-03-27 18:20:33.723829221 UTC"
    assert!(
        current.contains("debug until "),
        "set_log_filter did not update log to expected"
    );

    // 4. After 'expiry' it automatically reverts
    tokio::time::sleep(Duration::from_secs(1)).await;
    let base = env.api().log_filter_string();
    assert_eq!(
        local.to_string(),
        base,
        "Expiry task did not revert log filter"
    );

    cancel_token.cancel();
    join_set.join_all().await;

    Ok(())
}
