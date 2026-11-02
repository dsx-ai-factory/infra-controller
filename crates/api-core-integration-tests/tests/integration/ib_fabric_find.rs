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

use ::rpc::forge as rpc;
use carbide_api_core::cfg::file::CarbideConfig;
use carbide_api_core::test_support::default_config;
use carbide_ib_fabric::config::IBFabricConfig;
use carbide_test_harness::prelude::*;

#[sqlx_test]
async fn test_find_ib_fabric_ids_disabled(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let ids_all = env
        .api()
        .find_ib_fabric_ids(tonic::Request::new(rpc::IbFabricSearchFilter::default()))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.ib_fabric_ids, Vec::<String>::new());
}

#[sqlx_test]
async fn test_find_ib_fabric_ids_enabled(pool: PgPool) {
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_runtime_config(
                CarbideConfig {
                    ib_config: Some(IBFabricConfig {
                        enabled: true,
                        ..Default::default()
                    }),
                    ..default_config::get()
                }
                .into(),
            )
        })
        .build()
        .await;

    let ids_all = env
        .api()
        .find_ib_fabric_ids(tonic::Request::new(rpc::IbFabricSearchFilter::default()))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.ib_fabric_ids, vec!["default".to_string()]);
}
