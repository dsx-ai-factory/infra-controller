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
use std::sync::Arc;
use std::sync::atomic::Ordering;

use carbide_api_core::cfg::file::CarbideConfig;
use carbide_api_core::cfg::load::parse_carbide_config;
use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ConfigSetting, SetDynamicConfigRequest};

trait TestHarnessConfigExt {
    fn config(&self) -> &CarbideConfig;
}

impl TestHarnessConfigExt for TestHarness {
    fn config(&self) -> &CarbideConfig {
        self.api().runtime_config.as_ref()
    }
}

async fn init(db_pool: PgPool, config: CarbideConfig) -> TestHarness {
    let config = Arc::new(config);
    TestHarness::builder(db_pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(config))
        .build()
        .await
}

async fn create_env_with_tracing_config(
    db_pool: PgPool,
    enabled: bool,
    allow_runtime_changes: bool,
) -> TestHarness {
    let mut config = default_config::get();
    config.tracing.enabled = enabled;
    config.tracing.allow_runtime_changes = allow_runtime_changes;

    init(db_pool, config).await
}

async fn set_tracing_enabled(
    env: &TestHarness,
    enabled: bool,
) -> Result<tonic::Response<()>, tonic::Status> {
    env.api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::TracingEnabled as i32,
            value: enabled.to_string(),
            expiry: None,
        }))
        .await
}

#[sqlx_test]
async fn test_bmc_proxy_setting_config_allowed(db_pool: PgPool) -> Result<(), eyre::Report> {
    let env = {
        let mut config = default_config::get();
        config.site_explorer.allow_changing_bmc_proxy = Some(true);
        init(db_pool, config).await
    };

    assert!(matches!(
        env.config().site_explorer.allow_changing_bmc_proxy.as_ref(),
        Some(true)
    ));
    assert!(env.config().site_explorer.bmc_proxy.load().is_none());

    env.api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::BmcProxy as i32,
            value: "test-host:1234".to_string(),
            expiry: None,
        }))
        .await?;
    assert_eq!(
        env.config()
            .site_explorer
            .bmc_proxy
            .load()
            .clone()
            .as_ref()
            .clone()
            .expect("bmc_proxy should have gotten set")
            .to_string(),
        "test-host:1234"
    );

    Ok(())
}

#[sqlx_test]
async fn test_tracing_dynamic_config_runtime_changes_allowed(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = create_env_with_tracing_config(db_pool, false, true).await;
    assert!(!env.config().tracing.enabled);
    assert!(env.config().tracing.allow_runtime_changes);
    assert!(
        !env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    set_tracing_enabled(&env, true).await?;
    assert!(
        env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    set_tracing_enabled(&env, false).await?;
    assert!(
        !env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    Ok(())
}

#[sqlx_test]
async fn test_tracing_config_enabled_can_be_disabled_when_runtime_changes_allowed(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = create_env_with_tracing_config(db_pool, true, true).await;
    assert!(env.config().tracing.enabled);
    assert!(env.config().tracing.allow_runtime_changes);
    assert!(
        env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    set_tracing_enabled(&env, false).await?;
    assert!(
        !env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    Ok(())
}

#[sqlx_test]
async fn test_tracing_dynamic_config_rejected_when_runtime_changes_disabled(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = create_env_with_tracing_config(db_pool, true, false).await;
    assert!(env.config().tracing.enabled);
    assert!(!env.config().tracing.allow_runtime_changes);
    assert!(
        env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    let err = set_tracing_enabled(&env, false)
        .await
        .expect_err("runtime tracing change should be rejected");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    Ok(())
}

#[sqlx_test]
async fn test_tracing_dynamic_config_rejected_from_disabled_startup_config(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = create_env_with_tracing_config(db_pool, false, false).await;
    assert!(!env.config().tracing.enabled);
    assert!(!env.config().tracing.allow_runtime_changes);
    assert!(
        !env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    let err = set_tracing_enabled(&env, true)
        .await
        .expect_err("runtime tracing change should be rejected");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        !env.api()
            .dynamic_settings
            .tracing_enabled
            .load(Ordering::Relaxed)
    );

    Ok(())
}

#[sqlx_test]
async fn test_bmc_proxy_setting_config_unspecified(db_pool: PgPool) -> Result<(), eyre::Report> {
    let env = {
        let mut config = default_config::get();
        // Leave allow_changing_bmc_proxy unspecified, it should behave as if false
        config.site_explorer.allow_changing_bmc_proxy = None;
        init(db_pool, config).await
    };

    assert!(
        env.config()
            .site_explorer
            .allow_changing_bmc_proxy
            .is_none()
    );
    assert!(env.config().site_explorer.bmc_proxy.load().is_none());

    match env
        .api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::BmcProxy as i32,
            value: "test-host:1234".to_string(),
            expiry: None,
        }))
        .await
    {
        Err(e) => {
            assert_eq!(e.code(), tonic::Code::PermissionDenied);
        }
        _ => panic!("Setting dynamic config should have failed with a permission_denied"),
    };

    assert!(env.config().site_explorer.bmc_proxy.load().is_none());

    Ok(())
}

#[sqlx_test]
async fn test_bmc_proxy_setting_config_not_allowed(db_pool: PgPool) -> Result<(), eyre::Report> {
    let env = {
        let mut config = default_config::get();
        config.site_explorer.allow_changing_bmc_proxy = Some(false);
        init(db_pool, config).await
    };

    assert!(matches!(
        env.config().site_explorer.allow_changing_bmc_proxy.as_ref(),
        Some(false)
    ));

    assert!(env.config().site_explorer.bmc_proxy.load().is_none());

    match env
        .api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::BmcProxy as i32,
            value: "test-host:1234".to_string(),
            expiry: None,
        }))
        .await
    {
        Err(e) => {
            assert_eq!(e.code(), tonic::Code::PermissionDenied);
        }
        _ => panic!("Setting dynamic config should have failed with a permission_denied"),
    };

    assert!(env.config().site_explorer.bmc_proxy.load().is_none());

    Ok(())
}

#[sqlx_test]
async fn test_bmc_proxy_setting_parsed_config_unspecified(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = {
        // Create a config with allow_changing_bmc_proxy unset, then pass it to parse_carbide_config,
        // then use *that* config, and assert that it defaults to false
        let mut config = default_config::get();
        // Leave allow_changing_bmc_proxy unspecified, it should behave as if false
        config.site_explorer.allow_changing_bmc_proxy = None;
        config.site_explorer.bmc_proxy = carbide_site_explorer::config::bmc_proxy(None);
        config.site_explorer.override_target_ip = None;
        config.site_explorer.override_target_port = None;
        let config_str = toml::to_string(&config)?;
        let mut tmp = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut tmp, config_str.as_bytes())?;
        let parsed_config = parse_carbide_config(tmp.path(), None)?;
        init(db_pool, parsed_config.as_ref().to_owned()).await
    };

    assert!(
        env.config()
            .site_explorer
            .allow_changing_bmc_proxy
            .is_none()
    );
    assert!(env.api().dynamic_settings.bmc_proxy.load().is_none());

    match env
        .api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::BmcProxy as i32,
            value: "test-host:1234".to_string(),
            expiry: None,
        }))
        .await
    {
        Err(e) => {
            assert_eq!(e.code(), tonic::Code::PermissionDenied);
        }
        _ => panic!("Setting dynamic config should have failed with a permission_denied"),
    };

    assert!(env.api().dynamic_settings.bmc_proxy.load().is_none());

    Ok(())
}

#[sqlx_test]
async fn test_bmc_proxy_setting_parsed_config_unspecified_with_bmc_proxy_set(
    db_pool: PgPool,
) -> Result<(), eyre::Report> {
    let env = {
        // Create a config with allow_changing_bmc_proxy unset, but with bmc_proxy set. This should
        // make allow_changing_bmc_proxy to default to true in parse_carbide_config.
        let mut config = default_config::get();
        // Leave allow_changing_bmc_proxy unspecified, it should behave as if false
        config.site_explorer.allow_changing_bmc_proxy = None;
        config.site_explorer.bmc_proxy =
            carbide_site_explorer::config::bmc_proxy(Some("test:1234".parse().unwrap()));
        let config_str = toml::to_string(&config)?;
        let mut tmp = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut tmp, config_str.as_bytes())?;
        let parsed_config = parse_carbide_config(tmp.path(), None)?;
        init(db_pool, parsed_config.as_ref().to_owned()).await
    };

    assert!(matches!(
        env.config().site_explorer.allow_changing_bmc_proxy.as_ref(),
        Some(true),
    ));

    assert_eq!(
        env.api()
            .dynamic_settings
            .bmc_proxy
            .load()
            .clone()
            .as_ref()
            .clone(),
        Some("test:1234".parse().unwrap())
    );

    env.api()
        .set_dynamic_config(tonic::Request::new(SetDynamicConfigRequest {
            setting: ConfigSetting::BmcProxy as i32,
            value: "other-host:5678".to_string(),
            expiry: None,
        }))
        .await
        .unwrap();

    assert_eq!(
        env.api()
            .dynamic_settings
            .bmc_proxy
            .load()
            .clone()
            .as_ref()
            .clone(),
        Some("other-host:5678".parse().unwrap())
    );

    Ok(())
}
