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

use std::fs;

use carbide_api_core::bootstrap::{Logging, start_runtime_prelude};
use carbide_api_core::cfg::load::parse_carbide_config;
use db::host_naming::HostNamingStrategyKind;
use tempfile::tempdir;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn validated_settings_are_installed_only_at_runtime_start() {
    let directory = tempdir().expect("create temporary config directory");
    let config_path = directory.path().join("carbide-api.toml");
    fs::write(
        &config_path,
        r#"
        database_url = "postgres://test"
        listen = "[::]:1081"
        asn = 65000
        sitename = "test-site"
        web_ui_logs_link_template = "https://logs.example.com/{search}"
        host_naming_strategy = "fun"

        [[web_ui_sidebar_tools]]
        name = "grafana"
        display_name = "Grafana"
        url = "https://grafana.example.com"
        "#,
    )
    .expect("write config");

    let config = parse_carbide_config(&config_path, None).expect("parse validated config");

    assert!(carbide_api_core::configured_tools().is_empty());
    assert_eq!(carbide_api_core::configured_site_name(), None);
    assert_eq!(carbide_api_core::configured_logs_link_template(), "");
    assert_eq!(
        db::host_naming::configured(),
        HostNamingStrategyKind::IpAddress
    );

    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let _runtime_prelude =
        start_runtime_prelude(&config, Logging::default(), &mut join_set, &cancel_token);

    let tools = carbide_api_core::configured_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "grafana");
    assert_eq!(tools[0].display_name, "Grafana");
    assert_eq!(tools[0].url, "https://grafana.example.com");
    assert_eq!(carbide_api_core::configured_site_name(), Some("test-site"));
    assert_eq!(
        carbide_api_core::configured_logs_link_template(),
        "https://logs.example.com/{search}"
    );
    assert_eq!(db::host_naming::configured(), HostNamingStrategyKind::Fun);

    cancel_token.cancel();
    join_set.shutdown().await;
}
